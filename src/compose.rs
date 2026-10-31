use std::collections::HashMap;
use std::path::Path;

use rayon::prelude::*;
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
        let size = (width as usize) * (height as usize);
        let mut pixels = vec![0u8; size * 4];
        let row_stride = (width as usize) * 4;
        // Initialize with opaque black (B=0, G=0, R=0, A=255) by rows in parallel
        pixels.par_chunks_exact_mut(row_stride).for_each(|row| {
            for chunk in row.chunks_exact_mut(4) {
                chunk[3] = 255;
            }
        });
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
    let mut target = compose_base_layer(plan, images, dim_ratio);
    compose_foreground_layer(&mut target, plan, images);
    target
}

pub fn compose_base_layer(
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
            if placed.blur_sigma > 0.0 {
                render_background_blur(&mut target, slot, img, placed.blur_sigma, dim_ratio);
            }
        }
    }

    target
}

pub fn compose_foreground_layer(
    target: &mut ComposedBitmap,
    plan: &LayoutPlan,
    images: &HashMap<ImageId, DecodedImage>,
) {
    for frame in &plan.frames {
        let slot = frame.slot;
        if let Some(placed) = &frame.content
            && let Some(img) = images.get(&placed.id)
        {
            render_foreground(target, slot, img, placed);
        }
    }
}

pub fn plan_has_background_blur(plan: &LayoutPlan) -> bool {
    plan.frames.iter().any(|f| {
        f.content.as_ref().map_or(false, |c| c.blur_sigma > 0.0)
    })
}

pub fn compose_single_frame_foreground(
    target: &mut ComposedBitmap,
    frame: &crate::layout::FramePlan,
    base: Option<&ComposedBitmap>,
    images: &HashMap<ImageId, DecodedImage>,
) {
    let slot = frame.slot;
    if slot.width == 0 || slot.height == 0 {
        return;
    }

    let min_x = (slot.x.max(0) as u32).min(target.width);
    let max_x = ((slot.x + slot.width as i32).max(0) as u32).min(target.width);
    let min_y = (slot.y.max(0) as u32).min(target.height);
    let max_y = ((slot.y + slot.height as i32).max(0) as u32).min(target.height);

    if min_x >= max_x || min_y >= max_y {
        return;
    }

    let row_stride = (target.width as usize) * 4;
    let byte_start = (min_x as usize) * 4;
    let byte_end = (max_x as usize) * 4;

    let target_slice = &mut target.pixels[(min_y as usize) * row_stride..(max_y as usize) * row_stride];

    if let Some(base) = base {
        let base_slice = &base.pixels[(min_y as usize) * row_stride..(max_y as usize) * row_stride];
        target_slice
            .par_chunks_exact_mut(row_stride)
            .zip(base_slice.par_chunks_exact(row_stride))
            .for_each(|(target_row, base_row)| {
                target_row[byte_start..byte_end].copy_from_slice(&base_row[byte_start..byte_end]);
            });
    } else {
        target_slice
            .par_chunks_exact_mut(row_stride)
            .for_each(|target_row| {
                for chunk in target_row[byte_start..byte_end].chunks_exact_mut(4) {
                    chunk[0] = 0;
                    chunk[1] = 0;
                    chunk[2] = 0;
                    chunk[3] = 255;
                }
            });
    }

    // Render foreground for this frame
    if let Some(placed) = &frame.content
        && let Some(img) = images.get(&placed.id)
    {
        render_foreground(target, slot, img, placed);
    }
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
    let img_w = img.width as i32;
    let img_h = img.height as i32;
    let stride_img = (img.width as usize) * 4;

    // Precalculate horizontal interpolation parameters (weights & pixel offsets)
    // This turns O(W * H * 16) floating-point calculations into O(W + H)
    struct XSample {
        sx_offsets: [usize; 4],
        weights: [f32; 4],
    }

    let mut x_table = Vec::with_capacity((max_x - min_x) as usize);
    for x in min_x..max_x {
        let u = ((x as i32 - fg_dest.x) as f64 + 0.5) / dw_f;
        let sx = src_x_start + u * src_w;
        let px = (sx - 0.5).clamp(0.0, (img.width - 1) as f64);
        let x0 = px.floor() as i32;
        let fx = (px - x0 as f64) as f32;
        let wx = catmull_rom_weights(fx);

        let sx0 = (x0 - 1).clamp(0, img_w - 1) as usize * 4;
        let sx1 = x0.clamp(0, img_w - 1) as usize * 4;
        let sx2 = (x0 + 1).clamp(0, img_w - 1) as usize * 4;
        let sx3 = (x0 + 2).clamp(0, img_w - 1) as usize * 4;
        x_table.push(XSample {
            sx_offsets: [sx0, sx1, sx2, sx3],
            weights: wx,
        });
    }

    let target_w = target.width as usize;
    let row_stride = target_w * 4;
    let target_slice = &mut target.pixels[(min_y as usize) * row_stride..(max_y as usize) * row_stride];

    target_slice
        .par_chunks_exact_mut(row_stride)
        .enumerate()
        .for_each(|(i, row_slice)| {
            let y = min_y + i as u32;
            let v = ((y as i32 - fg_dest.y) as f64 + 0.5) / dh_f;
            let sy = src_y_start + v * src_h;
            let py = (sy - 0.5).clamp(0.0, (img.height - 1) as f64);
            let y0 = py.floor() as i32;
            let fy = (py - y0 as f64) as f32;
            let wy = catmull_rom_weights(fy);

            let row0 = (y0 - 1).clamp(0, img_h - 1) as usize * stride_img;
            let row1 = y0.clamp(0, img_h - 1) as usize * stride_img;
            let row2 = (y0 + 1).clamp(0, img_h - 1) as usize * stride_img;
            let row3 = (y0 + 2).clamp(0, img_h - 1) as usize * stride_img;
            let rows = [row0, row1, row2, row3];

            let target_row = &mut row_slice[(min_x as usize) * 4..(max_x as usize) * 4];

            for (sample, out_pixel) in x_table.iter().zip(target_row.chunks_exact_mut(4)) {
                let mut b_acc = 0.0f32;
                let mut g_acc = 0.0f32;
                let mut r_acc = 0.0f32;

                for (&row, &wy_j) in rows.iter().zip(wy.iter()) {
                    for (&sx_off, &wx_i) in sample.sx_offsets.iter().zip(sample.weights.iter()) {
                        let offset = row + sx_off;
                        let weight = wy_j * wx_i;
                        b_acc += img.pixels[offset] as f32 * weight;
                        g_acc += img.pixels[offset + 1] as f32 * weight;
                        r_acc += img.pixels[offset + 2] as f32 * weight;
                    }
                }

                out_pixel[0] = b_acc.round().clamp(0.0, 255.0) as u8;
                out_pixel[1] = g_acc.round().clamp(0.0, 255.0) as u8;
                out_pixel[2] = r_acc.round().clamp(0.0, 255.0) as u8;
                out_pixel[3] = 255;
            }
        });
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

    small_buf
        .par_chunks_exact_mut((small_w as usize) * 3)
        .enumerate()
        .for_each(|(sy, row)| {
            let v = (sy as f64 + 0.5) / sh;
            let img_y = bg_y + v * bg_h;
            for (sx, pixel) in (0..small_w).zip(row.chunks_exact_mut(3)) {
                let u = (sx as f64 + 0.5) / sw;
                let img_x = bg_x + u * bg_w;
                let (b, g, r) = sample_bilinear(img, img_x, img_y);
                pixel[0] = b;
                pixel[1] = g;
                pixel[2] = r;
            }
        });

    // 2. Gaussian blur on small buffer (separable horizontal + vertical) with edge clamp
    let blurred = gaussian_blur_rgb(&small_buf, small_w, small_h, small_sigma);

    // 3. Upscale blurred buffer into slot on target
    let min_x = (slot.x.max(0) as u32).min(target.width);
    let max_x = ((slot.x + slot.width as i32).max(0) as u32).min(target.width);
    let min_y = (slot.y.max(0) as u32).min(target.height);
    let max_y = ((slot.y + slot.height as i32).max(0) as u32).min(target.height);

    if min_x < max_x && min_y < max_y {
        let sw_f = slot.width as f64;
        let sh_f = slot.height as f64;
        let dim_factor = (1.0 - dim_ratio).clamp(0.0, 1.0);
        let target_w = target.width as usize;
        let row_stride = target_w * 4;

        let target_slice = &mut target.pixels[(min_y as usize) * row_stride..(max_y as usize) * row_stride];
        target_slice
            .par_chunks_exact_mut(row_stride)
            .enumerate()
            .for_each(|(i, row_slice)| {
                let y = min_y + i as u32;
                let v = ((y as i32 - slot.y) as f64 + 0.5) / sh_f;
                let by = v * (small_h as f64);
                let target_subrow = &mut row_slice[(min_x as usize) * 4..(max_x as usize) * 4];

                for (x_offset, out_pixel) in target_subrow.chunks_exact_mut(4).enumerate() {
                    let x = min_x + x_offset as u32;
                    let u = ((x as i32 - slot.x) as f64 + 0.5) / sw_f;
                    let bx = u * (small_w as f64);

                    let (b, g, r) = sample_small_bilinear(&blurred, small_w, small_h, bx, by);
                    if dim_ratio > 0.0 {
                        out_pixel[0] = (b as f64 * dim_factor).round() as u8;
                        out_pixel[1] = (g as f64 * dim_factor).round() as u8;
                        out_pixel[2] = (r as f64 * dim_factor).round() as u8;
                    } else {
                        out_pixel[0] = b;
                        out_pixel[1] = g;
                        out_pixel[2] = r;
                    }
                    out_pixel[3] = 255;
                }
            });
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
    let kernel_f32: Vec<f32> = kernel.iter().map(|&v| v as f32).collect();

    let w = width as i32;
    let h = height as i32;
    let row_stride_3 = (width as usize) * 3;

    // Horizontal pass: partition into left clamped, middle unclamped, right clamped
    let mut tmp = vec![0f32; (width as usize) * (height as usize) * 3];
    let left_end = radius.min(w);
    let right_start = (w - radius).max(left_end);

    tmp.par_chunks_exact_mut(row_stride_3)
        .enumerate()
        .for_each(|(y_idx, row_out)| {
            let row_in = y_idx * row_stride_3;
            let src_row = &src[row_in..row_in + row_stride_3];

            for x in 0..left_end {
                let mut acc_b = 0.0f32;
                let mut acc_g = 0.0f32;
                let mut acc_r = 0.0f32;
                for (k_idx, k) in (-radius..=radius).enumerate() {
                    let sx = (x + k).clamp(0, w - 1) as usize;
                    let src_idx = sx * 3;
                    let weight = kernel_f32[k_idx];
                    acc_b += src_row[src_idx] as f32 * weight;
                    acc_g += src_row[src_idx + 1] as f32 * weight;
                    acc_r += src_row[src_idx + 2] as f32 * weight;
                }
                let out_idx = (x as usize) * 3;
                row_out[out_idx] = acc_b;
                row_out[out_idx + 1] = acc_g;
                row_out[out_idx + 2] = acc_r;
            }

            for x in left_end..right_start {
                let mut acc_b = 0.0f32;
                let mut acc_g = 0.0f32;
                let mut acc_r = 0.0f32;
                for (k_idx, k) in (-radius..=radius).enumerate() {
                    let sx = (x + k) as usize;
                    let src_idx = sx * 3;
                    let weight = kernel_f32[k_idx];
                    acc_b += src_row[src_idx] as f32 * weight;
                    acc_g += src_row[src_idx + 1] as f32 * weight;
                    acc_r += src_row[src_idx + 2] as f32 * weight;
                }
                let out_idx = (x as usize) * 3;
                row_out[out_idx] = acc_b;
                row_out[out_idx + 1] = acc_g;
                row_out[out_idx + 2] = acc_r;
            }

            for x in right_start..w {
                let mut acc_b = 0.0f32;
                let mut acc_g = 0.0f32;
                let mut acc_r = 0.0f32;
                for (k_idx, k) in (-radius..=radius).enumerate() {
                    let sx = (x + k).clamp(0, w - 1) as usize;
                    let src_idx = sx * 3;
                    let weight = kernel_f32[k_idx];
                    acc_b += src_row[src_idx] as f32 * weight;
                    acc_g += src_row[src_idx + 1] as f32 * weight;
                    acc_r += src_row[src_idx + 2] as f32 * weight;
                }
                let out_idx = (x as usize) * 3;
                row_out[out_idx] = acc_b;
                row_out[out_idx + 1] = acc_g;
                row_out[out_idx + 2] = acc_r;
            }
        });

    // Vertical pass:
    let mut out = vec![0u8; (width as usize) * (height as usize) * 3];
    let top_end = radius.min(h);
    let bottom_start = (h - radius).max(top_end);

    out.par_chunks_exact_mut(row_stride_3)
        .enumerate()
        .for_each(|(y_idx, row_out)| {
            let y = y_idx as i32;
            if y < top_end {
                for x in 0..w {
                    let mut acc_b = 0.0f32;
                    let mut acc_g = 0.0f32;
                    let mut acc_r = 0.0f32;
                    for (k_idx, k) in (-radius..=radius).enumerate() {
                        let sy = (y + k).clamp(0, h - 1) as usize;
                        let src_idx = sy * row_stride_3 + (x as usize) * 3;
                        let weight = kernel_f32[k_idx];
                        acc_b += tmp[src_idx] * weight;
                        acc_g += tmp[src_idx + 1] * weight;
                        acc_r += tmp[src_idx + 2] * weight;
                    }
                    let out_idx = (x as usize) * 3;
                    row_out[out_idx] = acc_b.round().clamp(0.0, 255.0) as u8;
                    row_out[out_idx + 1] = acc_g.round().clamp(0.0, 255.0) as u8;
                    row_out[out_idx + 2] = acc_r.round().clamp(0.0, 255.0) as u8;
                }
            } else if y < bottom_start {
                for x in 0..w {
                    let mut acc_b = 0.0f32;
                    let mut acc_g = 0.0f32;
                    let mut acc_r = 0.0f32;
                    for (k_idx, k) in (-radius..=radius).enumerate() {
                        let sy = (y + k) as usize;
                        let src_idx = sy * row_stride_3 + (x as usize) * 3;
                        let weight = kernel_f32[k_idx];
                        acc_b += tmp[src_idx] * weight;
                        acc_g += tmp[src_idx + 1] * weight;
                        acc_r += tmp[src_idx + 2] * weight;
                    }
                    let out_idx = (x as usize) * 3;
                    row_out[out_idx] = acc_b.round().clamp(0.0, 255.0) as u8;
                    row_out[out_idx + 1] = acc_g.round().clamp(0.0, 255.0) as u8;
                    row_out[out_idx + 2] = acc_r.round().clamp(0.0, 255.0) as u8;
                }
            } else {
                for x in 0..w {
                    let mut acc_b = 0.0f32;
                    let mut acc_g = 0.0f32;
                    let mut acc_r = 0.0f32;
                    for (k_idx, k) in (-radius..=radius).enumerate() {
                        let sy = (y + k).clamp(0, h - 1) as usize;
                        let src_idx = sy * row_stride_3 + (x as usize) * 3;
                        let weight = kernel_f32[k_idx];
                        acc_b += tmp[src_idx] * weight;
                        acc_g += tmp[src_idx + 1] * weight;
                        acc_r += tmp[src_idx + 2] * weight;
                    }
                    let out_idx = (x as usize) * 3;
                    row_out[out_idx] = acc_b.round().clamp(0.0, 255.0) as u8;
                    row_out[out_idx + 1] = acc_g.round().clamp(0.0, 255.0) as u8;
                    row_out[out_idx + 2] = acc_r.round().clamp(0.0, 255.0) as u8;
                }
            }
        });

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
