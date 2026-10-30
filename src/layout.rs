#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct ImageId(pub String);

#[derive(Debug, Clone, PartialEq)]
pub struct ImageSpec {
    pub id: ImageId,
    pub size_px: (u32, u32), // EXIF回転・反転適用後の元寸法 (Iw, Ih)
}

#[derive(Debug, Clone)]
pub struct FrameInput {
    pub image: Option<ImageSpec>, // Noneは空き枠
    pub pinned: bool,
    pub pan: (f64, f64), // 各軸 0.0..=1.0（0.5が中央）
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LayoutMode {
    Auto,
    Fixed(u8), // 1..=3
}

#[derive(Debug, Clone)]
pub struct LayoutRequest {
    pub monitor_px: (u32, u32),  // 対象モニターの物理解像度
    pub mode: LayoutMode,        // Auto | Fixed(1..=3)
    pub max_scale: f64,          // M (1.00..=2.50)
    pub border_px: u32,          // 0..=8
    pub frames: Vec<FrameInput>, // 現在の枠リスト
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SplitAxis {
    Horizontal, // 左右分割 (W >= H)
    Vertical,   // 上下分割 (W < H)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RectPx {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RectF {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScaleLimit {
    None,
    MaxScale,
    CropLimit,
}

#[derive(Debug, Clone)]
pub struct PlacedImage {
    pub id: ImageId,
    pub fg_src: RectF,         // 元画像座標系での前景切り抜き矩形
    pub fg_dest: RectPx,       // 出力座標系での前景描画先矩形（物理px）
    pub bg_src: RectF,         // 元画像座標系での背景cover切り抜き矩形
    pub blur_sigma: f64,       // 出力基準のガウシアン標準偏差
    pub scale: f64,            // 前景の描画倍率
    pub can_pan: (bool, bool), // (X軸パン可, Y軸パン可)
    pub scale_limited: ScaleLimit,
}

#[derive(Debug, Clone)]
pub struct FramePlan {
    pub slot: RectPx,                 // 出力内の枠矩形（境界線を除く）
    pub content: Option<PlacedImage>, // Noneは空き枠
    pub pinned: bool,
    pub pan: (f64, f64), // 正規化パン（非可動軸は0.5）
}

#[derive(Debug, Clone)]
pub struct LayoutPlan {
    pub monitor_px: (u32, u32),
    pub resolved_count: u8, // 採用枠数 (1..=3)
    pub axis: SplitAxis,    // Horizontal | Vertical
    pub frames: Vec<FramePlan>,
    pub coverage: f64, // 枠合計面積に対する前景被覆率
}

const MIN_VISIBLE_RATIO: f64 = 0.60;

pub fn plan_layout(req: &LayoutRequest, candidates: &[ImageSpec]) -> LayoutPlan {
    let (mw, mh) = req.monitor_px;
    let axis = if mw >= mh {
        SplitAxis::Horizontal
    } else {
        SplitAxis::Vertical
    };

    let border_px = req.border_px.clamp(0, 8);
    let max_scale = req.max_scale.clamp(1.00, 2.50);

    match req.mode {
        LayoutMode::Fixed(n) => {
            let count = n.clamp(1, 3);
            let frames_for_n = build_frames_for_count(count, &req.frames, candidates);
            evaluate_plan(
                req.monitor_px,
                axis,
                border_px,
                max_scale,
                count,
                frames_for_n,
            )
        }
        LayoutMode::Auto => {
            // Count pinned frames that have an image
            let pinned_with_image = req
                .frames
                .iter()
                .filter(|f| f.pinned && f.image.is_some())
                .count() as u8;

            let min_n = pinned_with_image.max(1);
            let max_n = 3u8;

            // Total available distinct images (images in req + candidates)
            let mut available_count = 0usize;
            let mut seen_ids = std::collections::HashSet::new();
            for f in &req.frames {
                if let Some(img) = &f.image
                    && seen_ids.insert(img.id.clone())
                {
                    available_count += 1;
                }
            }
            for c in candidates {
                if seen_ids.insert(c.id.clone()) {
                    available_count += 1;
                }
            }

            // Candidates list: N in min_n..=max_n
            // Exclude N > min_n if available_count < N
            let mut candidate_ns = Vec::new();
            for n in min_n..=max_n {
                if n > min_n && available_count < (n as usize) {
                    continue;
                }
                candidate_ns.push(n);
            }

            if candidate_ns.is_empty() {
                candidate_ns.push(min_n);
            }

            // Rule 2: If P <= 1 and 1-frame candidate completely fills slot (fg_dest == slot),
            // immediately choose N = 1.
            if pinned_with_image <= 1 && candidate_ns.contains(&1) {
                let frames_1 = build_frames_for_count(1, &req.frames, candidates);
                let plan_1 = evaluate_plan(req.monitor_px, axis, border_px, max_scale, 1, frames_1);
                if let Some(f0) = plan_1.frames.first()
                    && let Some(c0) = &f0.content
                    && c0.fg_dest == f0.slot
                {
                    return plan_1;
                }
            }

            // Rule 3: Evaluate all candidate Ns
            let mut evaluated: Vec<(u8, LayoutPlan, f64, f64)> = Vec::new(); // (N, plan, coverage, visible)

            for &n in &candidate_ns {
                let frames_n = build_frames_for_count(n, &req.frames, candidates);
                let plan = evaluate_plan(req.monitor_px, axis, border_px, max_scale, n, frames_n);

                let total_slot_area: f64 = plan
                    .frames
                    .iter()
                    .map(|f| (f.slot.width as f64) * (f.slot.height as f64))
                    .sum();
                let total_fg_area: f64 = plan
                    .frames
                    .iter()
                    .filter_map(|f| f.content.as_ref())
                    .map(|c| (c.fg_dest.width as f64) * (c.fg_dest.height as f64))
                    .sum();

                let coverage = if total_slot_area > 0.0 {
                    total_fg_area / total_slot_area
                } else {
                    0.0
                };

                let mut visible_sum = 0.0;
                let mut visible_count = 0usize;
                for f in &plan.frames {
                    if let Some(c) = &f.content {
                        if let Some(orig_spec) =
                            find_orig_spec(c.id.clone(), &req.frames, candidates)
                        {
                            let orig_area =
                                (orig_spec.size_px.0 as f64) * (orig_spec.size_px.1 as f64);
                            if orig_area > 0.0 {
                                visible_sum += (c.fg_src.width * c.fg_src.height) / orig_area;
                            }
                        }
                        visible_count += 1;
                    }
                }
                let visible = if visible_count > 0 {
                    visible_sum / (visible_count as f64)
                } else {
                    0.0
                };

                evaluated.push((n, plan, coverage, visible));
            }

            // Rule 4:
            // Filter candidates within 0.01 of max coverage
            let max_coverage = evaluated
                .iter()
                .map(|e| e.2)
                .fold(f64::NEG_INFINITY, f64::max);
            let coverage_filtered: Vec<_> = evaluated
                .into_iter()
                .filter(|e| e.2 >= max_coverage - 0.01)
                .collect();

            // Filter within 0.05 of max visible among remaining
            let max_visible = coverage_filtered
                .iter()
                .map(|e| e.3)
                .fold(f64::NEG_INFINITY, f64::max);
            let mut final_candidates: Vec<_> = coverage_filtered
                .into_iter()
                .filter(|e| e.3 >= max_visible - 0.05)
                .collect();

            // Select smallest N
            final_candidates.sort_by_key(|e| e.0);
            final_candidates.remove(0).1
        }
    }
}

fn find_orig_spec(
    id: ImageId,
    req_frames: &[FrameInput],
    candidates: &[ImageSpec],
) -> Option<ImageSpec> {
    for f in req_frames {
        if let Some(img) = &f.image
            && img.id == id
        {
            return Some(img.clone());
        }
    }
    for c in candidates {
        if c.id == id {
            return Some(c.clone());
        }
    }
    None
}

fn build_frames_for_count(
    n: u8,
    existing: &[FrameInput],
    candidates: &[ImageSpec],
) -> Vec<FrameInput> {
    let n = n as usize;
    let mut selected: Vec<FrameInput> = Vec::new();

    if existing.len() > n {
        // Prioritize pinned frames, fill remainder from unpinned from front.
        // Maintain relative order among kept frames.
        let mut pinned_indices = Vec::new();
        let mut unpinned_indices = Vec::new();

        for (i, f) in existing.iter().enumerate() {
            if f.pinned {
                pinned_indices.push(i);
            } else {
                unpinned_indices.push(i);
            }
        }

        let mut kept_indices: Vec<usize> = Vec::new();
        if pinned_indices.len() >= n {
            kept_indices.extend(&pinned_indices[..n]);
        } else {
            kept_indices.extend(&pinned_indices);
            let need = n - kept_indices.len();
            kept_indices.extend(&unpinned_indices[..need.min(unpinned_indices.len())]);
        }

        kept_indices.sort_unstable();
        for &idx in &kept_indices {
            selected.push(existing[idx].clone());
        }
    } else {
        selected.extend(existing.iter().cloned());
    }

    // Now fill missing or empty slots up to n
    let mut used_ids = std::collections::HashSet::new();
    for f in &selected {
        if let Some(img) = &f.image {
            used_ids.insert(img.id.clone());
        }
    }

    // Ensure selected has length n
    while selected.len() < n {
        selected.push(FrameInput {
            image: None,
            pinned: false,
            pan: (0.5, 0.5),
        });
    }

    for frame in &mut selected {
        if frame.image.is_none()
            && let Some(cand) = candidates.iter().find(|c| !used_ids.contains(&c.id))
        {
            frame.image = Some(cand.clone());
            used_ids.insert(cand.id.clone());
        }
    }

    selected
}

fn evaluate_plan(
    monitor_px: (u32, u32),
    axis: SplitAxis,
    border_px: u32,
    max_scale: f64,
    count: u8,
    frame_inputs: Vec<FrameInput>,
) -> LayoutPlan {
    let (mw, mh) = monitor_px;
    let n = count as usize;

    let slots = calculate_slots(mw, mh, axis, border_px, count);

    let mut frame_plans = Vec::with_capacity(n);
    let mut total_slot_area = 0.0;
    let mut total_fg_area = 0.0;

    for (i, slot) in slots.into_iter().enumerate() {
        let input = frame_inputs.get(i).cloned().unwrap_or(FrameInput {
            image: None,
            pinned: false,
            pan: (0.5, 0.5),
        });

        let slot_area = (slot.width as f64) * (slot.height as f64);
        total_slot_area += slot_area;

        let (placed, pan_out) = if let Some(spec) = input.image {
            let (placed_img, effective_pan) = place_image(&spec, slot, max_scale, input.pan);
            let fg_area = (placed_img.fg_dest.width as f64) * (placed_img.fg_dest.height as f64);
            total_fg_area += fg_area;
            (Some(placed_img), effective_pan)
        } else {
            (None, (0.5, 0.5))
        };

        frame_plans.push(FramePlan {
            slot,
            content: placed,
            pinned: input.pinned,
            pan: pan_out,
        });
    }

    let coverage = if total_slot_area > 0.0 {
        total_fg_area / total_slot_area
    } else {
        0.0
    };

    LayoutPlan {
        monitor_px,
        resolved_count: count,
        axis,
        frames: frame_plans,
        coverage,
    }
}

pub fn calculate_slots(
    mw: u32,
    mh: u32,
    axis: SplitAxis,
    border_px: u32,
    count: u8,
) -> Vec<RectPx> {
    let n = count as u32;
    if n == 0 {
        return vec![];
    }

    let total_border = border_px.saturating_mul(n - 1);
    let mut slots = Vec::with_capacity(n as usize);

    match axis {
        SplitAxis::Horizontal => {
            let rem_w = mw.saturating_sub(total_border);
            let base_w = rem_w / n;
            let remainder = rem_w % n;

            let mut cur_x = 0;
            for i in 0..n {
                let w = base_w + if i < remainder { 1 } else { 0 };
                slots.push(RectPx {
                    x: cur_x,
                    y: 0,
                    width: w,
                    height: mh,
                });
                cur_x += (w + border_px) as i32;
            }
        }
        SplitAxis::Vertical => {
            let rem_h = mh.saturating_sub(total_border);
            let base_h = rem_h / n;
            let remainder = rem_h % n;

            let mut cur_y = 0;
            for i in 0..n {
                let h = base_h + if i < remainder { 1 } else { 0 };
                slots.push(RectPx {
                    x: 0,
                    y: cur_y,
                    width: mw,
                    height: h,
                });
                cur_y += (h + border_px) as i32;
            }
        }
    }

    slots
}

fn place_image(
    spec: &ImageSpec,
    slot: RectPx,
    max_scale: f64,
    pan: (f64, f64),
) -> (PlacedImage, (f64, f64)) {
    let (iw, ih) = (spec.size_px.0 as f64, spec.size_px.1 as f64);
    let (sw, sh) = (slot.width as f64, slot.height as f64);

    let fit = (sw / iw).min(sh / ih);
    let cover = (sw / iw).max(sh / ih);
    let crop_limit = fit / MIN_VISIBLE_RATIO;
    let scale = cover.min(max_scale).min(crop_limit);

    let dw = iw * scale;
    let dh = ih * scale;

    let scale_limited = if scale < cover - 1e-6 {
        if max_scale <= crop_limit + 1e-6 && (scale - max_scale).abs() < 1e-6 {
            ScaleLimit::MaxScale
        } else {
            ScaleLimit::CropLimit
        }
    } else {
        ScaleLimit::None
    };

    // Pan clamping to [0, 1]
    let pan_x = pan.0.clamp(0.0, 1.0);
    let pan_y = pan.1.clamp(0.0, 1.0);

    // X axis calculation
    let (fg_src_x, fg_src_w, fg_dest_x, fg_dest_w, can_pan_x, effective_pan_x) = if dw >= sw {
        let can_pan = dw > sw;
        let src_len = iw.min(sw / scale);
        let max_src_pos = (iw - src_len).max(0.0);
        let src_pos = (max_src_pos * pan_x).clamp(0.0, max_src_pos);
        let dest_x = slot.x;
        let dest_w = slot.width;
        let eff_pan = if can_pan { pan_x } else { 0.5 };
        (src_pos, src_len, dest_x, dest_w, can_pan, eff_pan)
    } else {
        let can_pan = false;
        let dest_len = (dw.round() as i64).clamp(1, slot.width as i64) as u32;
        let dest_pos = ((slot.width as i64 - dest_len as i64) / 2) as i32;
        let dest_x = slot.x + dest_pos;
        let dest_w = dest_len;
        (0.0, iw, dest_x, dest_w, can_pan, 0.5)
    };

    // Y axis calculation
    let (fg_src_y, fg_src_h, fg_dest_y, fg_dest_h, can_pan_y, effective_pan_y) = if dh >= sh {
        let can_pan = dh > sh;
        let src_len = ih.min(sh / scale);
        let max_src_pos = (ih - src_len).max(0.0);
        let src_pos = (max_src_pos * pan_y).clamp(0.0, max_src_pos);
        let dest_y = slot.y;
        let dest_h = slot.height;
        let eff_pan = if can_pan { pan_y } else { 0.5 };
        (src_pos, src_len, dest_y, dest_h, can_pan, eff_pan)
    } else {
        let can_pan = false;
        let dest_len = (dh.round() as i64).clamp(1, slot.height as i64) as u32;
        let dest_pos = ((slot.height as i64 - dest_len as i64) / 2) as i32;
        let dest_y = slot.y + dest_pos;
        let dest_h = dest_len;
        (0.0, ih, dest_y, dest_h, can_pan, 0.5)
    };

    // Background blur: ONLY when margins remain in the slot (Dw < Sw || Dh < Sh)
    let (bg_src, blur_sigma) = if dw < sw || dh < sh {
        let bg_len_x = iw.min(sw / cover);
        let bg_pos_x = (iw - bg_len_x) / 2.0;
        let bg_len_y = ih.min(sh / cover);
        let bg_pos_y = (ih - bg_len_y) / 2.0;
        let sigma = 24.0 * (sw.min(sh)) / 1080.0;
        (
            RectF {
                x: bg_pos_x,
                y: bg_pos_y,
                width: bg_len_x,
                height: bg_len_y,
            },
            sigma,
        )
    } else {
        (
            RectF {
                x: 0.0,
                y: 0.0,
                width: 0.0,
                height: 0.0,
            },
            0.0,
        )
    };

    let placed = PlacedImage {
        id: spec.id.clone(),
        fg_src: RectF {
            x: fg_src_x,
            y: fg_src_y,
            width: fg_src_w,
            height: fg_src_h,
        },
        fg_dest: RectPx {
            x: fg_dest_x,
            y: fg_dest_y,
            width: fg_dest_w,
            height: fg_dest_h,
        },
        bg_src,
        blur_sigma,
        scale,
        can_pan: (can_pan_x, can_pan_y),
        scale_limited,
    };

    (placed, (effective_pan_x, effective_pan_y))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_example_1_4k_fhd_m125() {
        // Spec 3.2 Example 1: 4K (3840x2160) split 2 (1919x2160 with border 2px)
        // FHD image (1920x1080), M = 1.25
        let req = LayoutRequest {
            monitor_px: (3840, 2160),
            mode: LayoutMode::Fixed(2),
            max_scale: 1.25,
            border_px: 2,
            frames: vec![
                FrameInput {
                    image: Some(ImageSpec {
                        id: ImageId("fhd.jpg".into()),
                        size_px: (1920, 1080),
                    }),
                    pinned: false,
                    pan: (0.5, 0.5),
                },
                FrameInput {
                    image: None,
                    pinned: false,
                    pan: (0.5, 0.5),
                },
            ],
        };

        let plan = plan_layout(&req, &[]);
        assert_eq!(plan.frames.len(), 2);
        assert_eq!(plan.frames[0].slot.width, 1919);
        assert_eq!(plan.frames[0].slot.height, 2160);
        assert_eq!(plan.frames[1].slot.width, 1919);
        assert_eq!(plan.frames[1].slot.height, 2160);

        let content = plan.frames[0].content.as_ref().unwrap();
        // cover = max(1919/1920, 2160/1080) = 2.0
        // crop_limit = fit / 0.6 = (1919/1920) / 0.6 = 1.66579
        // scale = min(2.0, 1.25, 1.66579) = 1.25
        assert!((content.scale - 1.25).abs() < 1e-4);
        assert_eq!(content.scale_limited, ScaleLimit::MaxScale);
        // Dw = 1920 * 1.25 = 2400 >= 1919 (X fills slot)
        assert_eq!(content.fg_dest.width, 1919);
        // Dh = 1080 * 1.25 = 1350 < 2160 (Y has margins)
        assert_eq!(content.fg_dest.height, 1350);
        // Margins exist, so background blur is active
        assert!(content.blur_sigma > 0.0);
    }

    #[test]
    fn test_example_2_4k_tall_auto_mode() {
        // Spec 3.2 Example 2 & 10.1: 4K (3840x2160) with tall image (2400x3600)
        // 1 frame crop_limit preserves >= 60% visible area, auto chooses 3 frames
        let tall_img = ImageSpec {
            id: ImageId("tall.jpg".into()),
            size_px: (2400, 3600),
        };
        let c1 = ImageSpec {
            id: ImageId("cand1.jpg".into()),
            size_px: (2400, 3600),
        };
        let c2 = ImageSpec {
            id: ImageId("cand2.jpg".into()),
            size_px: (2400, 3600),
        };

        let req = LayoutRequest {
            monitor_px: (3840, 2160),
            mode: LayoutMode::Auto,
            max_scale: 1.25,
            border_px: 2,
            frames: vec![FrameInput {
                image: Some(tall_img.clone()),
                pinned: false,
                pan: (0.5, 0.5),
            }],
        };

        // When evaluating with 3 candidates available
        let plan = plan_layout(&req, &[c1, c2]);
        assert_eq!(plan.resolved_count, 3);
    }

    #[test]
    fn test_slot_remainder_distribution() {
        // Remainder distribution: first remainder slots get +1px
        let slots = calculate_slots(10, 10, SplitAxis::Horizontal, 0, 3);
        assert_eq!(slots.len(), 3);
        // 10 / 3 = 3 rem 1 => widths 4, 3, 3
        assert_eq!(slots[0].width, 4);
        assert_eq!(slots[1].width, 3);
        assert_eq!(slots[2].width, 3);
        assert_eq!(slots[0].x, 0);
        assert_eq!(slots[1].x, 4);
        assert_eq!(slots[2].x, 7);
    }

    #[test]
    fn test_no_gap_when_d_ge_s() {
        // Acceptance 10.1: D >= S has no gap, fg_dest == [0, S]
        let req = LayoutRequest {
            monitor_px: (1920, 1080),
            mode: LayoutMode::Fixed(1),
            max_scale: 1.5,
            border_px: 0,
            frames: vec![FrameInput {
                image: Some(ImageSpec {
                    id: ImageId("test.jpg".into()),
                    size_px: (1920, 1080),
                }),
                pinned: false,
                pan: (0.5, 0.5),
            }],
        };
        let plan = plan_layout(&req, &[]);
        let c = plan.frames[0].content.as_ref().unwrap();
        assert_eq!(c.fg_dest.x, 0);
        assert_eq!(c.fg_dest.y, 0);
        assert_eq!(c.fg_dest.width, 1920);
        assert_eq!(c.fg_dest.height, 1080);
    }

    #[test]
    fn test_pinned_frame_preserved_on_reduction() {
        // Frame reduction from 3 to 2 preserves pinned frame
        let f1 = FrameInput {
            image: Some(ImageSpec {
                id: ImageId("1.jpg".into()),
                size_px: (100, 100),
            }),
            pinned: false,
            pan: (0.5, 0.5),
        };
        let f2 = FrameInput {
            image: Some(ImageSpec {
                id: ImageId("2.jpg".into()),
                size_px: (100, 100),
            }),
            pinned: true,
            pan: (0.2, 0.2),
        };
        let f3 = FrameInput {
            image: Some(ImageSpec {
                id: ImageId("3.jpg".into()),
                size_px: (100, 100),
            }),
            pinned: false,
            pan: (0.5, 0.5),
        };

        let req = LayoutRequest {
            monitor_px: (1920, 1080),
            mode: LayoutMode::Fixed(2),
            max_scale: 1.25,
            border_px: 0,
            frames: vec![f1, f2, f3],
        };

        let plan = plan_layout(&req, &[]);
        assert_eq!(plan.frames[0].content.as_ref().unwrap().id.0, "1.jpg");
        assert_eq!(plan.frames[1].content.as_ref().unwrap().id.0, "2.jpg");
        assert!(plan.frames[1].pinned);
    }

    #[test]
    fn test_vertical_split() {
        // Vertical monitor W < H: 1080x1920
        let req = LayoutRequest {
            monitor_px: (1080, 1920),
            mode: LayoutMode::Fixed(2),
            max_scale: 1.25,
            border_px: 4,
            frames: vec![],
        };
        let plan = plan_layout(&req, &[]);
        assert_eq!(plan.axis, SplitAxis::Vertical);
        assert_eq!(plan.frames.len(), 2);
        assert_eq!(plan.frames[0].slot.x, 0);
        assert_eq!(plan.frames[0].slot.width, 1080);
        // (1920 - 4) / 2 = 958
        assert_eq!(plan.frames[0].slot.height, 958);
        assert_eq!(plan.frames[1].slot.y, 962);
        assert_eq!(plan.frames[1].slot.height, 958);
    }

    #[test]
    fn test_pan_and_can_pan() {
        let req = LayoutRequest {
            monitor_px: (1000, 1000),
            mode: LayoutMode::Fixed(1),
            max_scale: 2.0,
            border_px: 0,
            frames: vec![FrameInput {
                image: Some(ImageSpec {
                    id: ImageId("wide.jpg".into()),
                    size_px: (2000, 1000),
                }),
                pinned: false,
                pan: (0.75, 0.5),
            }],
        };
        let plan = plan_layout(&req, &[]);
        let c = plan.frames[0].content.as_ref().unwrap();
        // fit = 0.5, cover = 1.0, crop_limit = 0.5/0.6 = 0.8333
        // scale = min(1.0, 2.0, 0.8333) = 0.8333
        // Dw = 2000 * 0.8333 = 1666.6 >= 1000 (can pan X!)
        // Dh = 1000 * 0.8333 = 833.3 < 1000 (cannot pan Y, margin)
        assert!(c.can_pan.0);
        assert!(!c.can_pan.1);
    }

    #[test]
    fn test_insufficient_candidates_auto_mode() {
        // Only 1 candidate available, auto mode should not pick N=2 or 3
        let img1 = ImageSpec {
            id: ImageId("only_one.jpg".into()),
            size_px: (2400, 3600),
        };
        let req = LayoutRequest {
            monitor_px: (3840, 2160),
            mode: LayoutMode::Auto,
            max_scale: 1.25,
            border_px: 2,
            frames: vec![FrameInput {
                image: Some(img1),
                pinned: false,
                pan: (0.5, 0.5),
            }],
        };
        let plan = plan_layout(&req, &[]);
        assert_eq!(plan.resolved_count, 1);
    }
}
