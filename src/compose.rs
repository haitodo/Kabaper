use std::collections::HashMap;
use std::path::Path;

use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Imaging::*;
use windows::Win32::System::Com::*;
use windows::core::*;

use crate::decode::DecodedImage;
use crate::layout::{ImageId, LayoutPlan, RectPx};

#[derive(Debug, Clone)]
pub struct ComposedBitmap {
    pub width: u32,
    pub height: u32,
    pub pixels: Vec<u8>, // 32-bit BGRA, A = 255
}

impl ComposedBitmap {
    pub fn new(width: u32, height: u32) -> Self {
        let size = (width as usize) * (height as usize) * 4;
        let mut pixels = vec![0u8; size];
        // Initialize with opaque black (B=0, G=0, R=0, A=255)
        for chunk in pixels.as_chunks_mut::<4>().0 {
            chunk[3] = 255;
        }
        Self {
            width,
            height,
            pixels,
        }
    }

    #[inline]
    pub fn set_pixel(&mut self, x: u32, y: u32, b: u8, g: u8, r: u8) {
        if x < self.width && y < self.height {
            let offset = ((y as usize) * (self.width as usize) + (x as usize)) * 4;
            self.pixels[offset] = b;
            self.pixels[offset + 1] = g;
            self.pixels[offset + 2] = r;
            self.pixels[offset + 3] = 255;
        }
    }

    #[inline]
    pub fn get_pixel(&self, x: u32, y: u32) -> (u8, u8, u8, u8) {
        if x < self.width && y < self.height {
            let offset = ((y as usize) * (self.width as usize) + (x as usize)) * 4;
            (
                self.pixels[offset],
                self.pixels[offset + 1],
                self.pixels[offset + 2],
                self.pixels[offset + 3],
            )
        } else {
            (0, 0, 0, 255)
        }
    }
}

pub fn compose_layout(
    plan: &LayoutPlan,
    images: &HashMap<ImageId, DecodedImage>,
) -> ComposedBitmap {
    compose_layout_with_dim(plan, images, 0.0)
}

pub fn compose_layout_with_dim(
    plan: &LayoutPlan,
    images: &HashMap<ImageId, DecodedImage>,
    dim_ratio: f64,
) -> ComposedBitmap {
    let (mw, mh) = plan.monitor_px;
    let mut target = ComposedBitmap::new(mw, mh);

    for frame in &plan.frames {
        let slot = frame.slot;
        if let Some(placed) = &frame.content
            && let Some(img) = images.get(&placed.id)
        {
            // Check if margin exists for background blur
            let has_margin = placed.blur_sigma > 0.0;
            if has_margin {
                render_background_blur(&mut target, slot, img, placed.blur_sigma, dim_ratio);
            }

            // Render foreground image
            render_foreground(&mut target, slot, img, placed);
        }
    }

    target
}

fn render_foreground(
    target: &mut ComposedBitmap,
    slot: RectPx,
    img: &DecodedImage,
    placed: &crate::layout::PlacedImage,
) {
    let fg_dest = placed.fg_dest;
    let fg_src = placed.fg_src;

    if fg_dest.width == 0 || fg_dest.height == 0 || img.width == 0 || img.height == 0 {
        return;
    }

    // Ratio between orig_size and decoded buffer size
    let scale_x_buf = (img.width as f64) / (img.orig_size.0 as f64);
    let scale_y_buf = (img.height as f64) / (img.orig_size.1 as f64);

    let src_x_start = fg_src.x * scale_x_buf;
    let src_y_start = fg_src.y * scale_y_buf;
    let src_w = fg_src.width * scale_x_buf;
    let src_h = fg_src.height * scale_y_buf;

    // Dest bounds clipped to slot
    let min_x = (fg_dest.x.max(slot.x) as u32).min(target.width);
    let max_x = ((fg_dest.x + fg_dest.width as i32).min(slot.x + slot.width as i32) as u32)
        .min(target.width);
    let min_y = (fg_dest.y.max(slot.y) as u32).min(target.height);
    let max_y = ((fg_dest.y + fg_dest.height as i32).min(slot.y + slot.height as i32) as u32)
        .min(target.height);

    if min_x >= max_x || min_y >= max_y {
        return;
    }

    let dw_f = fg_dest.width as f64;
    let dh_f = fg_dest.height as f64;

    for y in min_y..max_y {
        let v = ((y as i32 - fg_dest.y) as f64 + 0.5) / dh_f;
        let sy = src_y_start + v * src_h;

        let row_offset = (y as usize) * (target.width as usize) * 4;

        for x in min_x..max_x {
            let u = ((x as i32 - fg_dest.x) as f64 + 0.5) / dw_f;
            let sx = src_x_start + u * src_w;

            let (b, g, r) = sample_bicubic(img, sx, sy);
            let p_off = row_offset + (x as usize) * 4;
            target.pixels[p_off] = b;
            target.pixels[p_off + 1] = g;
            target.pixels[p_off + 2] = r;
            target.pixels[p_off + 3] = 255;
        }
    }
}

fn render_background_blur(
    target: &mut ComposedBitmap,
    slot: RectPx,
    img: &DecodedImage,
    sigma: f64,
    dim_ratio: f64,
) {
    if slot.width == 0 || slot.height == 0 || img.width == 0 || img.height == 0 {
        return;
    }

    // Spec 3.2: Downscaled blur is permitted as long as effective sigma matches
    // Downscale slot by factor of 4 for speed & broad blur
    let scale_factor = 4u32;
    let small_w = slot.width.div_ceil(scale_factor).max(1);
    let small_h = slot.height.div_ceil(scale_factor).max(1);
    let small_sigma = (sigma / scale_factor as f64).max(1.0);

    // 1. Resample img centered cover into small buffer
    let (iw, ih) = (img.width as f64, img.height as f64);
    let (sw, sh) = (small_w as f64, small_h as f64);
    let cover = (sw / iw).max(sh / ih);
    let bg_w = sw / cover;
    let bg_h = sh / cover;
    let bg_x = (iw - bg_w) / 2.0;
    let bg_y = (ih - bg_h) / 2.0;

    let mut small_buf = vec![0u8; (small_w as usize) * (small_h as usize) * 3];

    for sy in 0..small_h {
        let v = (sy as f64 + 0.5) / sh;
        let img_y = bg_y + v * bg_h;
        for sx in 0..small_w {
            let u = (sx as f64 + 0.5) / sw;
            let img_x = bg_x + u * bg_w;
            let (b, g, r) = sample_bilinear(img, img_x, img_y);
            let idx = ((sy as usize) * (small_w as usize) + (sx as usize)) * 3;
            small_buf[idx] = b;
            small_buf[idx + 1] = g;
            small_buf[idx + 2] = r;
        }
    }

    // 2. Gaussian blur on small buffer (separable horizontal + vertical) with edge clamp
    let blurred = gaussian_blur_rgb(&small_buf, small_w, small_h, small_sigma);

    // 3. Upscale blurred buffer into slot on target
    let min_x = (slot.x.max(0) as u32).min(target.width);
    let max_x = ((slot.x + slot.width as i32).max(0) as u32).min(target.width);
    let min_y = (slot.y.max(0) as u32).min(target.height);
    let max_y = ((slot.y + slot.height as i32).max(0) as u32).min(target.height);

    let sw_f = slot.width as f64;
    let sh_f = slot.height as f64;
    let dim_factor = (1.0 - dim_ratio).clamp(0.0, 1.0);

    for y in min_y..max_y {
        let v = ((y as i32 - slot.y) as f64 + 0.5) / sh_f;
        let by = v * (small_h as f64);
        let row_offset = (y as usize) * (target.width as usize) * 4;

        for x in min_x..max_x {
            let u = ((x as i32 - slot.x) as f64 + 0.5) / sw_f;
            let bx = u * (small_w as f64);

            let (b, g, r) = sample_small_bilinear(&blurred, small_w, small_h, bx, by);
            let p_off = row_offset + (x as usize) * 4;
            if dim_ratio > 0.0 {
                target.pixels[p_off] = (b as f64 * dim_factor).round() as u8;
                target.pixels[p_off + 1] = (g as f64 * dim_factor).round() as u8;
                target.pixels[p_off + 2] = (r as f64 * dim_factor).round() as u8;
            } else {
                target.pixels[p_off] = b;
                target.pixels[p_off + 1] = g;
                target.pixels[p_off + 2] = r;
            }
            target.pixels[p_off + 3] = 255;
        }
    }
}

fn gaussian_blur_rgb(src: &[u8], width: u32, height: u32, sigma: f64) -> Vec<u8> {
    let radius = (sigma * 3.0).ceil() as i32;
    if radius <= 0 {
        return src.to_vec();
    }

    // Generate 1D Gaussian kernel
    let mut kernel = Vec::with_capacity((radius * 2 + 1) as usize);
    let two_sigma_sq = 2.0 * sigma * sigma;
    let mut sum = 0.0;
    for k in -radius..=radius {
        let val = (-(k * k) as f64 / two_sigma_sq).exp();
        kernel.push(val);
        sum += val;
    }
    for val in &mut kernel {
        *val /= sum;
    }

    let w = width as i32;
    let h = height as i32;

    // Horizontal pass
    let mut tmp = vec![0f32; (width as usize) * (height as usize) * 3];
    for y in 0..h {
        let row_in = (y as usize) * (width as usize) * 3;
        let row_out = (y as usize) * (width as usize) * 3;
        for x in 0..w {
            let mut acc_b = 0.0f32;
            let mut acc_g = 0.0f32;
            let mut acc_r = 0.0f32;

            for (k_idx, k) in (-radius..=radius).enumerate() {
                // Edge clamping (sample端クランプ)
                let sx = (x + k).clamp(0, w - 1) as usize;
                let src_idx = row_in + sx * 3;
                let weight = kernel[k_idx] as f32;
                acc_b += src[src_idx] as f32 * weight;
                acc_g += src[src_idx + 1] as f32 * weight;
                acc_r += src[src_idx + 2] as f32 * weight;
            }

            let out_idx = row_out + (x as usize) * 3;
            tmp[out_idx] = acc_b;
            tmp[out_idx + 1] = acc_g;
            tmp[out_idx + 2] = acc_r;
        }
    }

    // Vertical pass
    let mut out = vec![0u8; (width as usize) * (height as usize) * 3];
    for y in 0..h {
        let row_out = (y as usize) * (width as usize) * 3;
        for x in 0..w {
            let mut acc_b = 0.0f32;
            let mut acc_g = 0.0f32;
            let mut acc_r = 0.0f32;

            for (k_idx, k) in (-radius..=radius).enumerate() {
                // Edge clamping
                let sy = (y + k).clamp(0, h - 1) as usize;
                let src_idx = sy * (width as usize) * 3 + (x as usize) * 3;
                let weight = kernel[k_idx] as f32;
                acc_b += tmp[src_idx] * weight;
                acc_g += tmp[src_idx + 1] * weight;
                acc_r += tmp[src_idx + 2] * weight;
            }

            let out_idx = row_out + (x as usize) * 3;
            out[out_idx] = acc_b.round().clamp(0.0, 255.0) as u8;
            out[out_idx + 1] = acc_g.round().clamp(0.0, 255.0) as u8;
            out[out_idx + 2] = acc_r.round().clamp(0.0, 255.0) as u8;
        }
    }

    out
}

#[inline]
fn sample_small_bilinear(src: &[u8], w: u32, h: u32, x: f64, y: f64) -> (u8, u8, u8) {
    let px = (x - 0.5).clamp(0.0, (w - 1) as f64);
    let py = (y - 0.5).clamp(0.0, (h - 1) as f64);

    let x0 = px.floor() as usize;
    let y0 = py.floor() as usize;
    let x1 = (x0 + 1).min(w as usize - 1);
    let y1 = (y0 + 1).min(h as usize - 1);

    let fx = (px - x0 as f64) as f32;
    let fy = (py - y0 as f64) as f32;

    let w00 = (1.0 - fx) * (1.0 - fy);
    let w10 = fx * (1.0 - fy);
    let w01 = (1.0 - fx) * fy;
    let w11 = fx * fy;

    let idx00 = (y0 * (w as usize) + x0) * 3;
    let idx10 = (y0 * (w as usize) + x1) * 3;
    let idx01 = (y1 * (w as usize) + x0) * 3;
    let idx11 = (y1 * (w as usize) + x1) * 3;

    let b = src[idx00] as f32 * w00
        + src[idx10] as f32 * w10
        + src[idx01] as f32 * w01
        + src[idx11] as f32 * w11;
    let g = src[idx00 + 1] as f32 * w00
        + src[idx10 + 1] as f32 * w10
        + src[idx01 + 1] as f32 * w01
        + src[idx11 + 1] as f32 * w11;
    let r = src[idx00 + 2] as f32 * w00
        + src[idx10 + 2] as f32 * w10
        + src[idx01 + 2] as f32 * w01
        + src[idx11 + 2] as f32 * w11;

    (
        b.round().clamp(0.0, 255.0) as u8,
        g.round().clamp(0.0, 255.0) as u8,
        r.round().clamp(0.0, 255.0) as u8,
    )
}

#[inline]
fn sample_bilinear(img: &DecodedImage, x: f64, y: f64) -> (u8, u8, u8) {
    let px = (x - 0.5).clamp(0.0, (img.width - 1) as f64);
    let py = (y - 0.5).clamp(0.0, (img.height - 1) as f64);

    let x0 = px.floor() as usize;
    let y0 = py.floor() as usize;
    let x1 = (x0 + 1).min(img.width as usize - 1);
    let y1 = (y0 + 1).min(img.height as usize - 1);

    let fx = (px - x0 as f64) as f32;
    let fy = (py - y0 as f64) as f32;

    let w00 = (1.0 - fx) * (1.0 - fy);
    let w10 = fx * (1.0 - fy);
    let w01 = (1.0 - fx) * fy;
    let w11 = fx * fy;

    let w = img.width as usize;
    let idx00 = (y0 * w + x0) * 4;
    let idx10 = (y0 * w + x1) * 4;
    let idx01 = (y1 * w + x0) * 4;
    let idx11 = (y1 * w + x1) * 4;

    let b = img.pixels[idx00] as f32 * w00
        + img.pixels[idx10] as f32 * w10
        + img.pixels[idx01] as f32 * w01
        + img.pixels[idx11] as f32 * w11;
    let g = img.pixels[idx00 + 1] as f32 * w00
        + img.pixels[idx10 + 1] as f32 * w10
        + img.pixels[idx01 + 1] as f32 * w01
        + img.pixels[idx11 + 1] as f32 * w11;
    let r = img.pixels[idx00 + 2] as f32 * w00
        + img.pixels[idx10 + 2] as f32 * w10
        + img.pixels[idx01 + 2] as f32 * w01
        + img.pixels[idx11 + 2] as f32 * w11;

    (
        b.round().clamp(0.0, 255.0) as u8,
        g.round().clamp(0.0, 255.0) as u8,
        r.round().clamp(0.0, 255.0) as u8,
    )
}

#[inline]
fn sample_bicubic(img: &DecodedImage, x: f64, y: f64) -> (u8, u8, u8) {
    // Bicubic Catmull-Rom spline sampling
    let px = (x - 0.5).clamp(0.0, (img.width - 1) as f64);
    let py = (y - 0.5).clamp(0.0, (img.height - 1) as f64);

    let x0 = px.floor() as i32;
    let y0 = py.floor() as i32;
    let fx = (px - x0 as f64) as f32;
    let fy = (py - y0 as f64) as f32;

    let wx = catmull_rom_weights(fx);
    let wy = catmull_rom_weights(fy);

    let w = img.width as i32;
    let h = img.height as i32;

    let mut b_acc = 0.0f32;
    let mut g_acc = 0.0f32;
    let mut r_acc = 0.0f32;

    for (j, &wy_j) in wy.iter().enumerate() {
        let sy = (y0 - 1 + j as i32).clamp(0, h - 1) as usize;
        let row_offset = sy * (w as usize) * 4;

        for (i, &wx_i) in wx.iter().enumerate() {
            let sx = (x0 - 1 + i as i32).clamp(0, w - 1) as usize;
            let weight = wy_j * wx_i;
            let offset = row_offset + sx * 4;

            b_acc += img.pixels[offset] as f32 * weight;
            g_acc += img.pixels[offset + 1] as f32 * weight;
            r_acc += img.pixels[offset + 2] as f32 * weight;
        }
    }

    (
        b_acc.round().clamp(0.0, 255.0) as u8,
        g_acc.round().clamp(0.0, 255.0) as u8,
        r_acc.round().clamp(0.0, 255.0) as u8,
    )
}

#[inline]
fn catmull_rom_weights(t: f32) -> [f32; 4] {
    let t2 = t * t;
    let t3 = t2 * t;
    [
        0.5 * (-t3 + 2.0 * t2 - t),
        0.5 * (3.0 * t3 - 5.0 * t2 + 2.0),
        0.5 * (-3.0 * t3 + 4.0 * t2 + t),
        0.5 * (t3 - t2),
    ]
}

/// Save ComposedBitmap to an 8-bit sRGB PNG file via WIC atomically
pub fn save_png_wic(bitmap: &ComposedBitmap, path: &Path) -> Result<()> {
    unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        let factory: IWICImagingFactory =
            CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER)?;

        // Write to a temporary file first for atomic replacement (Spec 7.2)
        let parent = path.parent().unwrap_or(Path::new("."));
        let temp_filename = format!(
            ".tmp_save_{}_{}.png",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        );
        let temp_path = parent.join(temp_filename);

        let wide_temp_path: Vec<u16> = temp_path
            .as_os_str()
            .encode_wide()
            .chain(std::iter::once(0))
            .collect();

        let stream = factory.CreateStream()?;
        stream.InitializeFromFilename(PCWSTR(wide_temp_path.as_ptr()), GENERIC_WRITE.0)?;

        let encoder: IWICBitmapEncoder =
            factory.CreateEncoder(&GUID_ContainerFormatPng, std::ptr::null())?;
        encoder.Initialize(&stream, WICBitmapEncoderNoCache)?;

        let mut frame_encode: Option<IWICBitmapFrameEncode> = None;
        let mut property_bag = None;
        encoder.CreateNewFrame(&mut frame_encode, &mut property_bag)?;
        let frame = frame_encode.ok_or_else(|| {
            Error::new(HRESULT(0x80004005u32 as i32), "Failed to create WIC frame")
        })?;

        frame.Initialize(property_bag.as_ref())?;
        frame.SetSize(bitmap.width, bitmap.height)?;

        let mut format = GUID_WICPixelFormat32bppPBGRA;
        frame.SetPixelFormat(&mut format)?;

        let stride = bitmap.width * 4;
        frame.WritePixels(bitmap.height, stride, &bitmap.pixels)?;

        frame.Commit()?;
        encoder.Commit()?;
        drop(encoder);
        drop(stream);

        // Atomic replace
        if let Err(e) = std::fs::rename(&temp_path, path) {
            // If rename fails (e.g. cross-volume or existing file on Windows), try copy + remove
            if std::fs::copy(&temp_path, path).is_ok() {
                let _ = std::fs::remove_file(&temp_path);
            } else {
                let _ = std::fs::remove_file(&temp_path);
                return Err(Error::new(
                    HRESULT(0x80004005u32 as i32),
                    format!("Failed to save PNG to {}: {}", path.display(), e),
                ));
            }
        }

        Ok(())
    }
}

use std::os::windows::ffi::OsStrExt;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout::*;

    #[test]
    fn test_compose_layout_and_save_png() {
        // Create dummy decoded image: 100x100 red image
        let mut pixels = vec![0u8; 100 * 100 * 4];
        for chunk in pixels.as_chunks_mut::<4>().0 {
            chunk[0] = 0; // B
            chunk[1] = 0; // G
            chunk[2] = 255; // R
            chunk[3] = 255; // A
        }
        let decoded = DecodedImage {
            id: ImageId("test_red.jpg".into()),
            orig_size: (100, 100),
            width: 100,
            height: 100,
            pixels,
        };

        let mut images = HashMap::new();
        images.insert(decoded.id.clone(), decoded);

        let req = LayoutRequest {
            monitor_px: (200, 100),
            mode: LayoutMode::Fixed(1),
            max_scale: 1.25,
            border_px: 0,
            frames: vec![FrameInput {
                image: Some(ImageSpec {
                    id: ImageId("test_red.jpg".into()),
                    size_px: (100, 100),
                }),
                pinned: false,
                pan: (0.5, 0.5),
            }],
        };

        let plan = plan_layout(&req, &[]);
        let composed = compose_layout(&plan, &images);
        assert_eq!(composed.width, 200);
        assert_eq!(composed.height, 100);

        // Test saving to PNG
        let temp_dir = std::env::temp_dir();
        let out_png = temp_dir.join("test_output_composed.png");
        let res = save_png_wic(&composed, &out_png);
        assert!(res.is_ok(), "PNG save failed: {:?}", res);
        assert!(out_png.exists());
        let _ = std::fs::remove_file(out_png);
    }

    #[test]
    fn test_compose_layout_dimming() {
        let mut pixels = vec![0u8; 100 * 100 * 4];
        for chunk in pixels.as_chunks_mut::<4>().0 {
            chunk[0] = 200; // B
            chunk[1] = 200; // G
            chunk[2] = 200; // R
            chunk[3] = 255; // A
        }
        let decoded = DecodedImage {
            id: ImageId("test_white.jpg".into()),
            orig_size: (100, 100),
            width: 100,
            height: 100,
            pixels,
        };

        let mut images = HashMap::new();
        images.insert(decoded.id.clone(), decoded);

        let req = LayoutRequest {
            monitor_px: (400, 100),
            mode: LayoutMode::Fixed(1),
            max_scale: 1.0,
            border_px: 0,
            frames: vec![FrameInput {
                image: Some(ImageSpec {
                    id: ImageId("test_white.jpg".into()),
                    size_px: (100, 100),
                }),
                pinned: false,
                pan: (0.5, 0.5),
            }],
        };

        let plan = plan_layout(&req, &[]);
        let composed_normal = compose_layout_with_dim(&plan, &images, 0.0);
        let composed_dimmed = compose_layout_with_dim(&plan, &images, 0.5);

        // Pixel at (10, 50) is in background blur area (since slot is 400x100, fg is 100x100 at center 150..250)
        let (b_norm, _, _, _) = composed_normal.get_pixel(10, 50);
        let (b_dim, _, _, _) = composed_dimmed.get_pixel(10, 50);
        assert!(b_norm > 100);
        assert!(b_dim < b_norm, "Dimmed pixel ({}) should be darker than normal ({})", b_dim, b_norm);
        // Approximately 50%
        let ratio = b_dim as f64 / b_norm as f64;
        assert!((ratio - 0.5).abs() < 0.1, "Ratio was {}", ratio);
    }
}
