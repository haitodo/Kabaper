use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::core::*;

use crate::ui::types::{BarButtonId, FrameOverlayButtonId};

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

pub fn get_bar_height(dpi: u32) -> i32 {
    scale_by_dpi(45, dpi)
}

pub fn get_operation_bar_buttons(dpi: u32) -> Vec<(BarButtonId, RECT)> {
    let pad_x = scale_by_dpi(10, dpi);
    let gap = scale_by_dpi(6, dpi);
    let btn_y = scale_by_dpi(6, dpi);
    let btn_h = scale_by_dpi(32, dpi);

    let defs = [
        (BarButtonId::Open, 76),
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
