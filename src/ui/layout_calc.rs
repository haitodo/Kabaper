use std::collections::HashSet;
use std::path::PathBuf;

use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::core::*;

use crate::catalog::Catalog;
use crate::ui::types::{BarButtonId, FrameOverlayButtonId, ViewMode};

pub fn get_window_dpi(hwnd: HWND) -> u32 {
    unsafe {
        let dpi = GetDpiForWindow(hwnd);
        if dpi == 0 {
            96
        } else {
            dpi
        }
    }
}

#[inline]
pub fn scale_by_dpi(value: i32, dpi: u32) -> i32 {
    let effective_dpi = if dpi == 0 { 96 } else { dpi };
    (value * effective_dpi as i32 + 48) / 96
}

#[inline]
pub fn rect_contains(rect: &RECT, x: i32, y: i32) -> bool {
    x >= rect.left && x <= rect.right && y >= rect.top && y <= rect.bottom
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ModalCardLayout {
    pub card_rect: RECT,
    pub close_btn_rect: RECT,
}

pub fn get_shortcuts_modal_layout(win_w: i32, win_h: i32, dpi: u32) -> ModalCardLayout {
    let box_w = scale_by_dpi(480, dpi);
    let box_h = scale_by_dpi(680, dpi);
    let bx = (win_w - box_w) / 2;
    let by = (win_h - box_h) / 2;

    let close_w = scale_by_dpi(120, dpi);
    let close_h = scale_by_dpi(34, dpi);
    let close_x = bx + (box_w - close_w) / 2;
    let close_y = by + scale_by_dpi(625, dpi);

    ModalCardLayout {
        card_rect: RECT {
            left: bx,
            top: by,
            right: bx + box_w,
            bottom: by + box_h,
        },
        close_btn_rect: RECT {
            left: close_x,
            top: close_y,
            right: close_x + close_w,
            bottom: close_y + close_h,
        },
    }
}

pub fn get_settings_modal_layout(win_w: i32, win_h: i32, dpi: u32) -> ModalCardLayout {
    let box_w = scale_by_dpi(520, dpi);
    let box_h = scale_by_dpi(430, dpi);
    let bx = (win_w - box_w) / 2;
    let by = (win_h - box_h) / 2;

    let close_w = scale_by_dpi(120, dpi);
    let close_h = scale_by_dpi(34, dpi);
    let close_x = bx + (box_w - close_w) / 2;
    let close_y = by + scale_by_dpi(375, dpi);

    ModalCardLayout {
        card_rect: RECT {
            left: bx,
            top: by,
            right: bx + box_w,
            bottom: by + box_h,
        },
        close_btn_rect: RECT {
            left: close_x,
            top: close_y,
            right: close_x + close_w,
            bottom: close_y + close_h,
        },
    }
}

pub fn get_exclude_list_modal_layout(win_w: i32, win_h: i32, dpi: u32) -> ModalCardLayout {
    let box_w = scale_by_dpi(540, dpi);
    let box_h = scale_by_dpi(440, dpi);
    let bx = (win_w - box_w) / 2;
    let by = (win_h - box_h) / 2;

    let close_w = scale_by_dpi(120, dpi);
    let close_h = scale_by_dpi(34, dpi);
    let close_x = bx + (box_w - close_w) / 2;
    let close_y = by + scale_by_dpi(380, dpi);

    ModalCardLayout {
        card_rect: RECT {
            left: bx,
            top: by,
            right: bx + box_w,
            bottom: by + box_h,
        },
        close_btn_rect: RECT {
            left: close_x,
            top: close_y,
            right: close_x + close_w,
            bottom: close_y + close_h,
        },
    }
}

pub fn get_bar_height(dpi: u32) -> i32 {
    scale_by_dpi(45, dpi)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CanvasLayout {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
}

#[inline]
pub fn get_sidebar_width(dpi: u32) -> i32 {
    scale_by_dpi(280, dpi)
}

pub fn get_canvas_layout(
    win_w: i32,
    win_h: i32,
    bar_h: i32,
    show_sidebar: bool,
    show_filmstrip: bool,
    dpi: u32,
) -> CanvasLayout {
    let sidebar_w = if show_sidebar { get_sidebar_width(dpi) } else { 0 };
    let strip_h = if show_filmstrip { scale_by_dpi(92, dpi) } else { 0 };
    let x = sidebar_w;
    let y = bar_h;
    let w = (win_w - sidebar_w).max(1);
    let h = (win_h - bar_h - strip_h).max(1);
    CanvasLayout { x, y, w, h }
}

pub fn get_operation_bar_buttons(dpi: u32) -> Vec<(BarButtonId, RECT)> {
    let pad_x = scale_by_dpi(10, dpi);
    let gap = scale_by_dpi(6, dpi);
    let btn_y = scale_by_dpi(6, dpi);
    let btn_h = scale_by_dpi(32, dpi);

    let defs = [
        (BarButtonId::Open, 76),
        (BarButtonId::Sidebar, 76),
        (BarButtonId::ViewMode, 68),
        (BarButtonId::Monitor, 78),
        (BarButtonId::Split, 88),
        (BarButtonId::Scale, 82),
        (BarButtonId::Apply, 68),
        (BarButtonId::Random, 78),
        (BarButtonId::UpdateWallpaper, 82),
        (BarButtonId::Undo, 80),
        (BarButtonId::SavePng, 82),
        (BarButtonId::Settings, 64),
        (BarButtonId::Shortcuts, 82),
    ];

    let mut current_x = pad_x;
    let mut result = Vec::with_capacity(defs.len());
    for (id, base_w) in defs {
        let w = scale_by_dpi(base_w, dpi);
        let rect = RECT {
            left: current_x,
            top: btn_y,
            right: current_x + w,
            bottom: btn_y + btn_h,
        };
        result.push((id, rect));
        current_x += w + gap;
    }
    result
}

pub fn get_frame_overlay_buttons(
    fx: i32,
    fy: i32,
    fh: i32,
    dpi: u32,
) -> [(FrameOverlayButtonId, RECT); 4] {
    let btn_h = scale_by_dpi(28, dpi);
    let bottom_margin = scale_by_dpi(36, dpi);
    let btn_y = fy + fh - bottom_margin;
    let gap = scale_by_dpi(6, dpi);
    let start_x = fx + scale_by_dpi(8, dpi);

    let defs = [
        (FrameOverlayButtonId::Pin, 56),
        (FrameOverlayButtonId::Swap, 64),
        (FrameOverlayButtonId::Random, 72),
        (FrameOverlayButtonId::Exclude, 76),
    ];

    let mut current_x = start_x;
    let mut buttons = Vec::with_capacity(4);
    for (id, base_w) in defs {
        let w = scale_by_dpi(base_w, dpi);
        buttons.push((
            id,
            RECT {
                left: current_x,
                top: btn_y,
                right: current_x + w,
                bottom: btn_y + btn_h,
            },
        ));
        current_x += w + gap;
    }

    [buttons[0], buttons[1], buttons[2], buttons[3]]
}

pub fn osd_text_display_width(text: &str) -> i32 {
    let mut w = 0;
    for ch in text.chars() {
        if ch.is_ascii() {
            w += 8;
        } else {
            w += 14;
        }
    }
    w
}

pub fn osd_notification_rect(text: &str, win_w: i32, has_undo: bool) -> RECT {
    osd_notification_rect_dpi(text, win_w, has_undo, 96)
}

pub fn osd_notification_rect_dpi(text: &str, win_w: i32, has_undo: bool, dpi: u32) -> RECT {
    let text_w = scale_by_dpi(osd_text_display_width(text), dpi);
    let extra = scale_by_dpi(if has_undo { 160 } else { 50 }, dpi);
    let min_w = scale_by_dpi(if has_undo { 360 } else { 200 }, dpi);
    let osd_w = (text_w + extra).max(min_w);
    let osd_h = scale_by_dpi(40, dpi);
    let ox = (win_w - osd_w) / 2;
    let oy = scale_by_dpi(60, dpi);
    RECT {
        left: ox,
        top: oy,
        right: ox + osd_w,
        bottom: oy + osd_h,
    }
}

pub fn osd_undo_button_rect(text: &str, win_w: i32) -> RECT {
    osd_undo_button_rect_dpi(text, win_w, 96)
}

pub fn osd_undo_button_rect_dpi(text: &str, win_w: i32, dpi: u32) -> RECT {
    let r = osd_notification_rect_dpi(text, win_w, true, dpi);
    let offset_right = scale_by_dpi(110, dpi);
    let btn_w = scale_by_dpi(95, dpi);
    let offset_top = scale_by_dpi(6, dpi);
    let btn_h = scale_by_dpi(28, dpi);
    RECT {
        left: r.right - offset_right,
        top: r.top + offset_top,
        right: r.right - offset_right + btn_w,
        bottom: r.top + offset_top + btn_h,
    }
}

pub fn empty_state_folder_btn_rect(win_w: i32, win_h: i32, bar_h: i32) -> RECT {
    empty_state_folder_btn_rect_dpi(win_w, win_h, bar_h, 96)
}

pub fn empty_state_folder_btn_rect_dpi(win_w: i32, win_h: i32, bar_h: i32, dpi: u32) -> RECT {
    let btn_w = scale_by_dpi(220, dpi);
    let btn_h = scale_by_dpi(36, dpi);
    let bx = (win_w - btn_w) / 2;
    let by = bar_h + (win_h - bar_h) / 2 + scale_by_dpi(25, dpi);
    RECT {
        left: bx,
        top: by,
        right: bx + btn_w,
        bottom: by + btn_h,
    }
}

pub fn get_filmstrip_layout(
    win_w: i32,
    win_h: i32,
    cur_idx: usize,
    total_items: usize,
    dpi: u32,
) -> (i32, i32, i32, usize, usize) {
    let strip_h = scale_by_dpi(92, dpi);
    let strip_y = win_h - strip_h;
    let item_w = scale_by_dpi(90, dpi);
    let visible_count = ((win_w - scale_by_dpi(20, dpi)).max(item_w) / item_w) as usize;
    let half = visible_count / 2;
    let start_idx = cur_idx.saturating_sub(half).min(total_items.saturating_sub(visible_count));
    let end_idx = (start_idx + visible_count).min(total_items);
    (strip_y, strip_h, item_w, start_idx, end_idx)
}

/// # Safety
/// Caller must pass a valid window handle `hwnd`.
pub unsafe fn create_gui_font(hwnd: HWND, size_pt: i32, weight: i32) -> HFONT {
    unsafe {
        let dpi = GetDpiForWindow(hwnd);
        let effective_dpi = if dpi == 0 { 96 } else { dpi };
        let height = -((size_pt * effective_dpi as i32 + 36) / 72);

        let mut font = CreateFontW(
            height,
            0,
            0,
            0,
            weight,
            0,
            0,
            0,
            DEFAULT_CHARSET,
            OUT_DEFAULT_PRECIS,
            CLIP_DEFAULT_PRECIS,
            CLEARTYPE_QUALITY,
            0,
            w!("Yu Gothic UI"),
        );
        if font.0.is_null() {
            font = CreateFontW(
                height,
                0,
                0,
                0,
                weight,
                0,
                0,
                0,
                DEFAULT_CHARSET,
                OUT_DEFAULT_PRECIS,
                CLIP_DEFAULT_PRECIS,
                CLEARTYPE_QUALITY,
                0,
                w!("Segoe UI"),
            );
        }
        if font.0.is_null() {
            font = CreateFontW(
                height,
                0,
                0,
                0,
                weight,
                0,
                0,
                0,
                DEFAULT_CHARSET,
                OUT_DEFAULT_PRECIS,
                CLIP_DEFAULT_PRECIS,
                CLEARTYPE_QUALITY,
                0,
                w!("Meiryo UI"),
            );
        }
        if font.0.is_null() {
            font = HFONT(GetStockObject(DEFAULT_GUI_FONT).0);
        }
        font
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum SidebarRowKind {
    FolderHeader {
        folder_path: PathBuf,
        display_name: String,
        item_count: usize,
        is_collapsed: bool,
    },
    FileItem {
        catalog_index: usize,
        filename: String,
        is_selected: bool,
        is_pinned: bool,
        is_excluded: bool,
        frame_targets: Vec<(usize, String, RECT)>,
    },
}

#[derive(Debug, Clone)]
pub struct SidebarRowLayout {
    pub kind: SidebarRowKind,
    pub rect: RECT,
}

#[derive(Debug, Clone)]
pub struct SidebarLayout {
    pub bounds: RECT,
    pub header_rect: RECT,
    pub focus_current_btn_rect: RECT,
    pub collapse_all_btn_rect: RECT,
    pub close_btn_rect: RECT,
    pub target_frame_bar: Option<(RECT, Vec<(usize, String, RECT, bool)>)>,
    pub list_rect: RECT,
    pub rows: Vec<SidebarRowLayout>,
    pub total_content_height: i32,
    pub scrollbar_rect: Option<RECT>,
    pub scrollbar_thumb_rect: Option<RECT>,
}

pub fn compute_sidebar_layout(
    catalog: &Catalog,
    selected_index: Option<usize>,
    pinned_paths: &HashSet<PathBuf>,
    collapsed_folders: &HashSet<PathBuf>,
    scroll_offset: i32,
    _win_w: i32,
    win_h: i32,
    bar_h: i32,
    show_filmstrip: bool,
    view_mode: ViewMode,
    layout_frames_count: usize,
    selected_frame_idx: usize,
    dpi: u32,
) -> SidebarLayout {
    let sidebar_w = get_sidebar_width(dpi);
    let strip_h = if show_filmstrip { scale_by_dpi(92, dpi) } else { 0 };
    let bounds = RECT {
        left: 0,
        top: bar_h,
        right: sidebar_w,
        bottom: win_h - strip_h,
    };

    let header_h = scale_by_dpi(38, dpi);
    let header_rect = RECT {
        left: 0,
        top: bar_h,
        right: sidebar_w,
        bottom: bar_h + header_h,
    };

    let btn_size = scale_by_dpi(26, dpi);
    let gap = scale_by_dpi(4, dpi);
    let btn_y = bar_h + (header_h - btn_size) / 2;
    let btn_bottom = btn_y + btn_size;

    let close_right = sidebar_w - scale_by_dpi(6, dpi);
    let close_left = close_right - btn_size;
    let close_btn_rect = RECT {
        left: close_left,
        top: btn_y,
        right: close_right,
        bottom: btn_bottom,
    };

    let col_right = close_left - gap;
    let col_left = col_right - btn_size;
    let collapse_all_btn_rect = RECT {
        left: col_left,
        top: btn_y,
        right: col_right,
        bottom: btn_bottom,
    };

    let focus_right = col_left - gap;
    let focus_left = focus_right - btn_size;
    let focus_current_btn_rect = RECT {
        left: focus_left,
        top: btn_y,
        right: focus_right,
        bottom: btn_bottom,
    };

    let target_frame_bar = if view_mode == ViewMode::Composite && layout_frames_count >= 2 {
        let bar_top = header_rect.bottom;
        let bar_h = scale_by_dpi(32, dpi);
        let bar_rect = RECT {
            left: 0,
            top: bar_top,
            right: sidebar_w,
            bottom: bar_top + bar_h,
        };

        let mut buttons = Vec::new();
        let btn_w = scale_by_dpi(54, dpi);
        let btn_h = scale_by_dpi(22, dpi);
        let btn_y = bar_top + (bar_h - btn_h) / 2;
        let label_w = scale_by_dpi(48, dpi);
        let start_x = scale_by_dpi(10, dpi) + label_w;
        let btn_gap = scale_by_dpi(4, dpi);

        for f_idx in 0..layout_frames_count {
            let label = if layout_frames_count == 2 {
                if f_idx == 0 { "左枠".to_string() } else { "右枠".to_string() }
            } else if layout_frames_count == 3 {
                if f_idx == 0 { "左枠".to_string() } else if f_idx == 1 { "中枠".to_string() } else { "右枠".to_string() }
            } else {
                format!("枠{}", f_idx + 1)
            };
            let b_left = start_x + (f_idx as i32) * (btn_w + btn_gap);
            let b_rect = RECT {
                left: b_left,
                top: btn_y,
                right: b_left + btn_w,
                bottom: btn_y + btn_h,
            };
            buttons.push((f_idx, label, b_rect, f_idx == selected_frame_idx));
        }
        Some((bar_rect, buttons))
    } else {
        None
    };

    let list_top = if let Some((bar_r, _)) = &target_frame_bar {
        bar_r.bottom
    } else {
        header_rect.bottom
    };

    let list_rect = RECT {
        left: 0,
        top: list_top,
        right: sidebar_w,
        bottom: bounds.bottom,
    };
    let list_h = (list_rect.bottom - list_rect.top).max(0);

    let fallback_groups;
    let groups: &[_] = if !catalog.folder_groups.is_empty() {
        &catalog.folder_groups
    } else if !catalog.items.is_empty() {
        let mut temp_cat = catalog.clone();
        temp_cat.rebuild_folder_groups();
        fallback_groups = temp_cat.folder_groups;
        &fallback_groups
    } else {
        &[]
    };

    let folder_h = scale_by_dpi(28, dpi);
    let item_h = scale_by_dpi(44, dpi);

    let mut y_accum = 0;
    let mut rows = Vec::new();

    for group in groups {
        let item_count = group.item_indices.len();
        let is_collapsed = collapsed_folders.contains(&group.folder_path);

        let row_top = list_rect.top + y_accum - scroll_offset;
        let row_bottom = row_top + folder_h;
        if row_bottom >= list_rect.top && row_top <= list_rect.bottom {
            rows.push(SidebarRowLayout {
                kind: SidebarRowKind::FolderHeader {
                    folder_path: group.folder_path.clone(),
                    display_name: group.display_name.clone(),
                    item_count,
                    is_collapsed,
                },
                rect: RECT {
                    left: list_rect.left,
                    top: row_top,
                    right: list_rect.right,
                    bottom: row_bottom,
                },
            });
        }
        y_accum += folder_h;

        if !is_collapsed {
            let group_items_h = (item_count as i32) * item_h;
            let group_items_top = list_rect.top + y_accum - scroll_offset;
            let group_items_bottom = group_items_top + group_items_h;

            // If the entire group of items is outside the visible list area, skip individual item iteration
            if group_items_bottom < list_rect.top || group_items_top > list_rect.bottom {
                y_accum += group_items_h;
            } else {
                for &idx in &group.item_indices {
                    let item = &catalog.items[idx];
                    let item_top = list_rect.top + y_accum - scroll_offset;
                    let item_bottom = item_top + item_h;
                    if item_bottom >= list_rect.top && item_top <= list_rect.bottom {
                        let is_selected = selected_index == Some(idx);
                        let is_pinned = pinned_paths.contains(&item.path);

                        let mut frame_targets = Vec::new();
                        if view_mode == ViewMode::Composite && layout_frames_count >= 2 {
                            let btn_w = scale_by_dpi(24, dpi);
                            let btn_h = scale_by_dpi(20, dpi);
                            let btn_gap = scale_by_dpi(3, dpi);
                            let badge_reserve = if is_pinned || item.is_excluded {
                                scale_by_dpi(26, dpi)
                            } else {
                                0
                            };
                            let total_btns_w = (layout_frames_count as i32) * btn_w
                                + ((layout_frames_count as i32) - 1) * btn_gap;
                            let start_btn_x =
                                list_rect.right - scale_by_dpi(6, dpi) - badge_reserve - total_btns_w;
                            let by = item_top + (item_h - btn_h) / 2;

                            for f_idx in 0..layout_frames_count {
                                let label = if layout_frames_count == 2 {
                                    if f_idx == 0 { "左".to_string() } else { "右".to_string() }
                                } else if layout_frames_count == 3 {
                                    if f_idx == 0 {
                                        "左".to_string()
                                    } else if f_idx == 1 {
                                        "中".to_string()
                                    } else {
                                        "右".to_string()
                                    }
                                } else {
                                    format!("{}", f_idx + 1)
                                };
                                let bx = start_btn_x + (f_idx as i32) * (btn_w + btn_gap);
                                frame_targets.push((
                                    f_idx,
                                    label,
                                    RECT {
                                        left: bx,
                                        top: by,
                                        right: bx + btn_w,
                                        bottom: by + btn_h,
                                    },
                                ));
                            }
                        }

                        rows.push(SidebarRowLayout {
                            kind: SidebarRowKind::FileItem {
                                catalog_index: idx,
                                filename: item.filename.clone(),
                                is_selected,
                                is_pinned,
                                is_excluded: item.is_excluded,
                                frame_targets,
                            },
                            rect: RECT {
                                left: list_rect.left,
                                top: item_top,
                                right: list_rect.right,
                                bottom: item_bottom,
                            },
                        });
                    }
                    y_accum += item_h;
                }
            }
        }
    }

    let total_content_height = y_accum;
    let (scrollbar_rect, scrollbar_thumb_rect) = if total_content_height > list_h && list_h > 0 {
        let bar_w = scale_by_dpi(8, dpi);
        let margin_r = scale_by_dpi(3, dpi);
        let s_rect = RECT {
            left: list_rect.right - bar_w - margin_r,
            top: list_rect.top + scale_by_dpi(2, dpi),
            right: list_rect.right - margin_r,
            bottom: list_rect.bottom - scale_by_dpi(2, dpi),
        };
        let bar_usable_h = (s_rect.bottom - s_rect.top).max(1);
        let thumb_h = ((bar_usable_h as i64 * list_h as i64) / total_content_height as i64) as i32;
        let thumb_h = thumb_h.clamp(scale_by_dpi(24, dpi), bar_usable_h);
        let max_scroll = (total_content_height - list_h).max(1);
        let thumb_top = s_rect.top
            + ((scroll_offset as i64 * (bar_usable_h - thumb_h) as i64) / max_scroll as i64) as i32;
        let thumb_rect = RECT {
            left: s_rect.left,
            top: thumb_top,
            right: s_rect.right,
            bottom: thumb_top + thumb_h,
        };
        (Some(s_rect), Some(thumb_rect))
    } else {
        (None, None)
    };

    SidebarLayout {
        bounds,
        header_rect,
        focus_current_btn_rect,
        collapse_all_btn_rect,
        close_btn_rect,
        target_frame_bar,
        list_rect,
        rows,
        total_content_height,
        scrollbar_rect,
        scrollbar_thumb_rect,
    }
}

pub fn is_in_scrollbar_hit_area(x: i32, y: i32, s_rect: RECT, list_rect: RECT, dpi: u32) -> bool {
    let hit_left = s_rect.left - scale_by_dpi(10, dpi);
    let hit_right = list_rect.right;
    x >= hit_left && x <= hit_right && y >= s_rect.top && y <= s_rect.bottom
}
