use std::collections::HashSet;
use std::path::PathBuf;
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::*;

use crate::layout::{ImageId, ScaleLimit};
use crate::monitor::MonitorInfo;
use crate::ui::layout_calc::*;
use crate::ui::state::AppState;
use crate::ui::types::*;
/// # Safety
/// Caller must pass a valid device context `hdc` and valid pointer `rect`.
pub unsafe fn draw_round_rect_fill_stroke(
    hdc: HDC,
    rect: &RECT,
    fill_color: COLORREF,
    stroke_color: Option<COLORREF>,
    radius: i32,
) {
    unsafe {
        let brush = CreateSolidBrush(fill_color);
        let old_brush = SelectObject(hdc, brush.into());
        let (pen, old_pen) = if let Some(sc) = stroke_color {
            let pen = CreatePen(PS_SOLID, 1, sc);
            let old_pen = SelectObject(hdc, pen.into());
            (Some(pen), Some(old_pen))
        } else {
            let pen = CreatePen(PS_NULL, 0, COLORREF(0));
            let old_pen = SelectObject(hdc, pen.into());
            (Some(pen), Some(old_pen))
        };

        let _ = RoundRect(hdc, rect.left, rect.top, rect.right, rect.bottom, radius, radius);

        if let (Some(pen), Some(old_pen)) = (pen, old_pen) {
            let _ = SelectObject(hdc, old_pen);
            let _ = DeleteObject(pen.into());
        }
        let _ = SelectObject(hdc, old_brush);
        let _ = DeleteObject(brush.into());
    }
}

/// # Safety
/// Caller must pass a valid device context `hdc`.
#[allow(clippy::too_many_arguments)]
pub unsafe fn draw_modern_button(
    hdc: HDC,
    x: i32,
    y: i32,
    w: i32,
    h: i32,
    text: &str,
    style: ModernButtonStyle,
    radius: i32,
) {
    unsafe {
        let r = RECT {
            left: x,
            top: y,
            right: x + w,
            bottom: y + h,
        };

        let (fill, stroke, text_color) = match style {
            ModernButtonStyle::Primary => (
                M3_PRIMARY_BLUE,
                Some(rgb(168, 199, 250)),
                M3_PRIMARY_TEXT,
            ),
            ModernButtonStyle::SecondaryTonal => (
                M3_SURFACE_HIGH,
                Some(M3_OUTLINE),
                M3_TEXT_PRIMARY,
            ),
            ModernButtonStyle::SegmentActive => (
                rgb(26, 52, 85),
                Some(M3_PRIMARY_ACCENT),
                M3_PRIMARY_ACCENT,
            ),
            ModernButtonStyle::SegmentInactive => (
                M3_SURFACE_CARD,
                Some(M3_OUTLINE_VARIANT),
                M3_TEXT_MUTED,
            ),
            ModernButtonStyle::Ghost => (
                M3_SURFACE_BAR,
                Some(M3_OUTLINE_VARIANT),
                M3_TEXT_SECONDARY,
            ),
        };

        draw_round_rect_fill_stroke(hdc, &r, fill, stroke, radius);

        let _ = SetBkMode(hdc, TRANSPARENT);
        let _ = SetTextColor(hdc, text_color);
        let mut tr = r;
        draw_text(hdc, text, &mut tr, DT_CENTER | DT_VCENTER | DT_SINGLELINE);
    }
}

unsafe fn draw_empty_state_guidance(
    hdc: HDC,
    win_w: i32,
    win_h: i32,
    bar_h: i32,
    msg: &str,
    dpi: u32,
) {
    unsafe {
        let btn_r = empty_state_folder_btn_rect_dpi(win_w, win_h, bar_h, dpi);
        let mut tr = RECT {
            left: scale_by_dpi(20, dpi),
            top: bar_h + (win_h - bar_h) / 2 - scale_by_dpi(50, dpi),
            right: win_w - scale_by_dpi(20, dpi),
            bottom: btn_r.top - scale_by_dpi(14, dpi),
        };
        let _ = SetTextColor(hdc, M3_TEXT_SECONDARY);
        let _ = SetBkMode(hdc, TRANSPARENT);
        draw_text(hdc, msg, &mut tr, DT_CENTER | DT_WORDBREAK);

        draw_modern_button(
            hdc,
            btn_r.left,
            btn_r.top,
            btn_r.right - btn_r.left,
            btn_r.bottom - btn_r.top,
            "フォルダを選択",
            ModernButtonStyle::Primary,
            scale_by_dpi(8, dpi),
        );
    }
}

pub(crate) fn paint_window(state: &mut AppState, hwnd: HWND, hdc: HDC) {
    unsafe {
        let mut cr = RECT::default();
        let _ = GetClientRect(hwnd, &mut cr);
        let win_w = cr.right.max(1);
        let win_h = cr.bottom.max(1);
        let dpi = get_window_dpi(hwnd);

        // Double-buffered memory DC
        let mem_dc = CreateCompatibleDC(Some(hdc));
        let mem_bmp = CreateCompatibleBitmap(hdc, win_w, win_h);
        let old_bmp = SelectObject(mem_dc, mem_bmp.into());

        // Modern system GUI font (Segoe UI with Meiryo UI fallback)
        let font = create_gui_font(hwnd, 10, 400);
        let old_font = SelectObject(mem_dc, font.into());

        // Fill background with Google Material 3 Deep Surface
        let bg_brush = CreateSolidBrush(M3_BG);
        let _ = FillRect(mem_dc, &cr, bg_brush);
        let _ = DeleteObject(bg_brush.into());

        let bar_h = if state.bar_visible {
            get_bar_height(dpi)
        } else {
            0
        };
        let canvas_layout = get_canvas_layout(
            win_w,
            win_h,
            bar_h,
            state.show_sidebar,
            state.show_filmstrip,
            dpi,
        );
        let canvas_x = canvas_layout.x;
        let canvas_y = canvas_layout.y;
        let canvas_w = canvas_layout.w;
        let canvas_h = canvas_layout.h;

        // Render Canvas
        if state.view_mode == ViewMode::Composite {
            if state.active_composed.is_none() && !state.catalog.items.is_empty() {
                state.rebuild_layout(false, false);
            }

            if let Some(composed) = &state.active_composed {
                let (mw, mh) = (composed.width as f64, composed.height as f64);
                let fit_scale = (canvas_w as f64 / mw).min(canvas_h as f64 / mh);
                let dw = (mw * fit_scale).round() as i32;
                let dh = (mh * fit_scale).round() as i32;
                let ox = canvas_x + (canvas_w - dw) / 2;
                let oy = canvas_y + (canvas_h - dh) / 2;

                let mut bi = BITMAPINFO::default();
                bi.bmiHeader.biSize = std::mem::size_of::<BITMAPINFOHEADER>() as u32;
                bi.bmiHeader.biWidth = composed.width as i32;
                bi.bmiHeader.biHeight = -(composed.height as i32); // Top-down
                bi.bmiHeader.biPlanes = 1;
                bi.bmiHeader.biBitCount = 32;
                bi.bmiHeader.biCompression = BI_RGB.0;

                let _ = SetStretchBltMode(mem_dc, HALFTONE);
                let _ = StretchDIBits(
                    mem_dc,
                    ox,
                    oy,
                    dw,
                    dh,
                    0,
                    0,
                    composed.width as i32,
                    composed.height as i32,
                    Some(composed.pixels.as_ptr() as *const _),
                    &bi,
                    DIB_RGB_COLORS,
                    SRCCOPY,
                );

                // Render Frame Overlays
                if let Some(plan) = &state.active_plan {
                    for (i, frame) in plan.frames.iter().enumerate() {
                        let fx = ox + (frame.slot.x as f64 * fit_scale).round() as i32;
                        let fy = oy + (frame.slot.y as f64 * fit_scale).round() as i32;
                        let fw = (frame.slot.width as f64 * fit_scale).round() as i32;
                        let fh = (frame.slot.height as f64 * fit_scale).round() as i32;

                        if i == state.selected_frame_idx {
                            let select_brush = CreateSolidBrush(COLORREF(0x00FF9933)); // Accent border
                            let frame_r = RECT {
                                left: fx,
                                top: fy,
                                right: fx + fw,
                                bottom: fy + fh,
                            };
                            let _ = FrameRect(mem_dc, &frame_r, select_brush);
                            let _ = DeleteObject(select_brush.into());

                            // Spec 4.1: Frame action buttons (ピン, 枠交換, ランダム, 除外)
                            let overlay_buttons = get_frame_overlay_buttons(fx, fy, fh, dpi);
                            for (btn_id, r) in overlay_buttons {
                                let w = r.right - r.left;
                                let h = r.bottom - r.top;
                                match btn_id {
                                    FrameOverlayButtonId::Pin => {
                                        let pin_txt = if frame.pinned { "解除" } else { "ピン" };
                                        draw_overlay_btn(mem_dc, r.left, r.top, w, h, pin_txt);
                                    }
                                    FrameOverlayButtonId::Swap => {
                                        draw_overlay_btn(mem_dc, r.left, r.top, w, h, "枠交換");
                                    }
                                    FrameOverlayButtonId::Random => {
                                        draw_overlay_btn(mem_dc, r.left, r.top, w, h, "ランダム");
                                    }
                                    FrameOverlayButtonId::Exclude => {
                                        let ex_txt = if let Some(c) = &frame.content {
                                            if state.catalog.items.iter().any(|it| {
                                                it.path.to_string_lossy() == c.id.0 && it.is_excluded
                                            }) {
                                                "除外解除"
                                            } else {
                                                "除外"
                                            }
                                        } else {
                                            "除外"
                                        };
                                        draw_overlay_btn(mem_dc, r.left, r.top, w, h, ex_txt);
                                    }
                                }
                            }
                        }

                        let mut badge_y = fy + scale_by_dpi(8, dpi);
                        let badge_step = scale_by_dpi(28, dpi);
                        if frame.pinned {
                            draw_badge(
                                mem_dc,
                                fx + scale_by_dpi(8, dpi),
                                badge_y,
                                "ピン留め",
                                COLORREF(0x000088FF),
                                dpi,
                            );
                            badge_y += badge_step;
                        }
                        if let Some(c) = &frame.content {
                            match c.scale_limited {
                                ScaleLimit::MaxScale => {
                                    draw_badge(
                                        mem_dc,
                                        fx + scale_by_dpi(8, dpi),
                                        badge_y,
                                        "倍率上限",
                                        COLORREF(0x0000A5FF),
                                        dpi,
                                    );
                                    badge_y += badge_step;
                                }
                                ScaleLimit::CropLimit => {
                                    draw_badge(
                                        mem_dc,
                                        fx + scale_by_dpi(8, dpi),
                                        badge_y,
                                        "切抜上限",
                                        COLORREF(0x007820D9),
                                        dpi,
                                    );
                                    badge_y += badge_step;
                                }
                                ScaleLimit::None => {}
                            }
                            if let Some(spec) = state
                                .catalog
                                .items
                                .iter()
                                .find(|it| it.path.to_string_lossy() == c.id.0)
                                && spec.is_excluded
                            {
                                draw_badge(
                                    mem_dc,
                                    fx + scale_by_dpi(8, dpi),
                                    badge_y,
                                    "除外中",
                                    COLORREF(0x003333FF),
                                    dpi,
                                );
                            }
                        } else {
                            draw_badge(
                                mem_dc,
                                fx + scale_by_dpi(8, dpi),
                                badge_y,
                                "空き枠",
                                COLORREF(0x00666666),
                                dpi,
                            );
                        }
                    }
                }
            } else {
                let msg = if state.catalog.items.is_empty() {
                    "画像フォルダを開いてください（Ctrl+O）\nまたは画像をここにドロップ"
                } else {
                    "有効な候補画像がありません\n除外を解除するか（Delキー）、別のフォルダを選択してください"
                };
                draw_empty_state_guidance(mem_dc, win_w, win_h, bar_h, msg, dpi);
            }
        } else {
            // Normal view
            if let Some(item) = state.catalog.current_item() {
                let img_id = ImageId(item.path.to_string_lossy().to_string());
                if !state.cached_images.contains_key(&img_id)
                    && let Ok(dec) = state.decoder.decode_image(&item.path, Some(4096))
                {
                    state.cached_images.insert(img_id.clone(), dec);
                }

                if let Some(dec) = state.cached_images.get(&img_id) {
                    let fit_scale = (canvas_w as f64 / dec.width as f64)
                        .min(canvas_h as f64 / dec.height as f64)
                        .min(1.0);
                    let current_scale = fit_scale * state.normal_zoom;
                    let dw = (dec.width as f64 * current_scale).round() as i32;
                    let dh = (dec.height as f64 * current_scale).round() as i32;

                    let max_pan_x = ((dw - canvas_w) / 2).max(0) as f64;
                    let max_pan_y = ((dh - canvas_h) / 2).max(0) as f64;
                    let pan_x = state.normal_pan_offset.0.clamp(-max_pan_x, max_pan_x);
                    let pan_y = state.normal_pan_offset.1.clamp(-max_pan_y, max_pan_y);

                    let ox = canvas_x + (canvas_w - dw) / 2 + pan_x.round() as i32;
                    let oy = canvas_y + (canvas_h - dh) / 2 + pan_y.round() as i32;

                    let mut bi = BITMAPINFO::default();
                    bi.bmiHeader.biSize = std::mem::size_of::<BITMAPINFOHEADER>() as u32;
                    bi.bmiHeader.biWidth = dec.width as i32;
                    bi.bmiHeader.biHeight = -(dec.height as i32);
                    bi.bmiHeader.biPlanes = 1;
                    bi.bmiHeader.biBitCount = 32;
                    bi.bmiHeader.biCompression = BI_RGB.0;

                    let _ = SetStretchBltMode(mem_dc, HALFTONE);
                    let _ = IntersectClipRect(
                        mem_dc,
                        canvas_x,
                        canvas_y,
                        canvas_x + canvas_w,
                        canvas_y + canvas_h,
                    );
                    let _ = StretchDIBits(
                        mem_dc,
                        ox,
                        oy,
                        dw,
                        dh,
                        0,
                        0,
                        dec.width as i32,
                        dec.height as i32,
                        Some(dec.pixels.as_ptr() as *const _),
                        &bi,
                        DIB_RGB_COLORS,
                        SRCCOPY,
                    );
                    let _ = SelectClipRgn(mem_dc, None);
                }
            } else {
                let msg = if state.catalog.items.is_empty() {
                    "画像フォルダを開いてください（Ctrl+O）\nまたは画像をここにドロップ"
                } else {
                    "有効な画像がありません\n別のフォルダを選択してください"
                };
                draw_empty_state_guidance(mem_dc, win_w, win_h, bar_h, msg, dpi);
            }
        }

        // Render Operation Bar
        if state.bar_visible {
            draw_operation_bar(state, mem_dc, win_w, dpi);
        }

        // Render Info Display
        if state.show_info {
            draw_info_overlay(state, mem_dc, win_h, dpi);
        }

        // Render Sidebar
        if state.show_sidebar {
            draw_sidebar(state, mem_dc, win_w, win_h, bar_h, dpi);
        }

        // Render Filmstrip
        if state.show_filmstrip {
            draw_filmstrip(state, mem_dc, win_w, win_h, dpi);
        }

        // Render OSD
        if let Some((msg_str, _, _, has_undo)) = &state.osd {
            draw_osd_notification(mem_dc, msg_str, win_w, *has_undo, dpi);
        }

        // Render Error Banner
        if !state.error_banner.is_empty() {
            draw_error_banner(mem_dc, &state.error_banner, win_w, dpi);
        }

        // Render Modal Overlays
        match state.modal_dialog {
            ModalDialog::Settings => draw_settings_modal(mem_dc, state, win_w, win_h, dpi),
            ModalDialog::ExcludeList => draw_exclude_list_modal(mem_dc, state, win_w, win_h, dpi),
            ModalDialog::Shortcuts => draw_shortcuts_modal(mem_dc, win_w, win_h, dpi),
            ModalDialog::None => {}
        }

        // Blit back to screen
        let _ = BitBlt(hdc, 0, 0, win_w, win_h, Some(mem_dc), 0, 0, SRCCOPY);

        let _ = SelectObject(mem_dc, old_font);
        let _ = DeleteObject(font.into());
        let _ = SelectObject(mem_dc, old_bmp);
        let _ = DeleteObject(mem_bmp.into());
        let _ = DeleteDC(mem_dc);
    }
}

fn draw_operation_bar(state: &AppState, hdc: HDC, width: i32, dpi: u32) {
    unsafe {
        let bar_h = get_bar_height(dpi);
        let bar_rect = RECT {
            left: 0,
            top: 0,
            right: width,
            bottom: bar_h,
        };
        let brush = CreateSolidBrush(M3_SURFACE_BAR);
        let _ = FillRect(hdc, &bar_rect, brush);
        let _ = DeleteObject(brush.into());

        let line_brush = CreateSolidBrush(M3_OUTLINE_VARIANT);
        let line_r = RECT {
            left: 0,
            top: bar_h - 1,
            right: width,
            bottom: bar_h,
        };
        let _ = FillRect(hdc, &line_r, line_brush);
        let _ = DeleteObject(line_brush.into());

        let _ = SetBkMode(hdc, TRANSPARENT);

        let radius = scale_by_dpi(8, dpi);
        let buttons = get_operation_bar_buttons(dpi);

        for (id, r) in buttons {
            let w = r.right - r.left;
            let h = r.bottom - r.top;
            match id {
                BarButtonId::Open => {
                    draw_modern_button(
                        hdc,
                        r.left,
                        r.top,
                        w,
                        h,
                        "開く",
                        ModernButtonStyle::SecondaryTonal,
                        radius,
                    );
                }
                BarButtonId::Sidebar => {
                    let style = if state.show_sidebar {
                        ModernButtonStyle::SegmentActive
                    } else {
                        ModernButtonStyle::SecondaryTonal
                    };
                    draw_modern_button(
                        hdc,
                        r.left,
                        r.top,
                        w,
                        h,
                        "一覧",
                        style,
                        radius,
                    );
                }
                BarButtonId::ViewMode => {
                    let view_txt = if state.view_mode == ViewMode::Normal {
                        "通常"
                    } else {
                        "合成"
                    };
                    draw_modern_button(
                        hdc,
                        r.left,
                        r.top,
                        w,
                        h,
                        view_txt,
                        ModernButtonStyle::SegmentActive,
                        radius,
                    );
                }
                BarButtonId::Monitor => {
                    let mon_str = format!(
                        "モニタ {}",
                        state.target_monitor().map_or(1, |m| m.number)
                    );
                    draw_modern_button(
                        hdc,
                        r.left,
                        r.top,
                        w,
                        h,
                        &mon_str,
                        ModernButtonStyle::SecondaryTonal,
                        radius,
                    );
                }
                BarButtonId::Split => {
                    let split_str = match state.config.layout.mode.as_str() {
                        "single" => "分割: 1",
                        "split2" => "分割: 2",
                        "split3" => "分割: 3",
                        _ => "分割: 自動",
                    };
                    draw_modern_button(
                        hdc,
                        r.left,
                        r.top,
                        w,
                        h,
                        split_str,
                        ModernButtonStyle::SecondaryTonal,
                        radius,
                    );
                }
                BarButtonId::Scale => {
                    let scale_str = format!("倍率 {:.2}", state.config.layout.max_scale);
                    draw_modern_button(
                        hdc,
                        r.left,
                        r.top,
                        w,
                        h,
                        &scale_str,
                        ModernButtonStyle::SecondaryTonal,
                        radius,
                    );
                }
                BarButtonId::Apply => {
                    draw_modern_button(
                        hdc,
                        r.left,
                        r.top,
                        w,
                        h,
                        "適用",
                        ModernButtonStyle::Primary,
                        radius,
                    );
                }
                BarButtonId::Random => {
                    draw_modern_button(
                        hdc,
                        r.left,
                        r.top,
                        w,
                        h,
                        "ランダム",
                        ModernButtonStyle::SecondaryTonal,
                        radius,
                    );
                }
                BarButtonId::UpdateWallpaper => {
                    draw_modern_button(
                        hdc,
                        r.left,
                        r.top,
                        w,
                        h,
                        "壁紙更新",
                        ModernButtonStyle::SecondaryTonal,
                        radius,
                    );
                }
                BarButtonId::Undo => {
                    draw_modern_button(
                        hdc,
                        r.left,
                        r.top,
                        w,
                        h,
                        "元に戻す",
                        ModernButtonStyle::SecondaryTonal,
                        radius,
                    );
                }
                BarButtonId::SavePng => {
                    draw_modern_button(
                        hdc,
                        r.left,
                        r.top,
                        w,
                        h,
                        "PNG保存",
                        ModernButtonStyle::Ghost,
                        radius,
                    );
                }
                BarButtonId::Settings => {
                    draw_modern_button(
                        hdc,
                        r.left,
                        r.top,
                        w,
                        h,
                        "設定",
                        ModernButtonStyle::Ghost,
                        radius,
                    );
                }
                BarButtonId::Shortcuts => {
                    draw_modern_button(
                        hdc,
                        r.left,
                        r.top,
                        w,
                        h,
                        "操作一覧",
                        ModernButtonStyle::Ghost,
                        radius,
                    );
                }
            }
        }

        if state.has_pinned_frames() {
            let badge_w = scale_by_dpi(110, dpi);
            let badge_y = scale_by_dpi(11, dpi);
            draw_badge(
                hdc,
                width - badge_w - scale_by_dpi(10, dpi),
                badge_y,
                "合成固定中",
                M3_CYAN_PINNED,
                dpi,
            );
        }
    }
}

#[allow(dead_code)]
fn draw_bar_btn(hdc: HDC, x: i32, y: i32, w: i32, h: i32, text: &str) {
    unsafe {
        draw_modern_button(hdc, x, y, w, h, text, ModernButtonStyle::SecondaryTonal, 8);
    }
}

unsafe fn fill_alpha_rect(hdc: HDC, rect: &RECT, color: COLORREF, alpha: u8) {
    let w = rect.right - rect.left;
    let h = rect.bottom - rect.top;
    if w <= 0 || h <= 0 {
        return;
    }
    unsafe {
        let mem_dc = CreateCompatibleDC(Some(hdc));
        if mem_dc.is_invalid() {
            return;
        }
        let mut bi = BITMAPINFO::default();
        bi.bmiHeader.biSize = std::mem::size_of::<BITMAPINFOHEADER>() as u32;
        bi.bmiHeader.biWidth = w;
        bi.bmiHeader.biHeight = -h;
        bi.bmiHeader.biPlanes = 1;
        bi.bmiHeader.biBitCount = 32;
        bi.bmiHeader.biCompression = BI_RGB.0;

        let mut bits = std::ptr::null_mut();
        if let Ok(bmp) = CreateDIBSection(Some(mem_dc), &bi, DIB_RGB_COLORS, &mut bits, None, 0)
            && !bits.is_null()
        {
            let old_bmp = SelectObject(mem_dc, bmp.into());
            let r = (color.0 & 0xFF) as u8;
            let g = ((color.0 >> 8) & 0xFF) as u8;
            let b = ((color.0 >> 16) & 0xFF) as u8;
            let p = bits as *mut u32;
            let pixel = ((b as u32) << 16) | ((g as u32) << 8) | (r as u32);
            for i in 0..(w * h) as isize {
                *p.offset(i) = pixel;
            }
            let blend = BLENDFUNCTION {
                BlendOp: AC_SRC_OVER as u8,
                BlendFlags: 0,
                SourceConstantAlpha: alpha,
                AlphaFormat: 0,
            };
            let _ = AlphaBlend(hdc, rect.left, rect.top, w, h, mem_dc, 0, 0, w, h, blend);
            SelectObject(mem_dc, old_bmp);
            let _ = DeleteObject(bmp.into());
        }
        let _ = DeleteDC(mem_dc);
    }
}

fn draw_overlay_btn(hdc: HDC, x: i32, y: i32, w: i32, h: i32, text: &str) {
    unsafe {
        let r = RECT {
            left: x,
            top: y,
            right: x + w,
            bottom: y + h,
        };
        fill_alpha_rect(hdc, &r, M3_SURFACE_HIGH, 210);

        let border_brush = CreateSolidBrush(M3_OUTLINE);
        let _ = FrameRect(hdc, &r, border_brush);
        let _ = DeleteObject(border_brush.into());

        let _ = SetBkMode(hdc, TRANSPARENT);
        let _ = SetTextColor(hdc, M3_TEXT_PRIMARY);
        let mut tr = r;
        draw_text(hdc, text, &mut tr, DT_CENTER | DT_VCENTER | DT_SINGLELINE);
    }
}

fn draw_badge(hdc: HDC, x: i32, y: i32, text: &str, color: COLORREF, dpi: u32) {
    unsafe {
        let text_w = scale_by_dpi(osd_text_display_width(text), dpi);
        let pad = scale_by_dpi(16, dpi);
        let w = text_w + pad;
        let h = scale_by_dpi(24, dpi);
        let r = RECT {
            left: x,
            top: y,
            right: x + w,
            bottom: y + h,
        };
        fill_alpha_rect(hdc, &r, color, 190);

        let border_brush = CreateSolidBrush(color);
        let _ = FrameRect(hdc, &r, border_brush);
        let _ = DeleteObject(border_brush.into());

        let _ = SetBkMode(hdc, TRANSPARENT);
        let _ = SetTextColor(hdc, M3_PRIMARY_TEXT);
        let mut tr = r;
        draw_text(hdc, text, &mut tr, DT_CENTER | DT_VCENTER | DT_SINGLELINE);
    }
}

fn draw_info_overlay(state: &AppState, hdc: HDC, win_h: i32, dpi: u32) {
    unsafe {
        let info_w = scale_by_dpi(360, dpi);
        let info_h = scale_by_dpi(110, dpi);
        let margin = scale_by_dpi(16, dpi);
        let r = RECT {
            left: margin,
            top: win_h - info_h - margin,
            right: margin + info_w,
            bottom: win_h - margin,
        };

        // Material 3 Frosted Glass Surface
        fill_alpha_rect(hdc, &r, M3_SURFACE_CARD, 210);

        let border_brush = CreateSolidBrush(M3_OUTLINE);
        let _ = FrameRect(hdc, &r, border_brush);
        let _ = DeleteObject(border_brush.into());

        let _ = SetBkMode(hdc, TRANSPARENT);

        let filename = state
            .catalog
            .current_item()
            .map_or("なし", |it| &it.filename);
        let idx = state.catalog.current_index.map_or(0, |i| i + 1);
        let total = state.catalog.items.len();

        let line1_name = filename;
        let line1_idx = format!("({} / {})", idx, total);
        let line2 = if let Some(plan) = &state.active_plan {
            format!(
                "解像度: {}x{}  被覆率: {:.1}%",
                plan.monitor_px.0,
                plan.monitor_px.1,
                plan.coverage * 100.0
            )
        } else {
            "解像度: 未設定".to_string()
        };
        let line3 = format!(
            "最大拡大率 M: {:.2}  枠線: {}px",
            state.config.layout.max_scale, state.config.layout.border_px
        );

        let pad_x = scale_by_dpi(14, dpi);
        let idx_w = scale_by_dpi(80, dpi);
        let line_h = scale_by_dpi(22, dpi);

        let mut r1_name = RECT {
            left: r.left + pad_x,
            top: r.top + scale_by_dpi(10, dpi),
            right: r.right - idx_w,
            bottom: r.top + scale_by_dpi(10, dpi) + line_h,
        };
        let mut r1_idx = RECT {
            left: r.right - idx_w,
            top: r.top + scale_by_dpi(10, dpi),
            right: r.right - pad_x,
            bottom: r.top + scale_by_dpi(10, dpi) + line_h,
        };
        let mut r2 = RECT {
            left: r.left + pad_x,
            top: r.top + scale_by_dpi(38, dpi),
            right: r.right - pad_x,
            bottom: r.top + scale_by_dpi(38, dpi) + line_h,
        };
        let mut r3 = RECT {
            left: r.left + pad_x,
            top: r.top + scale_by_dpi(65, dpi),
            right: r.right - pad_x,
            bottom: r.top + scale_by_dpi(65, dpi) + line_h,
        };

        let _ = SetTextColor(hdc, M3_TEXT_PRIMARY);
        draw_text(hdc, line1_name, &mut r1_name, DT_LEFT | DT_SINGLELINE | DT_END_ELLIPSIS);
        let _ = SetTextColor(hdc, M3_PRIMARY_ACCENT);
        draw_text(hdc, &line1_idx, &mut r1_idx, DT_RIGHT | DT_SINGLELINE);

        let _ = SetTextColor(hdc, M3_TEXT_SECONDARY);
        draw_text(hdc, &line2, &mut r2, DT_LEFT | DT_SINGLELINE);

        let _ = SetTextColor(hdc, M3_TEXT_MUTED);
        draw_text(hdc, &line3, &mut r3, DT_LEFT | DT_SINGLELINE);
    }
}

fn draw_filmstrip(state: &AppState, hdc: HDC, win_w: i32, win_h: i32, dpi: u32) {
    if state.catalog.items.is_empty() {
        return;
    }
    unsafe {
        let cur_idx = state.catalog.current_index.unwrap_or(0);
        let total = state.catalog.items.len();
        let (strip_y, strip_h, item_w, start_idx, end_idx) =
            get_filmstrip_layout(win_w, win_h, cur_idx, total, dpi);

        let strip_r = RECT {
            left: 0,
            top: strip_y,
            right: win_w,
            bottom: win_h,
        };

        // Dark background
        let bg_brush = CreateSolidBrush(COLORREF(0x00181818));
        let _ = FillRect(hdc, &strip_r, bg_brush);
        let _ = DeleteObject(bg_brush.into());

        // Top border
        let border_brush = CreateSolidBrush(M3_OUTLINE_VARIANT);
        let line_r = RECT {
            left: 0,
            top: strip_y,
            right: win_w,
            bottom: strip_y + 1,
        };
        let _ = FillRect(hdc, &line_r, border_brush);
        let _ = DeleteObject(border_brush.into());

        let thumb_w = scale_by_dpi(80, dpi);
        let thumb_h = scale_by_dpi(54, dpi);
        let pad_x = scale_by_dpi(5, dpi);
        let pad_y = scale_by_dpi(8, dpi);

        for (slot_idx, i) in (start_idx..end_idx).enumerate() {
            let item = &state.catalog.items[i];
            let item_x = (slot_idx as i32) * item_w + scale_by_dpi(10, dpi);
            let item_y = strip_y + pad_y;

            let card_r = RECT {
                left: item_x,
                top: item_y,
                right: item_x + thumb_w,
                bottom: item_y + thumb_h,
            };

            // Thumbnail card surface
            let card_brush = CreateSolidBrush(if i == cur_idx {
                COLORREF(0x003A3A3A)
            } else {
                COLORREF(0x00282828)
            });
            let _ = FillRect(hdc, &card_r, card_brush);
            let _ = DeleteObject(card_brush.into());

            // If decoded image is available in cache, draw miniature
            let img_id = ImageId(item.path.to_string_lossy().to_string());
            if let Some(dec) = state.cached_images.get(&img_id) {
                let scale = (thumb_w as f64 / dec.width as f64)
                    .min(thumb_h as f64 / dec.height as f64);
                let dw = (dec.width as f64 * scale).round() as i32;
                let dh = (dec.height as f64 * scale).round() as i32;
                let ox = item_x + (thumb_w - dw) / 2;
                let oy = item_y + (thumb_h - dh) / 2;

                let mut bi = BITMAPINFO::default();
                bi.bmiHeader.biSize = std::mem::size_of::<BITMAPINFOHEADER>() as u32;
                bi.bmiHeader.biWidth = dec.width as i32;
                bi.bmiHeader.biHeight = -(dec.height as i32);
                bi.bmiHeader.biPlanes = 1;
                bi.bmiHeader.biBitCount = 32;
                bi.bmiHeader.biCompression = BI_RGB.0;

                let _ = SetStretchBltMode(hdc, HALFTONE);
                let _ = StretchDIBits(
                    hdc,
                    ox,
                    oy,
                    dw,
                    dh,
                    0,
                    0,
                    dec.width as i32,
                    dec.height as i32,
                    Some(dec.pixels.as_ptr() as *const _),
                    &bi,
                    DIB_RGB_COLORS,
                    SRCCOPY,
                );
            }

            // Outline: accent border for currently selected image
            let outline_color = if i == cur_idx {
                COLORREF(0x00FF9933) // Accent
            } else if item.is_excluded {
                COLORREF(0x003333FF) // Excluded red
            } else {
                M3_OUTLINE
            };
            let outline_brush = CreateSolidBrush(outline_color);
            let _ = FrameRect(hdc, &card_r, outline_brush);
            let _ = DeleteObject(outline_brush.into());

            // Filename label below thumbnail
            let mut label_r = RECT {
                left: item_x - pad_x,
                top: item_y + thumb_h + scale_by_dpi(2, dpi),
                right: item_x + thumb_w + pad_x,
                bottom: strip_y + strip_h - scale_by_dpi(2, dpi),
            };
            let _ = SetBkMode(hdc, TRANSPARENT);
            let _ = SetTextColor(hdc, if i == cur_idx { M3_TEXT_PRIMARY } else { M3_TEXT_MUTED });
            draw_text(hdc, &item.filename, &mut label_r, DT_CENTER | DT_SINGLELINE | DT_END_ELLIPSIS);
        }
    }
}

fn draw_osd_notification(hdc: HDC, text: &str, win_w: i32, has_undo: bool, dpi: u32) {
    unsafe {
        let r = osd_notification_rect_dpi(text, win_w, has_undo, dpi);

        let brush = CreateSolidBrush(rgb(20, 36, 28)); // Deep emerald container
        let _ = FillRect(hdc, &r, brush);
        let _ = DeleteObject(brush.into());

        let border_brush = CreateSolidBrush(rgb(42, 85, 62)); // Subtle mint outline
        let _ = FrameRect(hdc, &r, border_brush);
        let _ = DeleteObject(border_brush.into());

        let _ = SetBkMode(hdc, TRANSPARENT);
        let _ = SetTextColor(hdc, M3_SECONDARY_MINT);

        let right_pad = scale_by_dpi(if has_undo { 120 } else { 16 }, dpi);
        let mut tr = RECT {
            left: r.left + scale_by_dpi(16, dpi),
            top: r.top,
            right: r.right - right_pad,
            bottom: r.bottom,
        };
        draw_text(hdc, text, &mut tr, DT_LEFT | DT_VCENTER | DT_SINGLELINE);

        if has_undo {
            let btn_r = osd_undo_button_rect_dpi(text, win_w, dpi);
            draw_modern_button(
                hdc,
                btn_r.left,
                btn_r.top,
                btn_r.right - btn_r.left,
                btn_r.bottom - btn_r.top,
                "元に戻す",
                ModernButtonStyle::SecondaryTonal,
                scale_by_dpi(6, dpi),
            );
        }
    }
}

fn draw_error_banner(hdc: HDC, errors: &[String], win_w: i32, dpi: u32) {
    unsafe {
        let bar_h = get_bar_height(dpi);
        let banner_h = scale_by_dpi(36, dpi);
        let r = RECT {
            left: 0,
            top: bar_h,
            right: win_w,
            bottom: bar_h + banner_h,
        };
        let brush = CreateSolidBrush(rgb(56, 18, 18)); // M3 Dark Crimson error container
        let _ = FillRect(hdc, &r, brush);
        let _ = DeleteObject(brush.into());

        let border_brush = CreateSolidBrush(rgb(110, 35, 35));
        let _ = FrameRect(hdc, &r, border_brush);
        let _ = DeleteObject(border_brush.into());

        let _ = SetBkMode(hdc, TRANSPARENT);
        let _ = SetTextColor(hdc, rgb(255, 218, 214));

        let msg = errors.join(" | ");
        let mut tr = RECT {
            left: scale_by_dpi(16, dpi),
            top: r.top,
            right: win_w - scale_by_dpi(50, dpi),
            bottom: r.bottom,
        };
        draw_text(hdc, &msg, &mut tr, DT_LEFT | DT_VCENTER | DT_SINGLELINE);
    }
}

fn draw_settings_modal(hdc: HDC, state: &AppState, win_w: i32, win_h: i32, dpi: u32) {
    unsafe {
        let layout = get_settings_modal_layout(win_w, win_h, dpi);
        let modal_r = layout.card_rect;
        let bx = modal_r.left;
        let by = modal_r.top;
        let box_w = modal_r.right - modal_r.left;

        // Material 3 Modal Card Surface
        let brush = CreateSolidBrush(M3_SURFACE_CARD);
        let _ = FillRect(hdc, &modal_r, brush);
        let _ = DeleteObject(brush.into());

        let border_brush = CreateSolidBrush(M3_OUTLINE);
        let _ = FrameRect(hdc, &modal_r, border_brush);
        let _ = DeleteObject(border_brush.into());

        // Header Divider
        let div_brush = CreateSolidBrush(M3_OUTLINE_VARIANT);
        let div_r = RECT {
            left: bx + scale_by_dpi(16, dpi),
            top: by + scale_by_dpi(50, dpi),
            right: bx + box_w - scale_by_dpi(16, dpi),
            bottom: by + scale_by_dpi(51, dpi),
        };
        let _ = FillRect(hdc, &div_r, div_brush);
        let _ = DeleteObject(div_brush.into());

        let _ = SetBkMode(hdc, TRANSPARENT);
        let _ = SetTextColor(hdc, M3_TEXT_PRIMARY);

        let mut tr = RECT {
            left: bx + scale_by_dpi(24, dpi),
            top: by + scale_by_dpi(16, dpi),
            right: bx + box_w - scale_by_dpi(24, dpi),
            bottom: by + scale_by_dpi(45, dpi),
        };
        draw_text(hdc, "設定", &mut tr, DT_LEFT | DT_SINGLELINE);

        let _ = SetTextColor(hdc, M3_TEXT_SECONDARY);

        let btn_h28 = scale_by_dpi(28, dpi);
        let r_radius = scale_by_dpi(6, dpi);

        // Row 1: Max Scale M
        let row1_y = by + scale_by_dpi(60, dpi);
        let mut r1 = RECT {
            left: bx + scale_by_dpi(24, dpi),
            top: row1_y + scale_by_dpi(5, dpi),
            right: bx + scale_by_dpi(150, dpi),
            bottom: row1_y + btn_h28,
        };
        draw_text(hdc, "最大拡大率 M:", &mut r1, DT_LEFT | DT_SINGLELINE);
        let minus_x = bx + scale_by_dpi(160, dpi);
        let minus_w = scale_by_dpi(40, dpi);
        let val_x = bx + scale_by_dpi(210, dpi);
        let val_w = scale_by_dpi(60, dpi);
        let plus_x = bx + scale_by_dpi(280, dpi);
        let plus_w = scale_by_dpi(40, dpi);
        draw_modern_button(
            hdc,
            minus_x,
            row1_y,
            minus_w,
            btn_h28,
            "-",
            ModernButtonStyle::SecondaryTonal,
            r_radius,
        );
        let m_str = format!("{:.2}", state.config.layout.max_scale);
        draw_modern_button(
            hdc,
            val_x,
            row1_y,
            val_w,
            btn_h28,
            &m_str,
            ModernButtonStyle::SegmentActive,
            r_radius,
        );
        draw_modern_button(
            hdc,
            plus_x,
            row1_y,
            plus_w,
            btn_h28,
            "+",
            ModernButtonStyle::SecondaryTonal,
            r_radius,
        );

        // Row 2: Border px
        let row2_y = by + scale_by_dpi(105, dpi);
        let mut r2 = RECT {
            left: bx + scale_by_dpi(24, dpi),
            top: row2_y + scale_by_dpi(5, dpi),
            right: bx + scale_by_dpi(150, dpi),
            bottom: row2_y + btn_h28,
        };
        draw_text(hdc, "境界線幅:", &mut r2, DT_LEFT | DT_SINGLELINE);
        draw_modern_button(
            hdc,
            minus_x,
            row2_y,
            minus_w,
            btn_h28,
            "-",
            ModernButtonStyle::SecondaryTonal,
            r_radius,
        );
        let b_str = format!("{} px", state.config.layout.border_px);
        draw_modern_button(
            hdc,
            val_x,
            row2_y,
            val_w,
            btn_h28,
            &b_str,
            ModernButtonStyle::SegmentActive,
            r_radius,
        );
        draw_modern_button(
            hdc,
            plus_x,
            row2_y,
            plus_w,
            btn_h28,
            "+",
            ModernButtonStyle::SecondaryTonal,
            r_radius,
        );

        // Row 3: Random split mode
        let row3_y = by + scale_by_dpi(150, dpi);
        let mut r3 = RECT {
            left: bx + scale_by_dpi(24, dpi),
            top: row3_y + scale_by_dpi(5, dpi),
            right: bx + scale_by_dpi(150, dpi),
            bottom: row3_y + btn_h28,
        };
        draw_text(hdc, "ランダム分割:", &mut r3, DT_LEFT | DT_SINGLELINE);
        let r_mode = state.config.random.mode.as_str();
        let modes = [
            (
                "自動",
                "auto",
                bx + scale_by_dpi(160, dpi),
                scale_by_dpi(60, dpi),
            ),
            (
                "1枠",
                "single",
                bx + scale_by_dpi(225, dpi),
                scale_by_dpi(50, dpi),
            ),
            (
                "2枠",
                "split2",
                bx + scale_by_dpi(280, dpi),
                scale_by_dpi(50, dpi),
            ),
            (
                "3枠",
                "split3",
                bx + scale_by_dpi(335, dpi),
                scale_by_dpi(50, dpi),
            ),
        ];
        for (lbl, val, btn_x, btn_w) in modes {
            let is_sel = r_mode == val;
            let style = if is_sel {
                ModernButtonStyle::SegmentActive
            } else {
                ModernButtonStyle::SegmentInactive
            };
            draw_modern_button(hdc, btn_x, row3_y, btn_w, btn_h28, lbl, style, r_radius);
        }

        // Row 4: Schedule enabled
        let row4_y = by + scale_by_dpi(195, dpi);
        let mut r4 = RECT {
            left: bx + scale_by_dpi(24, dpi),
            top: row4_y + scale_by_dpi(5, dpi),
            right: bx + scale_by_dpi(150, dpi),
            bottom: row4_y + btn_h28,
        };
        draw_text(hdc, "定期更新:", &mut r4, DT_LEFT | DT_SINGLELINE);
        let en_style = if state.config.schedule.enabled {
            ModernButtonStyle::SegmentActive
        } else {
            ModernButtonStyle::SegmentInactive
        };
        let en_str = if state.config.schedule.enabled {
            "有効 (ON)"
        } else {
            "無効 (OFF)"
        };
        let en_w = scale_by_dpi(100, dpi);
        draw_modern_button(
            hdc,
            bx + scale_by_dpi(160, dpi),
            row4_y,
            en_w,
            btn_h28,
            en_str,
            en_style,
            r_radius,
        );

        // Row 5: Schedule on logon
        let row5_y = by + scale_by_dpi(240, dpi);
        let mut r5 = RECT {
            left: bx + scale_by_dpi(24, dpi),
            top: row5_y + scale_by_dpi(5, dpi),
            right: bx + scale_by_dpi(150, dpi),
            bottom: row5_y + btn_h28,
        };
        draw_text(hdc, "ログオン時更新:", &mut r5, DT_LEFT | DT_SINGLELINE);
        let log_style = if state.config.schedule.on_logon {
            ModernButtonStyle::SegmentActive
        } else {
            ModernButtonStyle::SegmentInactive
        };
        let log_str = if state.config.schedule.on_logon {
            "有効 (ON)"
        } else {
            "無効 (OFF)"
        };
        draw_modern_button(
            hdc,
            bx + scale_by_dpi(160, dpi),
            row5_y,
            en_w,
            btn_h28,
            log_str,
            log_style,
            r_radius,
        );

        // Row 6: Schedule interval
        let row6_y = by + scale_by_dpi(285, dpi);
        let mut r6 = RECT {
            left: bx + scale_by_dpi(24, dpi),
            top: row6_y + scale_by_dpi(5, dpi),
            right: bx + scale_by_dpi(150, dpi),
            bottom: row6_y + btn_h28,
        };
        draw_text(hdc, "実行間隔:", &mut r6, DT_LEFT | DT_SINGLELINE);
        let int_str = match state.config.schedule.interval_minutes {
            0 => "間隔なし (クリックで変更)".to_string(),
            60 => "1時間 (クリックで変更)".to_string(),
            m if m >= 60 => format!("{}時間 (クリックで変更)", m / 60),
            m => format!("{}分 (クリックで変更)", m),
        };
        let int_w = scale_by_dpi(200, dpi);
        draw_modern_button(
            hdc,
            bx + scale_by_dpi(160, dpi),
            row6_y,
            int_w,
            btn_h28,
            &int_str,
            ModernButtonStyle::SecondaryTonal,
            r_radius,
        );

        // Close button: Primary Pill Button
        let cl_w = layout.close_btn_rect.right - layout.close_btn_rect.left;
        let cl_h = layout.close_btn_rect.bottom - layout.close_btn_rect.top;
        draw_modern_button(
            hdc,
            layout.close_btn_rect.left,
            layout.close_btn_rect.top,
            cl_w,
            cl_h,
            "閉じる",
            ModernButtonStyle::Primary,
            scale_by_dpi(8, dpi),
        );
    }
}

fn draw_exclude_list_modal(hdc: HDC, state: &AppState, win_w: i32, win_h: i32, dpi: u32) {
    unsafe {
        let layout = get_exclude_list_modal_layout(win_w, win_h, dpi);
        let modal_r = layout.card_rect;
        let bx = modal_r.left;
        let by = modal_r.top;
        let box_w = modal_r.right - modal_r.left;

        // Material 3 Modal Card Surface
        let brush = CreateSolidBrush(M3_SURFACE_CARD);
        let _ = FillRect(hdc, &modal_r, brush);
        let _ = DeleteObject(brush.into());

        let border_brush = CreateSolidBrush(M3_OUTLINE);
        let _ = FrameRect(hdc, &modal_r, border_brush);
        let _ = DeleteObject(border_brush.into());

        // Header Divider
        let div_brush = CreateSolidBrush(M3_OUTLINE_VARIANT);
        let div_r = RECT {
            left: bx + scale_by_dpi(16, dpi),
            top: by + scale_by_dpi(50, dpi),
            right: bx + box_w - scale_by_dpi(16, dpi),
            bottom: by + scale_by_dpi(51, dpi),
        };
        let _ = FillRect(hdc, &div_r, div_brush);
        let _ = DeleteObject(div_brush.into());

        let _ = SetBkMode(hdc, TRANSPARENT);
        let _ = SetTextColor(hdc, M3_TEXT_PRIMARY);

        let mut tr = RECT {
            left: bx + scale_by_dpi(24, dpi),
            top: by + scale_by_dpi(16, dpi),
            right: bx + box_w - scale_by_dpi(24, dpi),
            bottom: by + scale_by_dpi(45, dpi),
        };
        draw_text(hdc, "除外画像一覧", &mut tr, DT_LEFT | DT_SINGLELINE);

        let excluded_in_folder: Vec<(&str, &PathBuf)> = state
            .catalog
            .items
            .iter()
            .filter(|it| it.is_excluded)
            .map(|it| (it.filename.as_str(), &it.path))
            .collect();

        let r_radius = scale_by_dpi(6, dpi);
        if excluded_in_folder.is_empty() {
            let mut r_empty = RECT {
                left: bx + scale_by_dpi(30, dpi),
                top: by + scale_by_dpi(70, dpi),
                right: bx + box_w - scale_by_dpi(30, dpi),
                bottom: by + scale_by_dpi(100, dpi),
            };
            let _ = SetTextColor(hdc, M3_TEXT_MUTED);
            draw_text(
                hdc,
                "現在のフォルダに除外画像はありません。",
                &mut r_empty,
                DT_LEFT | DT_SINGLELINE,
            );
        } else {
            let _ = SetTextColor(hdc, M3_TEXT_PRIMARY);
            for (i, (fname, _)) in excluded_in_folder.iter().take(5).enumerate() {
                let row_y = by + scale_by_dpi(60, dpi) + (i as i32) * scale_by_dpi(36, dpi);
                let mut r_item = RECT {
                    left: bx + scale_by_dpi(30, dpi),
                    top: row_y + scale_by_dpi(4, dpi),
                    right: bx + scale_by_dpi(420, dpi),
                    bottom: row_y + scale_by_dpi(28, dpi),
                };
                draw_text(hdc, fname, &mut r_item, DT_LEFT | DT_SINGLELINE);
                draw_modern_button(
                    hdc,
                    bx + scale_by_dpi(440, dpi),
                    row_y,
                    scale_by_dpi(60, dpi),
                    scale_by_dpi(26, dpi),
                    "解除",
                    ModernButtonStyle::SecondaryTonal,
                    r_radius,
                );
            }
        }

        draw_modern_button(
            hdc,
            bx + scale_by_dpi(30, dpi),
            by + scale_by_dpi(250, dpi),
            scale_by_dpi(220, dpi),
            scale_by_dpi(30, dpi),
            "このフォルダを一括解除",
            ModernButtonStyle::SecondaryTonal,
            r_radius,
        );

        let total_ex = state.catalog.excluded_paths.len();
        let other_count = total_ex.saturating_sub(excluded_in_folder.len());
        let mut r_other = RECT {
            left: bx + scale_by_dpi(30, dpi),
            top: by + scale_by_dpi(295, dpi),
            right: bx + box_w - scale_by_dpi(30, dpi),
            bottom: by + scale_by_dpi(320, dpi),
        };
        let _ = SetTextColor(hdc, M3_TEXT_SECONDARY);
        let other_txt = format!("他フォルダの除外パス: {}件", other_count);
        draw_text(hdc, &other_txt, &mut r_other, DT_LEFT | DT_SINGLELINE);

        draw_modern_button(
            hdc,
            bx + scale_by_dpi(30, dpi),
            by + scale_by_dpi(325, dpi),
            scale_by_dpi(200, dpi),
            scale_by_dpi(30, dpi),
            "存在しないパスを整理",
            ModernButtonStyle::SecondaryTonal,
            r_radius,
        );
        draw_modern_button(
            hdc,
            bx + scale_by_dpi(245, dpi),
            by + scale_by_dpi(325, dpi),
            scale_by_dpi(200, dpi),
            scale_by_dpi(30, dpi),
            "他フォルダ分を一括解除",
            ModernButtonStyle::SecondaryTonal,
            r_radius,
        );

        let cl_w = layout.close_btn_rect.right - layout.close_btn_rect.left;
        let cl_h = layout.close_btn_rect.bottom - layout.close_btn_rect.top;
        draw_modern_button(
            hdc,
            layout.close_btn_rect.left,
            layout.close_btn_rect.top,
            cl_w,
            cl_h,
            "閉じる",
            ModernButtonStyle::Primary,
            scale_by_dpi(8, dpi),
        );
    }
}

fn draw_shortcuts_modal(hdc: HDC, win_w: i32, win_h: i32, dpi: u32) {
    unsafe {
        let layout = get_shortcuts_modal_layout(win_w, win_h, dpi);
        let modal_r = layout.card_rect;
        let bx = modal_r.left;
        let by = modal_r.top;
        let box_w = modal_r.right - modal_r.left;

        // Material 3 Modal Card Surface
        let brush = CreateSolidBrush(M3_SURFACE_CARD);
        let _ = FillRect(hdc, &modal_r, brush);
        let _ = DeleteObject(brush.into());

        let border_brush = CreateSolidBrush(M3_OUTLINE);
        let _ = FrameRect(hdc, &modal_r, border_brush);
        let _ = DeleteObject(border_brush.into());

        // Header Divider
        let div_brush = CreateSolidBrush(M3_OUTLINE_VARIANT);
        let div_r = RECT {
            left: bx + scale_by_dpi(16, dpi),
            top: by + scale_by_dpi(50, dpi),
            right: bx + box_w - scale_by_dpi(16, dpi),
            bottom: by + scale_by_dpi(51, dpi),
        };
        let _ = FillRect(hdc, &div_r, div_brush);
        let _ = DeleteObject(div_brush.into());

        let _ = SetBkMode(hdc, TRANSPARENT);
        let _ = SetTextColor(hdc, M3_TEXT_PRIMARY);

        let mut tr = RECT {
            left: bx + scale_by_dpi(24, dpi),
            top: by + scale_by_dpi(16, dpi),
            right: bx + box_w - scale_by_dpi(24, dpi),
            bottom: by + scale_by_dpi(45, dpi),
        };
        draw_text(hdc, "操作一覧", &mut tr, DT_LEFT | DT_SINGLELINE);

        let lines = [
            ("ホイール / → / ← / ↑ / ↓", "画像送り"),
            ("Shift+R / Ctrl+R", "画像のランダム入替"),
            ("Home / End", "先頭 / 末尾"),
            ("V / 中クリック", "通常 / 合成 切替"),
            ("Z / 0", "通常表示: 等倍切替 / リセット"),
            ("Tab / Shift+Tab", "枠選択"),
            ("Space", "選択枠のピン留め切替"),
            ("Shift+Space", "枠の入れ替え"),
            ("左ドラッグ / Shift+矢印", "パン調整"),
            ("C", "パン位置リセット"),
            ("A", "スマートパン (被写体/顔自動合わせ)"),
            ("B", "サイドバー (ファイル一覧) 切替"),
            ("Ctrl+↑ / Ctrl+↓", "前のフォルダ / 次のフォルダへ移動"),
            ("Alt+クリック (見出し)", "フォルダ単独展開 (他を閉じる)"),
            ("T", "フィルムストリップ表示切替"),
            ("L / Shift+L", "分割モード切替"),
            ("Ctrl+ホイール / + / -", "拡大率変更 / ズーム"),
            ("D", "背景ぼかし減光切替 (0〜60%)"),
            ("P / Ctrl+P", "プリセット切替 / 保存"),
            ("1〜9", "モニター選択"),
            ("Enter", "壁紙適用"),
            ("Ctrl+1〜9", "指定モニターへ即時適用"),
            ("R", "全モニターランダム更新"),
            ("Ctrl+Z", "元に戻す"),
            ("S / Ctrl+S", "PNG保存"),
            ("Del", "除外切替"),
            ("Ctrl+O", "フォルダ選択"),
            ("Ctrl+Shift+O", "ファイルの場所を開く"),
            ("F5", "再読込"),
            ("I", "情報表示切替"),
            ("F11 / Alt+Enter", "全画面切替"),
            ("Esc", "閉じる"),
        ];

        let line_h = scale_by_dpi(17, dpi);
        let split_x = bx + scale_by_dpi(240, dpi);
        for (i, (key, desc)) in lines.iter().enumerate() {
            let row_top = by + scale_by_dpi(58, dpi) + (i as i32) * line_h;
            let mut lr_key = RECT {
                left: bx + scale_by_dpi(24, dpi),
                top: row_top,
                right: split_x,
                bottom: row_top + line_h,
            };
            let mut lr_desc = RECT {
                left: split_x,
                top: row_top,
                right: bx + box_w - scale_by_dpi(24, dpi),
                bottom: row_top + line_h,
            };
            let _ = SetTextColor(hdc, M3_PRIMARY_ACCENT);
            draw_text(hdc, key, &mut lr_key, DT_LEFT | DT_SINGLELINE);
            let _ = SetTextColor(hdc, M3_TEXT_SECONDARY);
            draw_text(hdc, desc, &mut lr_desc, DT_LEFT | DT_SINGLELINE);
        }

        let cl_w = layout.close_btn_rect.right - layout.close_btn_rect.left;
        let cl_h = layout.close_btn_rect.bottom - layout.close_btn_rect.top;
        draw_modern_button(
            hdc,
            layout.close_btn_rect.left,
            layout.close_btn_rect.top,
            cl_w,
            cl_h,
            "閉じる",
            ModernButtonStyle::Primary,
            scale_by_dpi(8, dpi),
        );
    }
}

/// # Safety
/// Caller must pass a valid device context `hdc` and valid pointer `rect`.
pub unsafe fn draw_text(hdc: HDC, text: &str, rect: &mut RECT, format: DRAW_TEXT_FORMAT) {
    unsafe {
        let mut wide: Vec<u16> = text.encode_utf16().collect();
        if !wide.is_empty() {
            let _ = DrawTextW(hdc, &mut wide, rect as *mut _, format);
        }
    }
}

pub(crate) fn show_monitor_overlays(monitors: &[MonitorInfo]) {
    unsafe {
        let instance = windows::Win32::System::LibraryLoader::GetModuleHandleW(None).unwrap();
        for m in monitors {
            let cx = (m.rect.left + m.rect.right) / 2;
            let cy = (m.rect.top + m.rect.bottom) / 2;
            let size = 160;

            // Create window without WS_VISIBLE first to ensure GWLP_USERDATA is set before paint
            let hwnd = CreateWindowExW(
                WS_EX_TOPMOST | WS_EX_TOOLWINDOW,
                OVERLAY_CLASS_NAME,
                PCWSTR(std::ptr::null()),
                WS_POPUP,
                cx - size / 2,
                cy - size / 2,
                size,
                size,
                None,
                None,
                Some(instance.into()),
                None,
            );

            if let Ok(h) = hwnd {
                let _ = SetWindowLongPtrW(h, GWLP_USERDATA, m.number as isize);
                let _ = SetTimer(Some(h), 999, 1500, None);
                let _ = ShowWindow(h, SW_SHOW);
                let _ = UpdateWindow(h);
            }
        }
    }
}

pub(crate) unsafe extern "system" fn overlay_wnd_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    unsafe {
        match msg {
            WM_TIMER => {
                let _ = DestroyWindow(hwnd);
                LRESULT(0)
            }
            WM_PAINT => {
                let mut ps = PAINTSTRUCT::default();
                let hdc = BeginPaint(hwnd, &mut ps);
                let mut cr = RECT::default();
                let _ = GetClientRect(hwnd, &mut cr);

                // Google Material 3 Elevated Display Indicator HUD
                draw_round_rect_fill_stroke(hdc, &cr, M3_SURFACE_CARD, Some(M3_PRIMARY_ACCENT), 24);

                let num = GetWindowLongPtrW(hwnd, GWLP_USERDATA);
                let text = format!("{}", num);

                let _ = SetBkMode(hdc, TRANSPARENT);
                let _ = SetTextColor(hdc, M3_PRIMARY_ACCENT);

                let font = create_gui_font(hwnd, 54, 700);
                let old_font = SelectObject(hdc, font.into());
                let mut tr = cr;
                draw_text(hdc, &text, &mut tr, DT_CENTER | DT_VCENTER | DT_SINGLELINE);
                let _ = SelectObject(hdc, old_font);
                let _ = DeleteObject(font.into());

                let _ = EndPaint(hwnd, &ps);
                LRESULT(0)
            }
            _ => DefWindowProcW(hwnd, msg, wparam, lparam),
        }
    }
}

fn draw_sidebar(state: &AppState, hdc: HDC, win_w: i32, win_h: i32, bar_h: i32, dpi: u32) {
    unsafe {
        let pinned_paths: HashSet<PathBuf> = state
            .layout_frames
            .iter()
            .filter(|f| f.pinned)
            .filter_map(|f| f.image.as_ref())
            .map(|img| PathBuf::from(&img.id.0))
            .collect();

        let layout = compute_sidebar_layout(
            &state.catalog,
            state.current_selected_catalog_index(),
            &pinned_paths,
            &state.collapsed_folders,
            state.sidebar_scroll_offset,
            win_w,
            win_h,
            bar_h,
            state.show_filmstrip,
            state.view_mode,
            state.layout_frames.len(),
            state.selected_frame_idx,
            dpi,
        );

        // Sidebar background
        let bg_brush = CreateSolidBrush(M3_SURFACE_BAR);
        let _ = FillRect(hdc, &layout.bounds, bg_brush);
        let _ = DeleteObject(bg_brush.into());

        // Right vertical border
        let border_brush = CreateSolidBrush(M3_OUTLINE_VARIANT);
        let line_r = RECT {
            left: layout.bounds.right - 1,
            top: layout.bounds.top,
            right: layout.bounds.right,
            bottom: layout.bounds.bottom,
        };
        let _ = FillRect(hdc, &line_r, border_brush);

        // Header background & border
        let hdr_line_r = RECT {
            left: layout.header_rect.left,
            top: layout.header_rect.bottom - 1,
            right: layout.header_rect.right,
            bottom: layout.header_rect.bottom,
        };
        let _ = FillRect(hdc, &hdr_line_r, border_brush);
        let _ = DeleteObject(border_brush.into());

        // Header Title
        let total_items = state.catalog.items.len();
        let header_txt = format!("ファイル一覧 ({}枚)", total_items);
        let mut title_r = RECT {
            left: layout.header_rect.left + scale_by_dpi(12, dpi),
            top: layout.header_rect.top,
            right: layout.focus_current_btn_rect.left - scale_by_dpi(4, dpi),
            bottom: layout.header_rect.bottom,
        };
        let _ = SetBkMode(hdc, TRANSPARENT);
        let _ = SetTextColor(hdc, M3_TEXT_PRIMARY);

        let bold_font = create_gui_font(state.hwnd, 10, 600);
        let old_font = SelectObject(hdc, bold_font.into());
        draw_text(
            hdc,
            &header_txt,
            &mut title_r,
            DT_LEFT | DT_VCENTER | DT_SINGLELINE | DT_END_ELLIPSIS,
        );

        let btn_radius = scale_by_dpi(6, dpi);

        // Focus current folder button (◎)
        let fc_w = layout.focus_current_btn_rect.right - layout.focus_current_btn_rect.left;
        let fc_h = layout.focus_current_btn_rect.bottom - layout.focus_current_btn_rect.top;
        draw_modern_button(
            hdc,
            layout.focus_current_btn_rect.left,
            layout.focus_current_btn_rect.top,
            fc_w,
            fc_h,
            "◎",
            ModernButtonStyle::Ghost,
            btn_radius,
        );

        // Collapse all button (⊟)
        let col_w = layout.collapse_all_btn_rect.right - layout.collapse_all_btn_rect.left;
        let col_h = layout.collapse_all_btn_rect.bottom - layout.collapse_all_btn_rect.top;
        draw_modern_button(
            hdc,
            layout.collapse_all_btn_rect.left,
            layout.collapse_all_btn_rect.top,
            col_w,
            col_h,
            "⊟",
            ModernButtonStyle::Ghost,
            btn_radius,
        );

        // Close button (✕)
        let cl_w = layout.close_btn_rect.right - layout.close_btn_rect.left;
        let cl_h = layout.close_btn_rect.bottom - layout.close_btn_rect.top;
        draw_modern_button(
            hdc,
            layout.close_btn_rect.left,
            layout.close_btn_rect.top,
            cl_w,
            cl_h,
            "✕",
            ModernButtonStyle::Ghost,
            btn_radius,
        );

        // Restore normal font for list & sub-bars
        let _ = SelectObject(hdc, old_font);
        let _ = DeleteObject(bold_font.into());

        // Target frame selector bar (Composite mode)
        if let Some((bar_r, buttons)) = &layout.target_frame_bar {
            let bar_bg = CreateSolidBrush(M3_SURFACE_CARD);
            let _ = FillRect(hdc, bar_r, bar_bg);
            let _ = DeleteObject(bar_bg.into());

            let sep_brush = CreateSolidBrush(M3_OUTLINE_VARIANT);
            let sep_r = RECT {
                left: bar_r.left,
                top: bar_r.bottom - 1,
                right: bar_r.right,
                bottom: bar_r.bottom,
            };
            let _ = FillRect(hdc, &sep_r, sep_brush);
            let _ = DeleteObject(sep_brush.into());

            let mut label_r = RECT {
                left: bar_r.left + scale_by_dpi(10, dpi),
                top: bar_r.top,
                right: bar_r.left + scale_by_dpi(58, dpi),
                bottom: bar_r.bottom,
            };
            let _ = SetTextColor(hdc, M3_TEXT_SECONDARY);
            draw_text(
                hdc,
                "反映先:",
                &mut label_r,
                DT_LEFT | DT_VCENTER | DT_SINGLELINE,
            );

            let bar_btn_radius = scale_by_dpi(4, dpi);
            for (_f_idx, label, b_rect, is_active) in buttons {
                let style = if *is_active {
                    ModernButtonStyle::SegmentActive
                } else {
                    ModernButtonStyle::SegmentInactive
                };
                let bw = b_rect.right - b_rect.left;
                let bh = b_rect.bottom - b_rect.top;
                draw_modern_button(
                    hdc,
                    b_rect.left,
                    b_rect.top,
                    bw,
                    bh,
                    label,
                    style,
                    bar_btn_radius,
                );
            }
        }

        // Clip list area so scrolling rows don't bleed into header or filmstrip
        let _ = IntersectClipRect(
            hdc,
            layout.list_rect.left,
            layout.list_rect.top,
            layout.list_rect.right,
            layout.list_rect.bottom,
        );

        let thumb_size = scale_by_dpi(34, dpi);
        let pad_x = scale_by_dpi(8, dpi);

        for row in &layout.rows {
            match &row.kind {
                SidebarRowKind::FolderHeader {
                    display_name,
                    item_count,
                    is_collapsed,
                    ..
                } => {
                    let folder_bg = CreateSolidBrush(M3_SURFACE_CARD);
                    let _ = FillRect(hdc, &row.rect, folder_bg);
                    let _ = DeleteObject(folder_bg.into());

                    let sep_brush = CreateSolidBrush(M3_OUTLINE_VARIANT);
                    let sep_r = RECT {
                        left: row.rect.left,
                        top: row.rect.bottom - 1,
                        right: row.rect.right,
                        bottom: row.rect.bottom,
                    };
                    let _ = FillRect(hdc, &sep_r, sep_brush);
                    let _ = DeleteObject(sep_brush.into());

                    let arrow = if *is_collapsed { "▶ " } else { "▼ " };
                    let folder_label = format!("{}{} ({})", arrow, display_name, item_count);
                    let mut text_r = RECT {
                        left: row.rect.left + pad_x,
                        top: row.rect.top,
                        right: row.rect.right - pad_x,
                        bottom: row.rect.bottom,
                    };
                    let _ = SetTextColor(hdc, M3_PRIMARY_ACCENT);
                    draw_text(
                        hdc,
                        &folder_label,
                        &mut text_r,
                        DT_LEFT | DT_VCENTER | DT_SINGLELINE | DT_END_ELLIPSIS,
                    );
                }
                SidebarRowKind::FileItem {
                    catalog_index,
                    filename,
                    is_selected,
                    is_pinned,
                    is_excluded,
                    frame_targets,
                } => {
                    if *is_selected {
                        let sel_brush = CreateSolidBrush(M3_SURFACE_HIGH);
                        let _ = FillRect(hdc, &row.rect, sel_brush);
                        let _ = DeleteObject(sel_brush.into());

                        let ind_brush = CreateSolidBrush(M3_PRIMARY_BLUE);
                        let ind_r = RECT {
                            left: row.rect.left,
                            top: row.rect.top,
                            right: row.rect.left + scale_by_dpi(3, dpi),
                            bottom: row.rect.bottom,
                        };
                        let _ = FillRect(hdc, &ind_r, ind_brush);
                        let _ = DeleteObject(ind_brush.into());
                    }

                    let thumb_x = row.rect.left + pad_x + scale_by_dpi(4, dpi);
                    let thumb_y = row.rect.top + (row.rect.bottom - row.rect.top - thumb_size) / 2;
                    let thumb_r = RECT {
                        left: thumb_x,
                        top: thumb_y,
                        right: thumb_x + thumb_size,
                        bottom: thumb_y + thumb_size,
                    };

                    let card_brush = CreateSolidBrush(if *is_selected {
                        COLORREF(0x003A3A3A)
                    } else {
                        COLORREF(0x0022252A)
                    });
                    let _ = FillRect(hdc, &thumb_r, card_brush);
                    let _ = DeleteObject(card_brush.into());

                    let item = &state.catalog.items[*catalog_index];
                    let img_id = ImageId(item.path.to_string_lossy().to_string());
                    if let Some(dec) = state.cached_images.get(&img_id) {
                        let scale = (thumb_size as f64 / dec.width as f64)
                            .min(thumb_size as f64 / dec.height as f64);
                        let dw = (dec.width as f64 * scale).round() as i32;
                        let dh = (dec.height as f64 * scale).round() as i32;
                        let ox = thumb_x + (thumb_size - dw) / 2;
                        let oy = thumb_y + (thumb_size - dh) / 2;

                        let mut bi = BITMAPINFO::default();
                        bi.bmiHeader.biSize = std::mem::size_of::<BITMAPINFOHEADER>() as u32;
                        bi.bmiHeader.biWidth = dec.width as i32;
                        bi.bmiHeader.biHeight = -(dec.height as i32);
                        bi.bmiHeader.biPlanes = 1;
                        bi.bmiHeader.biBitCount = 32;
                        bi.bmiHeader.biCompression = BI_RGB.0;

                        let _ = SetStretchBltMode(hdc, HALFTONE);
                        let _ = StretchDIBits(
                            hdc,
                            ox,
                            oy,
                            dw,
                            dh,
                            0,
                            0,
                            dec.width as i32,
                            dec.height as i32,
                            Some(dec.pixels.as_ptr() as *const _),
                            &bi,
                            DIB_RGB_COLORS,
                            SRCCOPY,
                        );
                    }

                    let outline_color = if *is_selected {
                        COLORREF(0x00FF9933)
                    } else if *is_excluded {
                        COLORREF(0x003333FF)
                    } else {
                        M3_OUTLINE_VARIANT
                    };
                    let outline_brush = CreateSolidBrush(outline_color);
                    let _ = FrameRect(hdc, &thumb_r, outline_brush);
                    let _ = DeleteObject(outline_brush.into());

                    let badge_reserve = if *is_pinned || *is_excluded {
                        scale_by_dpi(28, dpi)
                    } else {
                        scale_by_dpi(8, dpi)
                    };

                    let mut file_right = row.rect.right - badge_reserve;
                    if let Some(first_btn) = frame_targets.first() {
                        file_right = first_btn.2.left - scale_by_dpi(4, dpi);
                    }

                    let text_x = thumb_x + thumb_size + scale_by_dpi(8, dpi);
                    let mut file_r = RECT {
                        left: text_x,
                        top: row.rect.top,
                        right: file_right,
                        bottom: row.rect.bottom,
                    };

                    let text_color = if *is_selected {
                        M3_TEXT_PRIMARY
                    } else if *is_excluded {
                        M3_TEXT_MUTED
                    } else {
                        M3_TEXT_SECONDARY
                    };
                    let _ = SetTextColor(hdc, text_color);
                    draw_text(
                        hdc,
                        filename,
                        &mut file_r,
                        DT_LEFT | DT_VCENTER | DT_SINGLELINE | DT_END_ELLIPSIS,
                    );

                    let frame_btn_radius = scale_by_dpi(3, dpi);
                    for (_f_idx, label, btn_rect) in frame_targets {
                        let bw = btn_rect.right - btn_rect.left;
                        let bh = btn_rect.bottom - btn_rect.top;
                        draw_modern_button(
                            hdc,
                            btn_rect.left,
                            btn_rect.top,
                            bw,
                            bh,
                            label,
                            ModernButtonStyle::Ghost,
                            frame_btn_radius,
                        );
                    }

                    if *is_pinned {
                        let mut badge_r = RECT {
                            left: row.rect.right - scale_by_dpi(26, dpi),
                            top: row.rect.top,
                            right: row.rect.right - scale_by_dpi(4, dpi),
                            bottom: row.rect.bottom,
                        };
                        let _ = SetTextColor(hdc, M3_CYAN_PINNED);
                        draw_text(hdc, "📌", &mut badge_r, DT_CENTER | DT_VCENTER | DT_SINGLELINE);
                    } else if *is_excluded {
                        let mut badge_r = RECT {
                            left: row.rect.right - scale_by_dpi(26, dpi),
                            top: row.rect.top,
                            right: row.rect.right - scale_by_dpi(4, dpi),
                            bottom: row.rect.bottom,
                        };
                        let _ = SetTextColor(hdc, COLORREF(0x005555FF));
                        draw_text(hdc, "🚫", &mut badge_r, DT_CENTER | DT_VCENTER | DT_SINGLELINE);
                    }
                }
            }
        }

        let _ = SelectClipRgn(hdc, None);

        if let (Some(bar_r), Some(thumb_r)) = (layout.scrollbar_rect, layout.scrollbar_thumb_rect) {
            let bar_brush = CreateSolidBrush(COLORREF(0x00202327));
            let _ = FillRect(hdc, &bar_r, bar_brush);
            let _ = DeleteObject(bar_brush.into());

            let thumb_brush = CreateSolidBrush(M3_OUTLINE);
            let _ = FillRect(hdc, &thumb_r, thumb_brush);
            let _ = DeleteObject(thumb_brush.into());
        }
    }
}

