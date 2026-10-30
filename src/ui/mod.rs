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
            dpi,
        );
        // Should have 2 folder headers + 2 root items = 4 rows
        assert_eq!(l2.rows.len(), 4);
        assert!(l2.total_content_height < h1_total);
    }
}
