use crate::decode::DecodedImage;

/// Calculates the optimal pan (pan_x, pan_y) in range [0.0, 1.0] for the given image
/// and slot dimensions using visual energy and saliency detection (edge density + detail gradient).
///
/// Ensures faces, upper body portraits, and primary subjects are positioned naturally
/// without heads or key elements being cropped off by margins.
pub fn calculate_smart_pan(
    img: &DecodedImage,
    slot_w: u32,
    slot_h: u32,
    scale: f64,
) -> (f64, f64) {
    if img.width < 2 || img.height < 2 || scale <= 0.0 || slot_w == 0 || slot_h == 0 {
        return (0.5, 0.5);
    }

    let iw = img.width as f64;
    let ih = img.height as f64;

    // Visible window dimensions in image coordinates
    let src_w = (slot_w as f64 / scale).min(iw);
    let src_h = (slot_h as f64 / scale).min(ih);

    let can_pan_x = iw > src_w + 1.0;
    let can_pan_y = ih > src_h + 1.0;

    if !can_pan_x && !can_pan_y {
        return (0.5, 0.5);
    }

    // Step sampling for high performance (< 1ms on large textures)
    let step_x = ((img.width / 80).max(1)) as usize;
    let step_y = ((img.height / 80).max(1)) as usize;

    let w = img.width as usize;
    let h = img.height as usize;

    let mut col_energy = vec![0.0f64; w];
    let mut row_energy = vec![0.0f64; h];

    // Compute gradient & saturation energy across sampled grid
    for y in (step_y..(h - step_y)).step_by(step_y) {
        let row_curr = y * w * 4;
        let row_prev = (y - step_y) * w * 4;
        let row_next = (y + step_y) * w * 4;

        for x in (step_x..(w - step_x)).step_by(step_x) {
            let idx = row_curr + x * 4;
            let (b, g, r) = (
                img.pixels[idx] as f64,
                img.pixels[idx + 1] as f64,
                img.pixels[idx + 2] as f64,
            );

            // Horizontal & vertical gradients
            let idx_l = row_curr + (x - step_x) * 4;
            let idx_r = row_curr + (x + step_x) * 4;
            let lum_l = 0.299 * (img.pixels[idx_l + 2] as f64)
                + 0.587 * (img.pixels[idx_l + 1] as f64)
                + 0.114 * (img.pixels[idx_l] as f64);
            let lum_r = 0.299 * (img.pixels[idx_r + 2] as f64)
                + 0.587 * (img.pixels[idx_r + 1] as f64)
                + 0.114 * (img.pixels[idx_r] as f64);

            let idx_u = row_prev + x * 4;
            let idx_d = row_next + x * 4;
            let lum_u = 0.299 * (img.pixels[idx_u + 2] as f64)
                + 0.587 * (img.pixels[idx_u + 1] as f64)
                + 0.114 * (img.pixels[idx_u] as f64);
            let lum_d = 0.299 * (img.pixels[idx_d + 2] as f64)
                + 0.587 * (img.pixels[idx_d + 1] as f64)
                + 0.114 * (img.pixels[idx_d] as f64);

            let grad = (lum_r - lum_l).abs() + (lum_d - lum_u).abs();

            // Color saturation / vibrancy
            let max_c = r.max(g).max(b);
            let min_c = r.min(g).min(b);
            let sat = max_c - min_c;

            // Skin tone bonus (human portrait & character saliency)
            // R > G > B with moderate saturation
            let skin_bonus = if r > g && g > b && sat > 15.0 && sat < 180.0 {
                30.0
            } else {
                0.0
            };

            let energy = grad + sat * 0.4 + skin_bonus;

            col_energy[x] += energy;
            row_energy[y] += energy;
        }
    }

    // Determine focus_y
    let pan_y = if can_pan_y {
        let total_row_energy: f64 = row_energy.iter().sum();
        let focus_y = if total_row_energy > 1.0 {
            // Compute weighted centroid of row energy
            let weighted_sum: f64 = row_energy
                .iter()
                .enumerate()
                .map(|(y, &e)| (y as f64) * e)
                .sum();
            weighted_sum / total_row_energy
        } else {
            // Default portrait bias: upper third
            ih * 0.35
        };

        // For vertical framing, keep head/face with comfortable headroom
        // Optimal position of focus in visible window: ~38% from the top
        let target_window_offset = src_h * 0.38;
        let desired_src_top = focus_y - target_window_offset;
        let max_src_top = ih - src_h;

        if max_src_top > 0.0 {
            (desired_src_top / max_src_top).clamp(0.0, 1.0)
        } else {
            0.5
        }
    } else {
        0.5
    };

    // Determine focus_x
    let pan_x = if can_pan_x {
        let total_col_energy: f64 = col_energy.iter().sum();
        let focus_x = if total_col_energy > 1.0 {
            let weighted_sum: f64 = col_energy
                .iter()
                .enumerate()
                .map(|(x, &e)| (x as f64) * e)
                .sum();
            weighted_sum / total_col_energy
        } else {
            iw * 0.5
        };

        // Center primary subject horizontally
        let target_window_offset = src_w * 0.5;
        let desired_src_left = focus_x - target_window_offset;
        let max_src_left = iw - src_w;

        if max_src_left > 0.0 {
            (desired_src_left / max_src_left).clamp(0.0, 1.0)
        } else {
            0.5
        }
    } else {
        0.5
    };

    (pan_x, pan_y)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout::ImageId;

    #[test]
    fn test_smart_pan_portrait_focus() {
        // Create 100x300 image where top (y=50) has a high-contrast "face" subject
        let mut pixels = vec![0u8; 100 * 300 * 4];

        // Draw a high contrast subject at y: 40..80, x: 30..70 (face region)
        for y in 40..80 {
            for x in 30..70 {
                let idx = (y * 100 + x) * 4;
                pixels[idx] = 120; // B
                pixels[idx + 1] = 160; // G
                pixels[idx + 2] = 240; // R (skin tone)
                pixels[idx + 3] = 255;
            }
        }

        let img = DecodedImage {
            id: ImageId("test_portrait.jpg".into()),
            orig_size: (100, 300),
            width: 100,
            height: 300,
            pixels,
        };

        // Slot 100x150, scale 1.0 (so height can pan from 0 to 150)
        let (pan_x, pan_y) = calculate_smart_pan(&img, 100, 150, 1.0);

        // pan_x should be centered ~0.5
        assert!((pan_x - 0.5).abs() < 0.1);

        // pan_y should focus on upper region (pan_y < 0.4, definitely not 0.5 or bottom)
        assert!(pan_y < 0.35, "Expected pan_y to focus on top face, got {}", pan_y);
    }
}
