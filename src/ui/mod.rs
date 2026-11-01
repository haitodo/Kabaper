pub mod layout_calc;
pub mod render;
pub mod state;
pub mod types;
pub mod window;

pub use layout_calc::*;
pub use render::*;
pub use state::*;
pub use types::*;
pub use window::*;

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;
    use std::path::PathBuf;
    use crate::catalog::{Catalog, CatalogItem};

    #[test]
    fn test_filmstrip_layout() {
        let (strip_y, strip_h, item_w, start, end) =
            get_filmstrip_layout(1000, 800, 5, 20, 96);
        assert_eq!(strip_h, 92);
        assert_eq!(strip_y, 800 - 92);
        assert!(item_w > 0);
        assert!(start <= 5);
        assert!(end > 5);
        assert!(end <= 20);
    }

    #[test]
    fn test_canvas_layout_with_sidebar() {
        let dpi = 96;
        let win_w = 1200;
        let win_h = 800;
        let bar_h = 45;

        // No sidebar, no filmstrip
        let c1 = get_canvas_layout(win_w, win_h, bar_h, false, false, dpi);
        assert_eq!(c1.x, 0);
        assert_eq!(c1.y, bar_h);
        assert_eq!(c1.w, win_w);
        assert_eq!(c1.h, win_h - bar_h);

        // Sidebar active
        let sidebar_w = get_sidebar_width(dpi);
        let c2 = get_canvas_layout(win_w, win_h, bar_h, true, false, dpi);
        assert_eq!(c2.x, sidebar_w);
        assert_eq!(c2.y, bar_h);
        assert_eq!(c2.w, win_w - sidebar_w);
        assert_eq!(c2.h, win_h - bar_h);

        // Both sidebar and filmstrip active
        let strip_h = 92;
        let c3 = get_canvas_layout(win_w, win_h, bar_h, true, true, dpi);
        assert_eq!(c3.x, sidebar_w);
        assert_eq!(c3.y, bar_h);
        assert_eq!(c3.w, win_w - sidebar_w);
        assert_eq!(c3.h, win_h - bar_h - strip_h);
    }

    #[test]
    fn test_sidebar_layout_folder_grouping_and_collapse() {
        let dpi = 96;
        let root = PathBuf::from("C:\\test_photos");
        let sub = root.join("2026_trip");

        let mut catalog = Catalog::new(HashSet::new());
        catalog.folder = Some(root.clone());
        catalog.items = vec![
            CatalogItem {
                path: root.join("img_root_1.jpg"),
                filename: "img_root_1.jpg".into(),
                is_excluded: false,
                failure_reason: None,
            },
            CatalogItem {
                path: root.join("img_root_2.jpg"),
                filename: "img_root_2.jpg".into(),
                is_excluded: false,
                failure_reason: None,
            },
            CatalogItem {
                path: sub.join("trip_1.jpg"),
                filename: "trip_1.jpg".into(),
                is_excluded: false,
                failure_reason: None,
            },
            CatalogItem {
                path: sub.join("trip_2.jpg"),
                filename: "trip_2.jpg".into(),
                is_excluded: false,
                failure_reason: None,
            },
        ];

        let pinned = HashSet::new();
        let mut collapsed = HashSet::new();

        // 1. Expanded all
        let l1 = compute_sidebar_layout(
            &catalog,
            Some(0),
            &pinned,
            &collapsed,
            0,
            1200,
            800,
            45,
            false,
            types::ViewMode::Normal,
            1,
            0,
            dpi,
        );
        // Should have 2 folder headers + 4 file items = 6 rows
        assert_eq!(l1.rows.len(), 6);
        let h1_total = l1.total_content_height;

        // Check header buttons layout order (focus < collapse_all < close)
        assert!(l1.collapse_all_btn_rect.left < l1.close_btn_rect.left);
        assert!(l1.focus_current_btn_rect.left < l1.collapse_all_btn_rect.left);

        // 2. Collapse the subfolder
        collapsed.insert(sub.clone());
        let l2 = compute_sidebar_layout(
            &catalog,
            Some(0),
            &pinned,
            &collapsed,
            0,
            1200,
            800,
            45,
            false,
            types::ViewMode::Normal,
            1,
            0,
            dpi,
        );
        // Should have 2 folder headers + 2 root items = 4 rows
        assert_eq!(l2.rows.len(), 4);
        assert!(l2.total_content_height < h1_total);
    }

    #[test]
    fn test_compute_sidebar_layout_composite_frames() {
        let root = PathBuf::from("C:\\test_photos");
        let item1 = CatalogItem {
            path: root.join("img1.jpg"),
            filename: "img1.jpg".into(),
            is_excluded: false,
            failure_reason: None,
        };
        let item2 = CatalogItem {
            path: root.join("img2.jpg"),
            filename: "img2.jpg".into(),
            is_excluded: false,
            failure_reason: None,
        };

        let mut catalog = Catalog::new(HashSet::new());
        catalog.items = vec![item1, item2];
        let pinned = HashSet::new();
        let collapsed = HashSet::new();
        let dpi = 96;

        // Composite mode with 2 frames
        let layout_2f = compute_sidebar_layout(
            &catalog,
            Some(0),
            &pinned,
            &collapsed,
            0,
            1200,
            800,
            45,
            false,
            types::ViewMode::Composite,
            2,
            1, // frame 1 is selected
            dpi,
        );

        // Check target frame bar exists
        assert!(layout_2f.target_frame_bar.is_some());
        let (bar_rect, buttons) = layout_2f.target_frame_bar.unwrap();
        assert_eq!(buttons.len(), 2);
        assert_eq!(buttons[0].1, "左枠");
        assert_eq!(buttons[0].3, false); // frame 0 is not selected
        assert_eq!(buttons[1].1, "右枠");
        assert_eq!(buttons[1].3, true); // frame 1 is selected
        assert!(layout_2f.list_rect.top >= bar_rect.bottom);

        // Check rows have frame_targets buttons
        let file_row = layout_2f
            .rows
            .iter()
            .find(|r| matches!(&r.kind, SidebarRowKind::FileItem { .. }))
            .unwrap();
        if let SidebarRowKind::FileItem { frame_targets, .. } = &file_row.kind {
            assert_eq!(frame_targets.len(), 2);
            assert_eq!(frame_targets[0].1, "左");
            assert_eq!(frame_targets[1].1, "右");
        } else {
            panic!("Expected FileItem row");
        }
    }

    #[test]
    fn test_shortcuts_modal_layout_and_close_button_hit() {
        let win_w = 1920;
        let win_h = 1080;
        let dpi = 96;

        let layout = get_shortcuts_modal_layout(win_w, win_h, dpi);
        let card = layout.card_rect;
        let close_btn = layout.close_btn_rect;

        // Card dimensions (480 x 680 at 96 dpi)
        assert_eq!(card.right - card.left, 480);
        assert_eq!(card.bottom - card.top, 680);
        assert_eq!(card.left, (win_w - 480) / 2);
        assert_eq!(card.top, (win_h - 680) / 2);

        // Close button must be completely inside the card bounds
        assert!(close_btn.left >= card.left);
        assert!(close_btn.right <= card.right);
        assert!(close_btn.top >= card.top);
        assert!(close_btn.bottom <= card.bottom);

        // Close button dimensions (120 x 34 at 96 dpi)
        assert_eq!(close_btn.right - close_btn.left, 120);
        assert_eq!(close_btn.bottom - close_btn.top, 34);

        // Center of close button must hit
        let cx = (close_btn.left + close_btn.right) / 2;
        let cy = (close_btn.top + close_btn.bottom) / 2;
        assert!(rect_contains(&close_btn, cx, cy));

        // Edges of close button must hit
        assert!(rect_contains(&close_btn, close_btn.left, close_btn.top));
        assert!(rect_contains(&close_btn, close_btn.right, close_btn.bottom));

        // Area slightly above the close button must NOT hit
        assert!(!rect_contains(&close_btn, cx, close_btn.top - 5));
        assert!(!rect_contains(&close_btn, cx, close_btn.top - 20));

        // Area slightly below the close button must NOT hit
        assert!(!rect_contains(&close_btn, cx, close_btn.bottom + 5));
    }

    #[test]
    fn test_modal_dialog_layouts_contain_close_buttons() {
        for dpi in [96, 120, 144, 192] {
            let win_w = 1920;
            let win_h = 1080;

            let settings = get_settings_modal_layout(win_w, win_h, dpi);
            assert!(settings.close_btn_rect.left >= settings.card_rect.left);
            assert!(settings.close_btn_rect.right <= settings.card_rect.right);
            assert!(settings.close_btn_rect.top >= settings.card_rect.top);
            assert!(settings.close_btn_rect.bottom <= settings.card_rect.bottom);
            let s_cx = (settings.close_btn_rect.left + settings.close_btn_rect.right) / 2;
            let s_cy = (settings.close_btn_rect.top + settings.close_btn_rect.bottom) / 2;
            assert!(rect_contains(&settings.close_btn_rect, s_cx, s_cy));

            let exclude = get_exclude_list_modal_layout(win_w, win_h, dpi);
            assert!(exclude.close_btn_rect.left >= exclude.card_rect.left);
            assert!(exclude.close_btn_rect.right <= exclude.card_rect.right);
            assert!(exclude.close_btn_rect.top >= exclude.card_rect.top);
            assert!(exclude.close_btn_rect.bottom <= exclude.card_rect.bottom);
            let e_cx = (exclude.close_btn_rect.left + exclude.close_btn_rect.right) / 2;
            let e_cy = (exclude.close_btn_rect.top + exclude.close_btn_rect.bottom) / 2;
            assert!(rect_contains(&exclude.close_btn_rect, e_cx, e_cy));
        }
    }

    #[test]
    fn test_sidebar_scrollbar_hit_area() {
        use windows::Win32::Foundation::RECT;
        let s_rect = RECT {
            left: 285,
            top: 50,
            right: 295,
            bottom: 450,
        };
        let list_rect = RECT {
            left: 0,
            top: 50,
            right: 300,
            bottom: 450,
        };
        // Inside thumb/bar
        assert!(is_in_scrollbar_hit_area(290, 100, s_rect, list_rect, 96));
        // Hit margin to the left of the bar
        assert!(is_in_scrollbar_hit_area(278, 100, s_rect, list_rect, 96));
        // Far left of hit margin -> false
        assert!(!is_in_scrollbar_hit_area(200, 100, s_rect, list_rect, 96));
        // Above or below
        assert!(!is_in_scrollbar_hit_area(290, 40, s_rect, list_rect, 96));
        assert!(!is_in_scrollbar_hit_area(290, 460, s_rect, list_rect, 96));
    }

    #[test]
    fn test_sidebar_wheel_fixed_multiline() {
        use std::fs;
        let temp = std::env::temp_dir().join(format!("kabaper_scroll_test_{}", std::process::id()));
        let _ = fs::create_dir_all(&temp);
        let storage = crate::store::Storage::init_with_dir(temp.clone()).unwrap();
        let initial_config = crate::store::AppConfig::default();
        let mut state = crate::ui::state::AppState::new(storage, initial_config, None).unwrap();

        let list_h = 400;
        let total_h = 2000;
        let dpi = 96;

        // 1 notch down (delta = -120): exactly 4 lines (44 * 4 = 176px)
        state.scroll_sidebar_wheel(-120, list_h, total_h, dpi);
        assert_eq!(state.sidebar_scroll_offset, 176);

        // 2nd notch down: linear without acceleration (176 * 2 = 352px)
        state.scroll_sidebar_wheel(-120, list_h, total_h, dpi);
        assert_eq!(state.sidebar_scroll_offset, 352);

        // 1 notch up: returns exactly 176px
        state.scroll_sidebar_wheel(120, list_h, total_h, dpi);
        assert_eq!(state.sidebar_scroll_offset, 176);

        let _ = fs::remove_dir_all(&temp);
    }

    #[test]
    fn test_sidebar_scrollbar_drag_and_track_click() {
        use std::fs;
        use windows::Win32::Foundation::RECT;
        let temp = std::env::temp_dir().join(format!("kabaper_drag_test_{}", std::process::id()));
        let _ = fs::create_dir_all(&temp);
        let storage = crate::store::Storage::init_with_dir(temp.clone()).unwrap();
        let initial_config = crate::store::AppConfig::default();
        let mut state = crate::ui::state::AppState::new(storage, initial_config, None).unwrap();

        let list_h = 400;
        let total_h = 2000; // max_scroll = 1600
        let s_rect = RECT {
            left: 288,
            top: 50,
            right: 296,
            bottom: 450, // usable_h = 400
        };
        let thumb_rect = RECT {
            left: 288,
            top: 50,
            right: 296,
            bottom: 130, // thumb_h = 80, travel = 320
        };

        // 1. Click thumb at top + 20px (y = 70)
        state.start_sidebar_scrollbar_drag(70, thumb_rect, s_rect, list_h, total_h);
        assert!(state.sidebar_scroll_dragging);
        assert_eq!(state.sidebar_drag_thumb_offset_y, 20);

        // Drag down by 160px (target_thumb_top = 230 - 20 = 210, ratio = (210 - 50) / 320 = 160/320 = 0.5)
        state.drag_sidebar_scrollbar(230, s_rect, 80, list_h, total_h);
        // Halfway of max_scroll (1600 * 0.5 = 800)
        assert_eq!(state.sidebar_scroll_offset, 800);

        // Drag past bottom (y = 500)
        state.drag_sidebar_scrollbar(500, s_rect, 80, list_h, total_h);
        assert_eq!(state.sidebar_scroll_offset, 1600);

        // Stop drag
        state.stop_sidebar_scrollbar_drag();
        assert!(!state.sidebar_scroll_dragging);

        // 2. Click track at y = 250 (middle of usable range)
        state.start_sidebar_scrollbar_drag(250, thumb_rect, s_rect, list_h, total_h);
        assert!(state.sidebar_scroll_dragging);
        // Should center thumb on 250 (thumb_top = 250 - 40 = 210, offset = 800)
        assert_eq!(state.sidebar_scroll_offset, 800);

        state.stop_sidebar_scrollbar_drag();
        let _ = fs::remove_dir_all(&temp);
    }

    fn create_test_bmp_sized(path: &std::path::Path, w: u32, h: u32) {
        let row_stride = ((w * 3 + 3) / 4) * 4;
        let data_size = row_stride * h;
        let file_size = 54 + data_size;
        let mut data = vec![0u8; file_size as usize];
        data[0] = 0x42;
        data[1] = 0x4D;
        data[2..6].copy_from_slice(&file_size.to_le_bytes());
        data[10..14].copy_from_slice(&54u32.to_le_bytes());
        data[14..18].copy_from_slice(&40u32.to_le_bytes());
        data[18..22].copy_from_slice(&w.to_le_bytes());
        data[22..26].copy_from_slice(&h.to_le_bytes());
        data[26..28].copy_from_slice(&1u16.to_le_bytes());
        data[28..30].copy_from_slice(&24u16.to_le_bytes());
        data[34..38].copy_from_slice(&data_size.to_le_bytes());
        std::fs::write(path, data).unwrap();
    }

    #[test]
    fn test_composite_scroll_does_not_corrupt_unselected_frame_size_or_cause_blur() {
        let temp = std::env::temp_dir().join(format!("kabaper_test_blur_repro_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&temp);
        std::fs::create_dir_all(&temp).unwrap();

        let p1 = temp.join("img1.bmp");
        let p2 = temp.join("img2.bmp");
        let p3 = temp.join("img3.bmp");
        create_test_bmp_sized(&p1, 1920, 1080);
        create_test_bmp_sized(&p2, 1920, 1080);
        create_test_bmp_sized(&p3, 1920, 1080);

        let storage_dir = temp.join("storage");
        let storage = crate::store::Storage::init_with_dir(storage_dir).unwrap();
        let mut state = AppState::new(storage, crate::store::AppConfig::default(), Some(&temp)).unwrap();

        state.view_mode = ViewMode::Composite;
        state.config.layout.mode = "split2".to_string();
        state.config.layout.border_px = 0;
        state.rebuild_layout(true, true);

        // Get monitor dimensions and slot dimensions
        let monitor = state.target_monitor().unwrap();
        let slot_w = monitor.width / 2;
        let slot_h = monitor.height;

        // Create images matching the exact slot dimension so there's zero margin (no blur)
        let p_match1 = temp.join("img_match1.bmp");
        let p_match2 = temp.join("img_match2.bmp");
        let p_match3 = temp.join("img_match3.bmp");
        create_test_bmp_sized(&p_match1, slot_w, slot_h);
        create_test_bmp_sized(&p_match2, slot_w, slot_h);
        create_test_bmp_sized(&p_match3, slot_w, slot_h);

        // Open directory with matching images
        state.catalog.open_folder(&temp, Some(&p_match1));
        state.rebuild_layout(true, true);

        // Initial check: Frame 0 has exact slot size, so blur_sigma must be 0.0
        let f0_init = state.active_plan.as_ref().unwrap().frames[0].content.as_ref().unwrap();
        assert_eq!(f0_init.blur_sigma, 0.0, "Initially Frame 0 has no blur");

        // Select frame 1 and scroll (next_image)
        state.selected_frame_idx = 1;
        state.next_image();

        // Frame 0 size must remain (slot_w, slot_h), NOT corrupted to (1, 1)
        let f0_size_after = state.layout_frames[0].image.as_ref().unwrap().size_px;
        assert_eq!(f0_size_after, (slot_w, slot_h), "Frame 0 size_px must remain intact");

        // Frame 0 must STILL have NO blur (blur_sigma == 0.0)
        let plan = state.active_plan.as_ref().unwrap();
        let f0_content = plan.frames[0].content.as_ref().unwrap();
        assert_eq!(f0_content.blur_sigma, 0.0, "Frame 0 must NOT have blur after scrolling frame 1");
        assert_eq!(f0_content.fg_dest.width, slot_w, "Frame 0 fg_dest must fill slot width");
        assert_eq!(f0_content.fg_dest.height, slot_h, "Frame 0 fg_dest must fill slot height");

        let _ = std::fs::remove_dir_all(&temp);
    }
}
