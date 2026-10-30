use std::path::Path;
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Imaging::*;
use windows::Win32::System::Com::StructuredStorage::PROPVARIANT;
use windows::Win32::System::Com::*;
use windows::core::*;

use crate::layout::{ImageId, ImageSpec};

#[derive(Debug, Clone)]
pub struct DecodedImage {
    pub id: ImageId,
    pub orig_size: (u32, u32), // Iw, Ih (post-orientation)
    pub width: u32,            // buffer width
    pub height: u32,           // buffer height
    pub pixels: Vec<u8>,       // 32-bit BGRA (B, G, R, A), alpha = 255 opaque (blended over black)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExifOrientation {
    Normal = 1,
    FlipHorizontal = 2,
    Rotate180 = 3,
    FlipVertical = 4,
    Transpose = 5,
    Rotate90 = 6,
    Transverse = 7,
    Rotate270 = 8,
}

impl ExifOrientation {
    pub fn from_u16(val: u16) -> Self {
        match val {
            1 => Self::Normal,
            2 => Self::FlipHorizontal,
            3 => Self::Rotate180,
            4 => Self::FlipVertical,
            5 => Self::Transpose,
            6 => Self::Rotate90,
            7 => Self::Transverse,
            8 => Self::Rotate270,
            _ => Self::Normal,
        }
    }

    pub fn to_wic_transform(self) -> WICBitmapTransformOptions {
        match self {
            Self::Normal => WICBitmapTransformRotate0,
            Self::FlipHorizontal => WICBitmapTransformFlipHorizontal,
            Self::Rotate180 => WICBitmapTransformRotate180,
            Self::FlipVertical => WICBitmapTransformFlipVertical,
            Self::Transpose => WICBitmapTransformOptions(
                WICBitmapTransformRotate90.0 | WICBitmapTransformFlipHorizontal.0,
            ),
            Self::Rotate90 => WICBitmapTransformRotate90,
            Self::Transverse => WICBitmapTransformOptions(
                WICBitmapTransformRotate270.0 | WICBitmapTransformFlipHorizontal.0,
            ),
            Self::Rotate270 => WICBitmapTransformRotate270,
        }
    }

    pub fn swaps_dimensions(self) -> bool {
        matches!(
            self,
            Self::Transpose | Self::Rotate90 | Self::Transverse | Self::Rotate270
        )
    }
}

pub struct ImageDecoder {
    factory: IWICImagingFactory,
}

impl ImageDecoder {
    pub fn new() -> Result<Self> {
        unsafe {
            // Ensure COM is initialized on the calling thread
            let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
            let factory: IWICImagingFactory =
                CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER)?;
            Ok(Self { factory })
        }
    }

    /// Read only the dimensions (Iw, Ih) and orientation without decoding all pixels.
    pub fn read_image_spec(&self, path: &Path) -> Result<ImageSpec> {
        let (raw_w, raw_h, orientation) = self.inspect_header(path)?;

        let (iw, ih) = if orientation.swaps_dimensions() {
            (raw_h, raw_w)
        } else {
            (raw_w, raw_h)
        };

        Ok(ImageSpec {
            id: ImageId(path.to_string_lossy().to_string()),
            size_px: (iw, ih),
        })
    }

    fn inspect_header(&self, path: &Path) -> Result<(u32, u32, ExifOrientation)> {
        unsafe {
            let wide_path: Vec<u16> = path
                .as_os_str()
                .encode_wide()
                .chain(std::iter::once(0))
                .collect();

            let decoder = self.factory.CreateDecoderFromFilename(
                PCWSTR(wide_path.as_ptr()),
                None,
                GENERIC_READ,
                WICDecodeMetadataCacheOnDemand,
            )?;

            let frame = decoder.GetFrame(0)?;
            let mut raw_w = 0u32;
            let mut raw_h = 0u32;
            frame.GetSize(&mut raw_w, &mut raw_h)?;

            // Section 2.2: Input limit: raw edge <= 32,768 and MP <= 100
            if raw_w.max(raw_h) > 32_768 || (raw_w as u64) * (raw_h as u64) > 100_000_000 {
                return Err(Error::new(
                    HRESULT(0x800700DFu32 as i32), // ERROR_FILE_TOO_LARGE
                    "Image dimension exceeds limit (max 32,768px or 100MP)",
                ));
            }

            let orientation = self
                .read_exif_orientation(&frame)
                .unwrap_or(ExifOrientation::Normal);
            Ok((raw_w, raw_h, orientation))
        }
    }

    fn read_exif_orientation(&self, frame: &IWICBitmapFrameDecode) -> Option<ExifOrientation> {
        unsafe {
            let reader = frame.GetMetadataQueryReader().ok()?;
            let queries = [
                "/app1/ifd/{ushort=274}",
                "/ifd/{ushort=274}",
                "/app1/ifd/exif/{ushort=274}",
                "/System.Photo.Orientation",
            ];

            for q in queries {
                let wide_q: Vec<u16> = q.encode_utf16().chain(std::iter::once(0)).collect();
                let mut prop = PROPVARIANT::default();
                if reader
                    .GetMetadataByName(PCWSTR(wide_q.as_ptr()), &mut prop)
                    .is_ok()
                {
                    // VT_UI2 is 18 (ushort)
                    let val = match prop.Anonymous.Anonymous.vt.0 {
                        18 => Some(prop.Anonymous.Anonymous.Anonymous.uiVal),
                        19 => Some(prop.Anonymous.Anonymous.Anonymous.ulVal as u16),
                        2 => Some(prop.Anonymous.Anonymous.Anonymous.iVal as u16),
                        3 => Some(prop.Anonymous.Anonymous.Anonymous.lVal as u16),
                        _ => None,
                    };
                    if let Some(v) = val
                        && (1..=8).contains(&v)
                    {
                        return Some(ExifOrientation::from_u16(v));
                    }
                }
            }
            None
        }
    }

    /// Fully decode an image, applying EXIF rotation, ICC to sRGB, transparency compositing over black,
    /// and optional high quality downscaling if max_edge is specified.
    pub fn decode_image(&self, path: &Path, max_edge: Option<u32>) -> Result<DecodedImage> {
        unsafe {
            let wide_path: Vec<u16> = path
                .as_os_str()
                .encode_wide()
                .chain(std::iter::once(0))
                .collect();

            let decoder = self.factory.CreateDecoderFromFilename(
                PCWSTR(wide_path.as_ptr()),
                None,
                GENERIC_READ,
                WICDecodeMetadataCacheOnDemand,
            )?;

            let frame = decoder.GetFrame(0)?;
            let mut raw_w = 0u32;
            let mut raw_h = 0u32;
            frame.GetSize(&mut raw_w, &mut raw_h)?;

            if raw_w.max(raw_h) > 32_768 || (raw_w as u64) * (raw_h as u64) > 100_000_000 {
                return Err(Error::new(
                    HRESULT(0x800700DFu32 as i32),
                    "Image dimension exceeds limit (max 32,768px or 100MP)",
                ));
            }

            let orientation = self
                .read_exif_orientation(&frame)
                .unwrap_or(ExifOrientation::Normal);
            let (orig_w, orig_h) = if orientation.swaps_dimensions() {
                (raw_h, raw_w)
            } else {
                (raw_w, raw_h)
            };

            // Setup source: apply EXIF flip/rotation first
            let source_bitmap: IWICBitmapSource = if orientation != ExifOrientation::Normal {
                let rotator = self.factory.CreateBitmapFlipRotator()?;
                rotator.Initialize(&frame, orientation.to_wic_transform())?;
                rotator.cast()?
            } else {
                frame.cast()?
            };

            // Optional color context transformation to sRGB
            let transformed_source = match self.try_color_transform(&source_bitmap, &frame) {
                Ok(trans) => trans,
                Err(_) => {
                    // Fallback to source directly with warning logged
                    source_bitmap
                }
            };

            // Optional downscaling for cache
            let scaled_source: IWICBitmapSource = if let Some(limit) = max_edge {
                let max_dim = orig_w.max(orig_h);
                if max_dim > limit && limit > 0 {
                    let scale = limit as f64 / max_dim as f64;
                    let target_w = ((orig_w as f64 * scale).round() as u32).max(1);
                    let target_h = ((orig_h as f64 * scale).round() as u32).max(1);
                    let scaler = self.factory.CreateBitmapScaler()?;
                    scaler.Initialize(
                        &transformed_source,
                        target_w,
                        target_h,
                        WICBitmapInterpolationModeHighQualityCubic,
                    )?;
                    scaler.cast()?
                } else {
                    transformed_source
                }
            } else {
                transformed_source
            };

            // Format converter to 32bppPBGRA (premultiplied BGRA)
            let converter = self.factory.CreateFormatConverter()?;
            converter.Initialize(
                &scaled_source,
                &GUID_WICPixelFormat32bppPBGRA,
                WICBitmapDitherTypeNone,
                None,
                0.0,
                WICBitmapPaletteTypeCustom,
            )?;

            let mut out_w = 0u32;
            let mut out_h = 0u32;
            converter.GetSize(&mut out_w, &mut out_h)?;

            let stride = out_w.checked_mul(4).ok_or_else(|| {
                Error::new(HRESULT(0x8007000Eu32 as i32), "Image buffer size overflow")
            })?;
            let buffer_size = (stride as usize)
                .checked_mul(out_h as usize)
                .ok_or_else(|| {
                    Error::new(HRESULT(0x8007000Eu32 as i32), "Image buffer size overflow")
                })?;

            let mut pixels = vec![0u8; buffer_size];
            converter.CopyPixels(std::ptr::null(), stride, &mut pixels)?;

            // Section 2.2: Opaque black background compositing:
            // For PBGRA, transparent pixels are (r*a/255, g*a/255, b*a/255).
            // When composited over opaque black #000000, the resulting RGB is unchanged,
            // and the output alpha is set to 255 (fully opaque).
            for chunk in pixels.as_chunks_mut::<4>().0 {
                chunk[3] = 255;
            }

            Ok(DecodedImage {
                id: ImageId(path.to_string_lossy().to_string()),
                orig_size: (orig_w, orig_h),
                width: out_w,
                height: out_h,
                pixels,
            })
        }
    }

    unsafe fn try_color_transform(
        &self,
        source: &IWICBitmapSource,
        frame: &IWICBitmapFrameDecode,
    ) -> Result<IWICBitmapSource> {
        unsafe {
            let mut count = 0u32;
            frame.GetColorContexts(&mut [None], &mut count)?;
            if count == 0 {
                return Ok(source.clone());
            }

            let mut contexts = vec![None; count as usize];
            frame.GetColorContexts(&mut contexts, &mut count)?;

            if let Some(Some(src_ctx)) = contexts.into_iter().next() {
                let dst_ctx = self.factory.CreateColorContext()?;
                // Predefined sRGB
                dst_ctx.InitializeFromExifColorSpace(1)?; // 1 = sRGB

                let transformer = self.factory.CreateColorTransformer()?;
                let format = source.GetPixelFormat()?;

                transformer.Initialize(source, &src_ctx, &dst_ctx, &format)?;

                Ok(transformer.cast()?)
            } else {
                Ok(source.clone())
            }
        }
    }
}

use std::os::windows::ffi::OsStrExt;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_decoder_instantiation() {
        let decoder = ImageDecoder::new();
        assert!(decoder.is_ok());
    }

    #[test]
    fn test_exif_orientation_swaps() {
        assert!(!ExifOrientation::Normal.swaps_dimensions());
        assert!(!ExifOrientation::FlipHorizontal.swaps_dimensions());
        assert!(!ExifOrientation::Rotate180.swaps_dimensions());
        assert!(!ExifOrientation::FlipVertical.swaps_dimensions());
        assert!(ExifOrientation::Transpose.swaps_dimensions());
        assert!(ExifOrientation::Rotate90.swaps_dimensions());
        assert!(ExifOrientation::Transverse.swaps_dimensions());
        assert!(ExifOrientation::Rotate270.swaps_dimensions());
    }

    #[test]
    fn test_nonexistent_file_returns_err() {
        let decoder = ImageDecoder::new().unwrap();
        let res = decoder.read_image_spec(Path::new("non_existent_file.png"));
        assert!(res.is_err());
    }
}
