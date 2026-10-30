use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Dwm::*;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::System::Com::*;
use windows::Win32::System::DataExchange::COPYDATASTRUCT;
use windows::Win32::UI::HiDpi::*;
use windows::Win32::UI::Input::KeyboardAndMouse::*;
use windows::Win32::UI::Shell::*;
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::*;

use crate::catalog::normalize_path;
use crate::monitor::enumerate_monitors;
use crate::ui::layout_calc::*;
use crate::ui::render::*;
use crate::ui::state::AppState;
use crate::ui::types::*;
// Global pointer to AppState during Win32 event loop
static mut APP_STATE_PTR: *mut AppState = std::ptr::null_mut();

pub fn run_gui(mut state: AppState, class_name: &str) -> Result<()> {
    unsafe {
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE);
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        let instance = windows::Win32::System::LibraryLoader::GetModuleHandleW(None)?;

        let wide_class: Vec<u16> = class_name
            .encode_utf16()
            .chain(std::iter::once(0))
            .collect();

        // Load application icon from embedded resource (ID 1)
        let h_icon = LoadImageW(
            Some(instance.into()),
            PCWSTR(1 as _),
            IMAGE_ICON,
            GetSystemMetrics(SM_CXICON),
            GetSystemMetrics(SM_CYICON),
            LR_DEFAULTCOLOR | LR_SHARED,
        )
        .ok()
        .map(|handle| HICON(handle.0))
        .unwrap_or_default();

        let h_icon_sm = LoadImageW(
            Some(instance.into()),
            PCWSTR(1 as _),
            IMAGE_ICON,
            GetSystemMetrics(SM_CXSMICON),
            GetSystemMetrics(SM_CYSMICON),
            LR_DEFAULTCOLOR | LR_SHARED,
        )
        .ok()
        .map(|handle| HICON(handle.0))
        .unwrap_or_default();

        // Register main window class
        let wc = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            style: CS_HREDRAW | CS_VREDRAW | CS_DBLCLKS,
            lpfnWndProc: Some(wnd_proc),
            hInstance: instance.into(),
            hIcon: h_icon,
            hCursor: LoadCursorW(None, IDC_ARROW)?,
            hbrBackground: HBRUSH(GetStockObject(BLACK_BRUSH).0),
            lpszClassName: PCWSTR(wide_class.as_ptr()),
            hIconSm: h_icon_sm,
            ..Default::default()
        };
        let _ = RegisterClassExW(&wc);

        // Register overlay window class
        let wc_ov = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            style: CS_HREDRAW | CS_VREDRAW,
            lpfnWndProc: Some(overlay_wnd_proc),
            hInstance: instance.into(),
            hCursor: LoadCursorW(None, IDC_ARROW)?,
            hbrBackground: HBRUSH(GetStockObject(BLACK_BRUSH).0),
            lpszClassName: OVERLAY_CLASS_NAME,
            ..Default::default()
        };
        let _ = RegisterClassExW(&wc_ov);

        // Calculate window rect
        let win_cfg = state.config.window.clone();
        let mut rect = RECT {
            left: win_cfg.x,
            top: win_cfg.y,
            right: win_cfg.x + win_cfg.width as i32,
            bottom: win_cfg.y + win_cfg.height as i32,
        };

        // Spec 4.1: If outside all monitors, center on primary monitor
        let on_any_monitor = state.monitors.iter().any(|m| {
            m.rect
                .contains_point((rect.left + rect.right) / 2, (rect.top + rect.bottom) / 2)
        });
        if !on_any_monitor
            && let Some(prim) = state
                .monitors
                .iter()
                .find(|m| m.is_primary)
                .or_else(|| state.monitors.first())
        {
            let cx = (prim.rect.left + prim.rect.right) / 2;
            let cy = (prim.rect.top + prim.rect.bottom) / 2;
            rect = RECT {
                left: cx - 640,
                top: cy - 400,
                right: cx + 640,
                bottom: cy + 400,
            };
        }

        let hwnd = CreateWindowExW(
            WINDOW_EX_STYLE::default(),
            PCWSTR(wide_class.as_ptr()),
            w!("Kabaper"),
            WS_OVERLAPPEDWINDOW,
            rect.left,
            rect.top,
            rect.right - rect.left,
            rect.bottom - rect.top,
            None,
            None,
            Some(instance.into()),
            None,
        )?;

        state.hwnd = hwnd;
        state.normal_window_rect = rect;
        APP_STATE_PTR = &mut state as *mut AppState;

        if !h_icon.is_invalid() {
            let _ = SendMessageW(
                hwnd,
                WM_SETICON,
                Some(WPARAM(ICON_BIG as usize)),
                Some(LPARAM(h_icon.0 as _)),
            );
        }
        if !h_icon_sm.is_invalid() {
            let _ = SendMessageW(
                hwnd,
                WM_SETICON,
                Some(WPARAM(ICON_SMALL as usize)),
                Some(LPARAM(h_icon_sm.0 as _)),
            );
        }

        // Enable Dark Mode for title bar (Windows 10 1809+ / Windows 11)
        let dark_mode = BOOL::from(true);
        if DwmSetWindowAttribute(
            hwnd,
            DWMWA_USE_IMMERSIVE_DARK_MODE,
            &dark_mode as *const _ as *const _,
            std::mem::size_of::<BOOL>() as u32,
        )
        .is_err()
        {
            // Fallback for Windows 10 versions 1809 - 1909 (attribute 19)
            let _ = DwmSetWindowAttribute(
                hwnd,
                DWMWINDOWATTRIBUTE(19),
                &dark_mode as *const _ as *const _,
                std::mem::size_of::<BOOL>() as u32,
            );
        }

        // Enable Drag & Drop (Spec 2.1)
        DragAcceptFiles(hwnd, true);

        // Set timer for OSD fade, bar fade, and config debounce
        let _ = SetTimer(Some(hwnd), 1, 100, None);

        // Show window (Spec 4.1: restore fullscreen if saved)
        if win_cfg.fullscreen {
            state.enter_fullscreen_startup();
        } else if win_cfg.maximized {
            let _ = ShowWindow(hwnd, SW_MAXIMIZE);
        } else {
            let _ = ShowWindow(hwnd, SW_SHOW);
        }
        let _ = UpdateWindow(hwnd);

        // Message loop
        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }

        // Save window state on exit (Spec 7.2)
        let mut wp = WINDOWPLACEMENT {
            length: std::mem::size_of::<WINDOWPLACEMENT>() as u32,
            ..Default::default()
        };
        if GetWindowPlacement(hwnd, &mut wp).is_ok() && !state.fullscreen {
            let is_max = wp.showCmd == SW_SHOWMAXIMIZED.0 as u32;
            state.config.window.maximized = is_max;
            let r = wp.rcNormalPosition;
            state.config.window.x = r.left;
            state.config.window.y = r.top;
            state.config.window.width = (r.right - r.left).max(100) as u32;
            state.config.window.height = (r.bottom - r.top).max(100) as u32;
        }
        state.config.window.fullscreen = state.fullscreen;
        state.config.window.show_info = state.show_info;
        if let Some(item) = state.catalog.current_item() {
            state.config.library.last_image = Some(item.path.to_string_lossy().to_string());
        }
        let _ = state.storage.save_config(&state.config);

        APP_STATE_PTR = std::ptr::null_mut();
        Ok(())
    }
}

unsafe extern "system" fn wnd_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    unsafe {
        if APP_STATE_PTR.is_null() {
            return DefWindowProcW(hwnd, msg, wparam, lparam);
        }
        let state = &mut *APP_STATE_PTR;

        match msg {
            WM_TIMER => {
                // Check OSD expiry
                if let Some((_, start, dur, _)) = &state.osd
                    && start.elapsed() >= *dur
                {
                    state.osd = None;
                    let _ = InvalidateRect(Some(hwnd), None, false);
                }

                // Fullscreen bar auto-hide after 3s
                if state.fullscreen
                    && state.bar_visible
                    && state.last_interaction.elapsed() >= Duration::from_secs(3)
                {
                    state.bar_visible = false;
                    let _ = InvalidateRect(Some(hwnd), None, false);
                }

                // Config debounce saving (1s after last change)
                if state.config_dirty
                    && state.last_config_change.elapsed() >= Duration::from_secs(1)
                {
                    state.config_dirty = false;
                    let _ = state.storage.save_config(&state.config);
                }

                LRESULT(0)
            }

            WM_SETCURSOR => {
                let hit_canvas = (lparam.0 & 0xffff) as u32 == HTCLIENT;
                if hit_canvas
                    && state.modal_dialog == ModalDialog::None
                {
                    if state.view_mode == ViewMode::Composite {
                        let mut pt = POINT::default();
                        let _ = GetCursorPos(&mut pt);
                        let _ = ScreenToClient(hwnd, &mut pt);

                        if is_over_pan_capable_frame(state, hwnd, pt.x, pt.y) {
                            let _ = SetCursor(Some(LoadCursorW(None, IDC_SIZEALL).unwrap()));
                            return LRESULT(1);
                        }
                    } else if state.view_mode == ViewMode::Normal && state.normal_zoom > 1.0 {
                        let _ = SetCursor(Some(LoadCursorW(None, IDC_SIZEALL).unwrap()));
                        return LRESULT(1);
                    }
                }
                DefWindowProcW(hwnd, msg, wparam, lparam)
            }

            WM_MOUSEMOVE => {
                state.last_interaction = Instant::now();
                let x = (lparam.0 & 0xffff) as i16 as i32;
                let y = ((lparam.0 >> 16) & 0xffff) as i16 as i32;

                if state.fullscreen && !state.bar_visible && y < 50 {
                    state.bar_visible = true;
                    let _ = InvalidateRect(Some(hwnd), None, false);
                }

                if state.is_panning {
                    let dx = x - state.pan_start_mouse.0;
                    let dy = y - state.pan_start_mouse.1;

                    if state.view_mode == ViewMode::Normal {
                        state.normal_pan_offset.0 = state.normal_pan_start_offset.0 + dx as f64;
                        state.normal_pan_offset.1 = state.normal_pan_start_offset.1 + dy as f64;
                        let _ = InvalidateRect(Some(hwnd), None, false);
                    } else if let Some(frame) = state.layout_frames.get_mut(state.selected_frame_idx) {
                        let can_pan = state
                            .active_plan
                            .as_ref()
                            .and_then(|p| p.frames.get(state.selected_frame_idx))
                            .and_then(|f| f.content.as_ref())
                            .map(|c| c.can_pan)
                            .unwrap_or((false, false));

                        let mut pan_x = frame.pan.0;
                        let mut pan_y = frame.pan.1;

                        if let Some(plan) = &state.active_plan
                            && let Some(frame_plan) = plan.frames.get(state.selected_frame_idx)
                            && let Some(content) = &frame_plan.content
                            && let Some(spec) = frame.image.as_ref()
                        {
                            let mut cr = RECT::default();
                            let _ = GetClientRect(hwnd, &mut cr);
                            let dpi = get_window_dpi(hwnd);
                            let bar_h = if state.bar_visible { get_bar_height(dpi) } else { 0 };
                            let canvas_layout = get_canvas_layout(
                                cr.right,
                                cr.bottom,
                                bar_h,
                                state.show_sidebar,
                                state.show_filmstrip,
                                dpi,
                            );
                            let canvas_w = canvas_layout.w;
                            let canvas_h = canvas_layout.h;

                            let (mw, mh) = plan.monitor_px;
                            let fit_scale =
                                (canvas_w as f64 / mw as f64).min(canvas_h as f64 / mh as f64);

                            let (iw, ih) = (spec.size_px.0 as f64, spec.size_px.1 as f64);
                            let dw = iw * content.scale;
                            let dh = ih * content.scale;
                            let sw = frame_plan.slot.width as f64;
                            let sh = frame_plan.slot.height as f64;

                            if can_pan.0 {
                                let movable_range_px = (dw - sw) * fit_scale;
                                if movable_range_px > 0.0 {
                                    pan_x = (state.pan_start_val.0
                                        - (dx as f64) / movable_range_px)
                                        .clamp(0.0, 1.0);
                                }
                            }
                            if can_pan.1 {
                                let movable_range_py = (dh - sh) * fit_scale;
                                if movable_range_py > 0.0 {
                                    pan_y = (state.pan_start_val.1
                                        - (dy as f64) / movable_range_py)
                                        .clamp(0.0, 1.0);
                                }
                            }
                        }

                        state.update_pan_fast(state.selected_frame_idx, (pan_x, pan_y));
                        let _ = InvalidateRect(Some(hwnd), None, false);
                    }
                }

                LRESULT(0)
            }

            WM_LBUTTONDOWN => {
                state.last_interaction = Instant::now();
                let x = (lparam.0 & 0xffff) as i16 as i32;
                let y = ((lparam.0 >> 16) & 0xffff) as i16 as i32;
                let dpi = get_window_dpi(hwnd);

                // Handle modal dialog clicks
                if state.modal_dialog != ModalDialog::None {
                    handle_modal_click(state, hwnd, x, y, dpi);
                    let _ = InvalidateRect(Some(hwnd), None, false);
                    return LRESULT(0);
                }

                // Handle operation bar clicks
                let bar_h = if state.bar_visible {
                    get_bar_height(dpi)
                } else {
                    0
                };
                if state.bar_visible && y < bar_h {
                    handle_bar_click(state, hwnd, x, y);
                    let _ = InvalidateRect(Some(hwnd), None, false);
                    return LRESULT(0);
                }

                // Handle OSD Undo button click (dynamically calculated)
                if let Some((msg_str, _, _, true)) = &state.osd {
                    let mut cr = RECT::default();
                    let _ = GetClientRect(hwnd, &mut cr);
                    let btn_r = osd_undo_button_rect_dpi(msg_str, cr.right, dpi);
                    if x >= btn_r.left && x <= btn_r.right && y >= btn_r.top && y <= btn_r.bottom {
                        state.undo_wallpaper();
                        let _ = InvalidateRect(Some(hwnd), None, false);
                        return LRESULT(0);
                    }
                }

                // Handle sidebar click
                if state.show_sidebar {
                    let mut cr = RECT::default();
                    let _ = GetClientRect(hwnd, &mut cr);
                    let sidebar_w = get_sidebar_width(dpi);
                    let strip_h = if state.show_filmstrip { scale_by_dpi(92, dpi) } else { 0 };
                    if x < sidebar_w && y >= bar_h && y < cr.bottom - strip_h {
                        handle_sidebar_click(state, x, y, cr.right, cr.bottom, dpi);
                        let _ = InvalidateRect(Some(hwnd), None, false);
                        return LRESULT(0);
                    }
                }

                // Handle filmstrip click
                if state.show_filmstrip {
                    let mut cr = RECT::default();
                    let _ = GetClientRect(hwnd, &mut cr);
                    let strip_h = scale_by_dpi(92, dpi);
                    if y >= cr.bottom - strip_h {
                        handle_filmstrip_click(state, x, cr.right, cr.bottom, dpi);
                        let _ = InvalidateRect(Some(hwnd), None, false);
                        return LRESULT(0);
                    }
                }

                // Handle empty state folder button click
                let is_empty = (state.view_mode == ViewMode::Composite
                    && state.active_composed.is_none())
                    || (state.view_mode == ViewMode::Normal
                        && state.catalog.current_item().is_none());
                if is_empty {
                    let mut cr = RECT::default();
                    let _ = GetClientRect(hwnd, &mut cr);
                    let btn_r = empty_state_folder_btn_rect_dpi(cr.right, cr.bottom, bar_h, dpi);
                    if x >= btn_r.left && x <= btn_r.right && y >= btn_r.top && y <= btn_r.bottom {
                        state.open_folder_dialog();
                        let _ = InvalidateRect(Some(hwnd), None, false);
                        return LRESULT(0);
                    }
                }

                // Handle frame selection, buttons, and pan in composite mode
                if state.view_mode == ViewMode::Composite
                    && let Some(plan) = &state.active_plan
                {
                    let mut cr = RECT::default();
                    let _ = GetClientRect(hwnd, &mut cr);
                    let canvas_layout = get_canvas_layout(
                        cr.right,
                        cr.bottom,
                        bar_h,
                        state.show_sidebar,
                        state.show_filmstrip,
                        dpi,
                    );
                    let canvas_w = canvas_layout.w;
                    let canvas_h = canvas_layout.h;

                    let (mw, mh) = plan.monitor_px;
                    let fit_scale = (canvas_w as f64 / mw as f64).min(canvas_h as f64 / mh as f64);
                    let dw = mw as f64 * fit_scale;
                    let dh = mh as f64 * fit_scale;
                    let ox = canvas_layout.x as f64 + (canvas_w as f64 - dw) / 2.0;
                    let oy = canvas_layout.y as f64 + (canvas_h as f64 - dh) / 2.0;

                    // Check frame overlay buttons on selected frame
                    let clicked_btn = if let Some(sel_frame) = plan.frames.get(state.selected_frame_idx) {
                        let fx = (ox + sel_frame.slot.x as f64 * fit_scale).round() as i32;
                        let fy = (oy + sel_frame.slot.y as f64 * fit_scale).round() as i32;
                        let fh = (sel_frame.slot.height as f64 * fit_scale).round() as i32;

                        let overlay_buttons = get_frame_overlay_buttons(fx, fy, fh, dpi);
                        let mut found = None;
                        for (btn_id, r) in overlay_buttons {
                            if x >= r.left && x <= r.right && y >= r.top && y <= r.bottom {
                                found = Some(btn_id);
                                break;
                            }
                        }
                        found
                    } else {
                        None
                    };

                    if let Some(btn_id) = clicked_btn {
                        match btn_id {
                            FrameOverlayButtonId::Pin => state.toggle_pin(),
                            FrameOverlayButtonId::Swap => state.swap_frames(),
                            FrameOverlayButtonId::Random => state.random_image(),
                            FrameOverlayButtonId::Exclude => state.toggle_exclude_current(),
                        }
                        let _ = InvalidateRect(Some(hwnd), None, false);
                        return LRESULT(0);
                    }

                    let rel_x = ((x as f64 - ox) / fit_scale) as i32;
                    let rel_y = ((y as f64 - oy) / fit_scale) as i32;

                    for (i, frame) in plan.frames.iter().enumerate() {
                        let s = frame.slot;
                        if rel_x >= s.x
                            && rel_x < s.x + s.width as i32
                            && rel_y >= s.y
                            && rel_y < s.y + s.height as i32
                        {
                            state.selected_frame_idx = i;
                            state.ensure_current_item_visible_in_sidebar();
                            state.is_panning = true;
                            state.pan_start_mouse = (x, y);
                            state.pan_start_val = state
                                .layout_frames
                                .get(i)
                                .map(|f| f.pan)
                                .unwrap_or((0.5, 0.5));
                            let _ = SetCapture(hwnd);
                            break;
                        }
                    }
                }

                if state.view_mode == ViewMode::Normal && state.normal_zoom > 1.0 {
                    state.is_panning = true;
                    state.pan_start_mouse = (x, y);
                    state.normal_pan_start_offset = state.normal_pan_offset;
                    let _ = SetCapture(hwnd);
                }

                let _ = InvalidateRect(Some(hwnd), None, false);
                LRESULT(0)
            }

            WM_LBUTTONUP => {
                if state.is_panning {
                    state.is_panning = false;
                    let _ = ReleaseCapture();
                }
                LRESULT(0)
            }

            WM_RBUTTONUP => {
                show_context_menu(state, hwnd, lparam);
                LRESULT(0)
            }

            WM_MBUTTONUP => {
                state.switch_view_mode();
                let _ = InvalidateRect(Some(hwnd), None, false);
                LRESULT(0)
            }

            WM_LBUTTONDBLCLK => {
                if state.view_mode == ViewMode::Normal {
                    state.toggle_fullscreen();
                    let _ = InvalidateRect(Some(hwnd), None, false);
                }
                LRESULT(0)
            }

            WM_MOUSEWHEEL => {
                let delta = ((wparam.0 >> 16) & 0xffff) as i16;
                let ctrl_down = (GetKeyState(VK_CONTROL.0 as i32) as u16 & 0x8000) != 0;

                let dpi = get_window_dpi(hwnd);
                let mut pt = POINT {
                    x: (lparam.0 & 0xffff) as i16 as i32,
                    y: ((lparam.0 >> 16) & 0xffff) as i16 as i32,
                };
                let _ = ScreenToClient(hwnd, &mut pt);

                let sidebar_w = if state.show_sidebar { get_sidebar_width(dpi) } else { 0 };
                let bar_h = if state.bar_visible { get_bar_height(dpi) } else { 0 };

                if !ctrl_down && state.show_sidebar && pt.x < sidebar_w && pt.y >= bar_h {
                    let mut cr = RECT::default();
                    let _ = GetClientRect(hwnd, &mut cr);
                    let strip_h = if state.show_filmstrip { scale_by_dpi(92, dpi) } else { 0 };
                    let header_h = scale_by_dpi(38, dpi);
                    let list_h = (cr.bottom - bar_h - strip_h - header_h).max(0);

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
                        cr.right,
                        cr.bottom,
                        bar_h,
                        state.show_filmstrip,
                        state.view_mode,
                        state.layout_frames.len(),
                        state.selected_frame_idx,
                        dpi,
                    );
                    let scroll_amount = (delta as i32 / 120) * scale_by_dpi(60, dpi);
                    state.scroll_sidebar(scroll_amount, list_h, layout.total_content_height);
                } else if ctrl_down {
                    if state.view_mode == ViewMode::Normal {
                        if delta > 0 {
                            state.zoom_normal(1.25);
                        } else {
                            state.zoom_normal(0.8);
                        }
                    } else {
                        let d = if delta > 0 { 0.05 } else { -0.05 };
                        state.adjust_max_scale(d);
                    }
                } else if delta < 0 {
                    state.next_image();
                } else {
                    state.prev_image();
                }
                let _ = InvalidateRect(Some(hwnd), None, false);
                LRESULT(0)
            }

            WM_KEYDOWN => {
                state.last_interaction = Instant::now();
                let key = VIRTUAL_KEY(wparam.0 as u16);
                let ctrl = (GetKeyState(VK_CONTROL.0 as i32) as u16 & 0x8000) != 0;
                let shift = (GetKeyState(VK_SHIFT.0 as i32) as u16 & 0x8000) != 0;
                let alt = (GetKeyState(VK_MENU.0 as i32) as u16 & 0x8000) != 0;

                // When modal dialog is open, handle navigation or dismiss
                if state.modal_dialog != ModalDialog::None {
                    if key == VK_ESCAPE {
                        state.modal_dialog = ModalDialog::None;
                        let _ = InvalidateRect(Some(hwnd), None, false);
                    }
                    return LRESULT(0);
                }

                match key {
                    VK_RIGHT | VK_DOWN => {
                        if ctrl {
                            state.jump_to_next_folder();
                        } else if shift {
                            // Shift+Arrow: Pan selected frame
                            if state.view_mode == ViewMode::Composite {
                                pan_by_keyboard(state, key);
                            } else {
                                state.set_osd(
                                    "合成表示でのみ有効です (Vキーで切替)".into(),
                                    2,
                                    false,
                                );
                            }
                        } else {
                            state.next_image();
                        }
                    }
                    VK_LEFT | VK_UP => {
                        if ctrl {
                            state.jump_to_prev_folder();
                        } else if shift {
                            if state.view_mode == ViewMode::Composite {
                                pan_by_keyboard(state, key);
                            } else {
                                state.set_osd(
                                    "合成表示でのみ有効です (Vキーで切替)".into(),
                                    2,
                                    false,
                                );
                            }
                        } else {
                            state.prev_image();
                        }
                    }
                    VK_PRIOR => state.jump_to_prev_folder(),
                    VK_NEXT => state.jump_to_next_folder(),
                    VK_HOME => state.first_image(),
                    VK_END => state.last_image(),
                    VK_TAB => {
                        if state.view_mode == ViewMode::Composite && !state.layout_frames.is_empty()
                        {
                            let len = state.layout_frames.len();
                            state.selected_frame_idx = if shift {
                                (state.selected_frame_idx + len - 1) % len
                            } else {
                                (state.selected_frame_idx + 1) % len
                            };
                            state.ensure_current_item_visible_in_sidebar();
                        } else {
                            state.set_osd("合成表示でのみ有効です (Vキーで切替)".into(), 2, false);
                        }
                    }
                    VK_SPACE => {
                        if state.view_mode == ViewMode::Composite {
                            if shift {
                                state.swap_frames();
                            } else {
                                state.toggle_pin();
                            }
                        } else {
                            state.set_osd("合成表示でのみ有効です (Vキーで切替)".into(), 2, false);
                        }
                    }
                    VK_C => {
                        if state.view_mode == ViewMode::Composite {
                            state.reset_pan();
                        } else {
                            state.reset_normal_zoom();
                            state.set_osd("ズーム: 全体表示 (Fit)".into(), 1, false);
                        }
                    }
                    VK_V => state.switch_view_mode(),
                    VK_L => state.cycle_layout_mode(!shift),
                    VK_OEM_PLUS | VK_ADD => {
                        if state.view_mode == ViewMode::Normal {
                            state.zoom_normal(1.25);
                        } else {
                            state.adjust_max_scale(0.05);
                        }
                    }
                    VK_OEM_MINUS | VK_SUBTRACT => {
                        if state.view_mode == ViewMode::Normal {
                            state.zoom_normal(0.8);
                        } else {
                            state.adjust_max_scale(-0.05);
                        }
                    }
                    VK_NUMPAD0 => {
                        if state.view_mode == ViewMode::Normal {
                            state.reset_normal_zoom();
                            state.set_osd("ズーム: 全体表示 (Fit)".into(), 1, false);
                        }
                    }
                    VK_RETURN => {
                        let is_empty = (state.view_mode == ViewMode::Composite
                            && state.active_composed.is_none())
                            || (state.view_mode == ViewMode::Normal
                                && state.catalog.current_item().is_none())
                            || state.catalog.items.is_empty();
                        if alt {
                            state.toggle_fullscreen();
                        } else if is_empty {
                            state.open_folder_dialog();
                        } else {
                            state.apply_current_wallpaper();
                        }
                    }
                    VK_R => {
                        if shift || ctrl {
                            state.random_image();
                        } else {
                            state.trigger_random_update();
                        }
                    }
                    VK_Z => {
                        if ctrl {
                            state.undo_wallpaper();
                        } else if state.view_mode == ViewMode::Normal {
                            let mut cr = RECT::default();
                            let _ = GetClientRect(hwnd, &mut cr);
                            let dpi = get_window_dpi(hwnd);
                            let bar_h = if state.bar_visible { get_bar_height(dpi) } else { 0 };
                            let canvas_layout = get_canvas_layout(
                                cr.right,
                                cr.bottom,
                                bar_h,
                                state.show_sidebar,
                                state.show_filmstrip,
                                dpi,
                            );
                            state.toggle_normal_100_percent(canvas_layout.w, canvas_layout.h);
                        }
                    }
                    VK_S => state.export_png_dialog(),
                    VK_D => state.cycle_dim_ratio(),
                    VK_A => state.apply_smart_pan_current_frame(),
                    VK_B => state.toggle_sidebar(),
                    VK_T => {
                        state.show_filmstrip = !state.show_filmstrip;
                        state.set_osd(
                            format!(
                                "フィルムストリップ: {}",
                                if state.show_filmstrip {
                                    "表示"
                                } else {
                                    "非表示"
                                }
                            ),
                            2,
                            false,
                        );
                    }
                    VK_P => {
                        if ctrl {
                            state.save_current_as_preset();
                        } else {
                            state.load_next_preset();
                        }
                    }
                    VK_DELETE => state.toggle_exclude_current(),
                    VK_O if ctrl => {
                        if shift {
                            state.open_in_explorer();
                        } else {
                            state.open_folder_dialog();
                        }
                    }
                    VK_F5 => {
                        state.catalog.reload();
                        state.rebuild_layout(true, true);
                        state.set_osd("フォルダを再読込しました".into(), 2, false);
                    }
                    VK_I => {
                        state.show_info = !state.show_info;
                        state.config.window.show_info = state.show_info;
                        state.mark_config_dirty();
                        state.set_osd(
                            format!("情報表示: {}", if state.show_info { "ON" } else { "OFF" }),
                            2,
                            false,
                        );
                    }
                    VK_F11 => state.toggle_fullscreen(),
                    VK_F1 | VK_OEM_2 => {
                        state.modal_dialog = ModalDialog::Shortcuts;
                    }
                    VK_APPS => {
                        let mut pt = POINT { x: 100, y: 100 };
                        let _ = ClientToScreen(hwnd, &mut pt);
                        show_context_menu_at(state, hwnd, pt.x, pt.y);
                    }
                    VK_MENU => {
                        if state.fullscreen {
                            state.bar_visible = true;
                        }
                    }
                    VK_ESCAPE => {
                        if state.modal_dialog != ModalDialog::None {
                            state.modal_dialog = ModalDialog::None;
                        } else if !state.error_banner.is_empty() {
                            state.error_banner.clear();
                        } else if state.fullscreen {
                            state.toggle_fullscreen();
                        }
                    }
                    k if k.0 >= b'1' as u16 && k.0 <= b'9' as u16 => {
                        let num = (k.0 - b'1' as u16 + 1) as usize;
                        if ctrl {
                            state.select_monitor(num);
                            state.apply_current_wallpaper();
                        } else {
                            state.select_monitor(num);
                        }
                    }
                    _ => {}
                }

                let _ = InvalidateRect(Some(hwnd), None, false);
                LRESULT(0)
            }

            WM_DROPFILES => {
                let hdrop = HDROP(wparam.0 as *mut _);
                let count = DragQueryFileW(hdrop, 0xFFFFFFFF, None);
                if count > 0 {
                    let mut buf = vec![0u16; 1024];
                    let len = DragQueryFileW(hdrop, 0, Some(&mut buf));
                    if len > 0 {
                        let path_str = String::from_utf16_lossy(&buf[..len as usize]);
                        let p = PathBuf::from(path_str);
                        state.open_path(&p);
                    }
                }
                DragFinish(hdrop);
                let _ = InvalidateRect(Some(hwnd), None, false);
                LRESULT(0)
            }

            WM_DISPLAYCHANGE => {
                state.monitors = enumerate_monitors();
                if let Some(target_id) = &state.config.target.monitor_id {
                    if let Some(idx) = state.monitors.iter().position(|m| &m.id == target_id) {
                        state.target_monitor_idx = idx;
                    } else {
                        state.target_monitor_idx = 0;
                    }
                }
                state.rebuild_layout(false, true);
                let _ = InvalidateRect(Some(hwnd), None, false);
                LRESULT(0)
            }

            WM_DPICHANGED => {
                if lparam.0 != 0 {
                    let prc = lparam.0 as *const RECT;
                    let r = *prc;
                    let _ = SetWindowPos(
                        hwnd,
                        None,
                        r.left,
                        r.top,
                        r.right - r.left,
                        r.bottom - r.top,
                        SWP_NOZORDER | SWP_NOACTIVATE,
                    );
                }
                state.monitors = enumerate_monitors();
                if let Some(target_id) = &state.config.target.monitor_id {
                    if let Some(idx) = state.monitors.iter().position(|m| &m.id == target_id) {
                        state.target_monitor_idx = idx;
                    } else {
                        state.target_monitor_idx = 0;
                    }
                }
                state.rebuild_layout(false, true);
                let _ = InvalidateRect(Some(hwnd), None, false);
                LRESULT(0)
            }

            WM_EXITSIZEMOVE => {
                if !state.fullscreen && !IsIconic(hwnd).as_bool() {
                    let mut wp = WINDOWPLACEMENT {
                        length: std::mem::size_of::<WINDOWPLACEMENT>() as u32,
                        ..Default::default()
                    };
                    if GetWindowPlacement(hwnd, &mut wp).is_ok() {
                        let is_max = wp.showCmd == SW_SHOWMAXIMIZED.0 as u32;
                        state.config.window.maximized = is_max;
                        let r = wp.rcNormalPosition;
                        state.config.window.x = r.left;
                        state.config.window.y = r.top;
                        state.config.window.width = (r.right - r.left).max(100) as u32;
                        state.config.window.height = (r.bottom - r.top).max(100) as u32;
                        state.mark_config_dirty();
                    }
                }
                DefWindowProcW(hwnd, msg, wparam, lparam)
            }

            WM_SIZE => {
                let size_type = wparam.0 as u32;
                if size_type == SIZE_MINIMIZED {
                    // Spec 8.2: Release prefetch cache when minimized, while retaining displayed images
                    state.release_prefetch_cache();
                } else if !state.fullscreen
                    && (size_type == SIZE_MAXIMIZED || size_type == SIZE_RESTORED)
                {
                    let mut wp = WINDOWPLACEMENT {
                        length: std::mem::size_of::<WINDOWPLACEMENT>() as u32,
                        ..Default::default()
                    };
                    if GetWindowPlacement(hwnd, &mut wp).is_ok() {
                        let is_max = wp.showCmd == SW_SHOWMAXIMIZED.0 as u32;
                        state.config.window.maximized = is_max;
                        let r = wp.rcNormalPosition;
                        state.config.window.x = r.left;
                        state.config.window.y = r.top;
                        state.config.window.width = (r.right - r.left).max(100) as u32;
                        state.config.window.height = (r.bottom - r.top).max(100) as u32;
                        state.mark_config_dirty();
                    }
                    state.prefetch_adjacent_images();
                    state.prune_image_cache();
                    let _ = InvalidateRect(Some(hwnd), None, false);
                }
                DefWindowProcW(hwnd, msg, wparam, lparam)
            }

            WM_COPYDATA => {
                let cds = &*(lparam.0 as *const COPYDATASTRUCT);
                if cds.cbData > 0 && !cds.lpData.is_null() {
                    let slice =
                        std::slice::from_raw_parts(cds.lpData as *const u8, cds.cbData as usize);
                    if let Ok(path_str) = std::str::from_utf8(slice)
                        && !path_str.trim().is_empty()
                    {
                        let p = PathBuf::from(path_str);
                        state.open_path(&p);
                    }
                }
                if IsIconic(hwnd).as_bool() {
                    let _ = ShowWindow(hwnd, SW_RESTORE);
                }
                let _ = SetForegroundWindow(hwnd);
                let _ = InvalidateRect(Some(hwnd), None, false);
                LRESULT(1)
            }

            WM_PAINT => {
                let mut ps = PAINTSTRUCT::default();
                let hdc = BeginPaint(hwnd, &mut ps);
                paint_window(state, hwnd, hdc);
                let _ = EndPaint(hwnd, &ps);
                LRESULT(0)
            }

            WM_ERASEBKGND => LRESULT(1),

            WM_DESTROY => {
                PostQuitMessage(0);
                LRESULT(0)
            }

            _ => DefWindowProcW(hwnd, msg, wparam, lparam),
        }
    }
}

fn pan_by_keyboard(state: &mut AppState, key: VIRTUAL_KEY) {
    let can_pan = state
        .active_plan
        .as_ref()
        .and_then(|p| p.frames.get(state.selected_frame_idx))
        .and_then(|f| f.content.as_ref())
        .map(|c| c.can_pan)
        .unwrap_or((false, false));

    if let Some(frame) = state.layout_frames.get(state.selected_frame_idx) {
        let mut new_pan = frame.pan;
        let mut changed = false;
        match key {
            VK_LEFT if can_pan.0 => {
                new_pan.0 = (new_pan.0 - 0.05).clamp(0.0, 1.0);
                changed = true;
            }
            VK_RIGHT if can_pan.0 => {
                new_pan.0 = (new_pan.0 + 0.05).clamp(0.0, 1.0);
                changed = true;
            }
            VK_UP if can_pan.1 => {
                new_pan.1 = (new_pan.1 - 0.05).clamp(0.0, 1.0);
                changed = true;
            }
            VK_DOWN if can_pan.1 => {
                new_pan.1 = (new_pan.1 + 0.05).clamp(0.0, 1.0);
                changed = true;
            }
            _ => {}
        }
        if changed {
            state.update_pan_fast(state.selected_frame_idx, new_pan);
        }
    }
}

fn is_over_pan_capable_frame(state: &AppState, hwnd: HWND, x: i32, y: i32) -> bool {
    unsafe {
        let Some(plan) = &state.active_plan else {
            return false;
        };
        let mut cr = RECT::default();
        let _ = GetClientRect(hwnd, &mut cr);
        let dpi = get_window_dpi(hwnd);
        let bar_h = if state.bar_visible { get_bar_height(dpi) } else { 0 };
        let canvas_layout = get_canvas_layout(
            cr.right,
            cr.bottom,
            bar_h,
            state.show_sidebar,
            state.show_filmstrip,
            dpi,
        );
        let canvas_w = canvas_layout.w;
        let canvas_h = canvas_layout.h;

        let (mw, mh) = plan.monitor_px;
        let fit_scale = (canvas_w as f64 / mw as f64).min(canvas_h as f64 / mh as f64);
        let dw = mw as f64 * fit_scale;
        let dh = mh as f64 * fit_scale;
        let ox = canvas_layout.x as f64 + (canvas_w as f64 - dw) / 2.0;
        let oy = canvas_layout.y as f64 + (canvas_h as f64 - dh) / 2.0;

        let rel_x = ((x as f64 - ox) / fit_scale) as i32;
        let rel_y = ((y as f64 - oy) / fit_scale) as i32;

        for frame in &plan.frames {
            let s = frame.slot;
            if rel_x >= s.x
                && rel_x < s.x + s.width as i32
                && rel_y >= s.y
                && rel_y < s.y + s.height as i32
                && let Some(c) = &frame.content
            {
                return c.can_pan.0 || c.can_pan.1;
            }
        }
        false
    }
}

fn handle_bar_click(state: &mut AppState, hwnd: HWND, x: i32, y: i32) {
    let dpi = get_window_dpi(hwnd);
    let buttons = get_operation_bar_buttons(dpi);
    for (id, r) in buttons {
        if x >= r.left && x <= r.right && y >= r.top && y <= r.bottom {
            match id {
                BarButtonId::Open => state.open_folder_dialog(),
                BarButtonId::Sidebar => state.toggle_sidebar(),
                BarButtonId::ViewMode => state.switch_view_mode(),
                BarButtonId::Monitor => {
                    let next_num = if let Some(m) = state.target_monitor() {
                        (m.number % state.monitors.len()) + 1
                    } else {
                        1
                    };
                    state.select_monitor(next_num);
                }
                BarButtonId::Split => state.cycle_layout_mode(true),
                BarButtonId::Scale => state.adjust_max_scale(0.05),
                BarButtonId::Apply => state.apply_current_wallpaper(),
                BarButtonId::Random => state.random_image(),
                BarButtonId::UpdateWallpaper => state.trigger_random_update(),
                BarButtonId::Undo => state.undo_wallpaper(),
                BarButtonId::SavePng => state.export_png_dialog(),
                BarButtonId::Settings => {
                    state.modal_dialog = ModalDialog::Settings;
                }
                BarButtonId::Shortcuts => {
                    state.modal_dialog = ModalDialog::Shortcuts;
                }
            }
            break;
        }
    }
}

fn sync_schedule_and_notify(state: &mut AppState) {
    if let Some(sched) = &state.scheduler_mgr {
        if let Ok(exe) = std::env::current_exe() {
            if let Err(e) = sched.sync_schedule(&state.config.schedule, &exe) {
                let err_msg = format!("タスクスケジューラ同期エラー: {}", e);
                state.set_osd(err_msg.clone(), 5, false);
                state
                    .error_banner
                    .retain(|msg| !msg.starts_with("タスクスケジューラ"));
                state.error_banner.push(err_msg);
            } else {
                state
                    .error_banner
                    .retain(|msg| !msg.starts_with("タスクスケジューラ"));
            }
        }
    } else if state.config.schedule.enabled {
        let err_msg = "タスクスケジューラサービスに接続できません".to_string();
        state.set_osd(err_msg.clone(), 5, false);
        state
            .error_banner
            .retain(|msg| !msg.starts_with("タスクスケジューラ"));
        state.error_banner.push(err_msg);
    }
}

fn handle_modal_click(state: &mut AppState, hwnd: HWND, x: i32, y: i32, dpi: u32) {
    unsafe {
        let mut cr = RECT::default();
        let _ = GetClientRect(hwnd, &mut cr);
        let win_w = cr.right.max(1);
        let win_h = cr.bottom.max(1);

        match state.modal_dialog {
            ModalDialog::Settings => {
                let layout = get_settings_modal_layout(win_w, win_h, dpi);
                let bx = layout.card_rect.left;
                let by = layout.card_rect.top;

                // Close button
                if rect_contains(&layout.close_btn_rect, x, y) {
                    state.modal_dialog = ModalDialog::None;
                    return;
                }

                // Row 1: Max Scale M [-] [+]
                let row1_y = by + scale_by_dpi(60, dpi);
                let btn_h28 = scale_by_dpi(28, dpi);
                if y >= row1_y && y <= row1_y + btn_h28 {
                    let minus_x = bx + scale_by_dpi(160, dpi);
                    let minus_w = scale_by_dpi(40, dpi);
                    let plus_x = bx + scale_by_dpi(280, dpi);
                    let plus_w = scale_by_dpi(40, dpi);
                    if x >= minus_x && x <= minus_x + minus_w {
                        state.adjust_max_scale(-0.05);
                    } else if x >= plus_x && x <= plus_x + plus_w {
                        state.adjust_max_scale(0.05);
                    }
                }

                // Row 2: Border px [-] [+]
                let row2_y = by + scale_by_dpi(105, dpi);
                if y >= row2_y && y <= row2_y + btn_h28 {
                    let minus_x = bx + scale_by_dpi(160, dpi);
                    let minus_w = scale_by_dpi(40, dpi);
                    let plus_x = bx + scale_by_dpi(280, dpi);
                    let plus_w = scale_by_dpi(40, dpi);
                    if x >= minus_x && x <= minus_x + minus_w {
                        state.config.layout.border_px =
                            state.config.layout.border_px.saturating_sub(1);
                        state.mark_config_dirty();
                        state.rebuild_layout(false, true);
                    } else if x >= plus_x && x <= plus_x + plus_w {
                        state.config.layout.border_px = (state.config.layout.border_px + 1).min(8);
                        state.mark_config_dirty();
                        state.rebuild_layout(false, true);
                    }
                }

                // Row 3: Random mode
                let row3_y = by + scale_by_dpi(150, dpi);
                if y >= row3_y && y <= row3_y + btn_h28 {
                    let m_auto_x = bx + scale_by_dpi(160, dpi);
                    let m_auto_w = scale_by_dpi(60, dpi);
                    let m_single_x = bx + scale_by_dpi(225, dpi);
                    let m_single_w = scale_by_dpi(50, dpi);
                    let m_split2_x = bx + scale_by_dpi(280, dpi);
                    let m_split2_w = scale_by_dpi(50, dpi);
                    let m_split3_x = bx + scale_by_dpi(335, dpi);
                    let m_split3_w = scale_by_dpi(50, dpi);

                    if x >= m_auto_x && x <= m_auto_x + m_auto_w {
                        state.config.random.mode = "auto".into();
                        state.mark_config_dirty();
                    } else if x >= m_single_x && x <= m_single_x + m_single_w {
                        state.config.random.mode = "single".into();
                        state.mark_config_dirty();
                    } else if x >= m_split2_x && x <= m_split2_x + m_split2_w {
                        state.config.random.mode = "split2".into();
                        state.mark_config_dirty();
                    } else if x >= m_split3_x && x <= m_split3_x + m_split3_w {
                        state.config.random.mode = "split3".into();
                        state.mark_config_dirty();
                    }
                }

                // Row 4: Schedule enabled
                let row4_y = by + scale_by_dpi(195, dpi);
                let en_x = bx + scale_by_dpi(160, dpi);
                let en_w = scale_by_dpi(100, dpi);
                if y >= row4_y && y <= row4_y + btn_h28 && x >= en_x && x <= en_x + en_w {
                    state.config.schedule.enabled = !state.config.schedule.enabled;
                    if state.config.schedule.enabled
                        && !state.config.schedule.on_logon
                        && state.config.schedule.interval_minutes == 0
                    {
                        state.config.schedule.interval_minutes = 60;
                    }
                    state.mark_config_dirty();
                    sync_schedule_and_notify(state);
                }

                // Row 5: Schedule logon
                let row5_y = by + scale_by_dpi(240, dpi);
                let en_x = bx + scale_by_dpi(160, dpi);
                let en_w = scale_by_dpi(100, dpi);
                if y >= row5_y && y <= row5_y + btn_h28 && x >= en_x && x <= en_x + en_w {
                    let next = !state.config.schedule.on_logon;
                    if !next
                        && state.config.schedule.interval_minutes == 0
                        && state.config.schedule.enabled
                    {
                        state.set_osd(
                            "定期更新ON時はログオン時と実行間隔の両方をOFFにできません".into(),
                            3,
                            false,
                        );
                    } else {
                        state.config.schedule.on_logon = next;
                        state.mark_config_dirty();
                        sync_schedule_and_notify(state);
                    }
                }

                // Row 6: Schedule interval cycle
                let row6_y = by + scale_by_dpi(285, dpi);
                let int_x = bx + scale_by_dpi(160, dpi);
                let int_w = scale_by_dpi(200, dpi);
                if y >= row6_y && y <= row6_y + btn_h28 && x >= int_x && x <= int_x + int_w {
                    let intervals = [0, 15, 30, 60, 180, 360, 720, 1440];
                    let cur = state.config.schedule.interval_minutes;
                    let idx = intervals.iter().position(|&i| i == cur).unwrap_or(3);
                    let mut next_idx = (idx + 1) % intervals.len();
                    if intervals[next_idx] == 0
                        && !state.config.schedule.on_logon
                        && state.config.schedule.enabled
                    {
                        next_idx = 1; // Skip 0
                    }
                    state.config.schedule.interval_minutes = intervals[next_idx];
                    state.mark_config_dirty();
                    sync_schedule_and_notify(state);
                }
            }
            ModalDialog::ExcludeList => {
                let layout = get_exclude_list_modal_layout(win_w, win_h, dpi);
                let bx = layout.card_rect.left;
                let by = layout.card_rect.top;

                // Close button
                if rect_contains(&layout.close_btn_rect, x, y) {
                    state.modal_dialog = ModalDialog::None;
                    return;
                }

                // Current folder exclude buttons
                let excluded_in_folder: Vec<PathBuf> = state
                    .catalog
                    .items
                    .iter()
                    .filter(|it| it.is_excluded)
                    .map(|it| it.path.clone())
                    .collect();

                let rel_btn_x = bx + scale_by_dpi(440, dpi);
                let rel_btn_w = scale_by_dpi(60, dpi);
                let rel_btn_h = scale_by_dpi(26, dpi);
                for (i, p) in excluded_in_folder.iter().take(5).enumerate() {
                    let btn_y = by + scale_by_dpi(60, dpi) + (i as i32) * scale_by_dpi(36, dpi);
                    if y >= btn_y && y <= btn_y + rel_btn_h && x >= rel_btn_x && x <= rel_btn_x + rel_btn_w {
                        state.catalog.toggle_exclude(p);
                        state.sync_catalog_excludes();
                        state.rebuild_layout(false, false);
                        return;
                    }
                }

                // Clear current folder excludes
                let clr1_x = bx + scale_by_dpi(30, dpi);
                let clr1_y = by + scale_by_dpi(250, dpi);
                let clr1_w = scale_by_dpi(220, dpi);
                let clr1_h = scale_by_dpi(30, dpi);
                if y >= clr1_y && y <= clr1_y + clr1_h && x >= clr1_x && x <= clr1_x + clr1_w {
                    for p in &excluded_in_folder {
                        state.catalog.excluded_paths.remove(p);
                    }
                    for it in &mut state.catalog.items {
                        if it.is_excluded {
                            it.is_excluded = false;
                        }
                    }
                    state.sync_catalog_excludes();
                    state.rebuild_layout(false, false);
                    state.set_osd("現在のフォルダの除外を一括解除しました".into(), 2, false);
                }

                // Clean nonexistent paths
                let clr2_x = bx + scale_by_dpi(30, dpi);
                let clr2_y = by + scale_by_dpi(325, dpi);
                let clr2_w = scale_by_dpi(200, dpi);
                let clr2_h = scale_by_dpi(30, dpi);
                if y >= clr2_y && y <= clr2_y + clr2_h && x >= clr2_x && x <= clr2_x + clr2_w {
                    state.catalog.excluded_paths.retain(|p| p.exists());
                    state.sync_catalog_excludes();
                    state.set_osd("存在しないパスの除外設定を整理しました".into(), 2, false);
                }

                // Clear other folder excludes
                let clr3_x = bx + scale_by_dpi(245, dpi);
                let clr3_y = by + scale_by_dpi(325, dpi);
                let clr3_w = scale_by_dpi(200, dpi);
                let clr3_h = scale_by_dpi(30, dpi);
                if y >= clr3_y
                    && y <= clr3_y + clr3_h
                    && x >= clr3_x
                    && x <= clr3_x + clr3_w
                    && let Some(folder) = &state.catalog.folder
                {
                    let f_norm = normalize_path(folder);
                    state
                        .catalog
                        .excluded_paths
                        .retain(|p| p.parent().map(normalize_path).as_ref() == Some(&f_norm));
                    state.sync_catalog_excludes();
                    state.set_osd("他フォルダの除外設定を一括解除しました".into(), 2, false);
                }
            }
            ModalDialog::Shortcuts => {
                let layout = get_shortcuts_modal_layout(win_w, win_h, dpi);
                if rect_contains(&layout.close_btn_rect, x, y) {
                    state.modal_dialog = ModalDialog::None;
                }
            }
            ModalDialog::None => {}
        }
    }
}

fn handle_sidebar_click(
    state: &mut AppState,
    x: i32,
    y: i32,
    win_w: i32,
    win_h: i32,
    dpi: u32,
) -> bool {
    if !state.show_sidebar {
        return false;
    }
    let bar_h = if state.bar_visible { get_bar_height(dpi) } else { 0 };
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

    // 1. Check header buttons
    if x >= layout.close_btn_rect.left
        && x <= layout.close_btn_rect.right
        && y >= layout.close_btn_rect.top
        && y <= layout.close_btn_rect.bottom
    {
        state.show_sidebar = false;
        return true;
    }
    if x >= layout.collapse_all_btn_rect.left
        && x <= layout.collapse_all_btn_rect.right
        && y >= layout.collapse_all_btn_rect.top
        && y <= layout.collapse_all_btn_rect.bottom
    {
        state.collapse_all_folders();
        return true;
    }
    if x >= layout.focus_current_btn_rect.left
        && x <= layout.focus_current_btn_rect.right
        && y >= layout.focus_current_btn_rect.top
        && y <= layout.focus_current_btn_rect.bottom
    {
        state.focus_current_folder();
        return true;
    }

    // 1.5 Check target frame bar (Composite mode)
    if let Some((bar_r, buttons)) = &layout.target_frame_bar {
        if y >= bar_r.top && y <= bar_r.bottom {
            for (f_idx, _label, btn_rect, _is_active) in buttons {
                if x >= btn_rect.left && x <= btn_rect.right && y >= btn_rect.top && y <= btn_rect.bottom {
                    state.selected_frame_idx = *f_idx;
                    state.ensure_current_item_visible_in_sidebar();
                    return true;
                }
            }
            return true;
        }
    }

    // 2. Check scrollbar thumb / track
    if let Some(s_rect) = layout.scrollbar_rect {
        if x >= s_rect.left && x <= s_rect.right && y >= s_rect.top && y <= s_rect.bottom {
            let list_h = (layout.list_rect.bottom - layout.list_rect.top).max(0);
            let click_ratio =
                (y - s_rect.top) as f64 / (s_rect.bottom - s_rect.top).max(1) as f64;
            let target_scroll =
                ((layout.total_content_height - list_h) as f64 * click_ratio).round() as i32;
            state.sidebar_scroll_offset =
                target_scroll.clamp(0, (layout.total_content_height - list_h).max(0));
            return true;
        }
    }

    // 3. Check rows in list
    if y >= layout.list_rect.top && y <= layout.list_rect.bottom {
        unsafe {
            let alt_down = (GetKeyState(VK_MENU.0 as i32) as u16 & 0x8000) != 0;
            for row in layout.rows {
                if y >= row.rect.top && y <= row.rect.bottom {
                    match row.kind {
                        SidebarRowKind::FolderHeader { folder_path, .. } => {
                            if alt_down {
                                state.toggle_folder_collapse_solo(&folder_path);
                            } else {
                                state.toggle_folder_collapse(&folder_path);
                            }
                            return true;
                        }
                        SidebarRowKind::FileItem {
                            catalog_index,
                            frame_targets,
                            ..
                        } => {
                            for (f_idx, _label, btn_rect) in frame_targets {
                                if x >= btn_rect.left
                                    && x <= btn_rect.right
                                    && y >= btn_rect.top
                                    && y <= btn_rect.bottom
                                {
                                    state.apply_image_to_frame(catalog_index, f_idx);
                                    return true;
                                }
                            }
                            state.select_image_by_index(catalog_index);
                            return true;
                        }
                    }
                }
            }
        }
    }

    true
}

fn handle_filmstrip_click(state: &mut AppState, x: i32, win_w: i32, win_h: i32, dpi: u32) -> bool {
    if !state.show_filmstrip || state.catalog.items.is_empty() {
        return false;
    }
    let cur_idx = state.catalog.current_index.unwrap_or(0);
    let total = state.catalog.items.len();
    let (_, _, item_w, start_idx, end_idx) =
        get_filmstrip_layout(win_w, win_h, cur_idx, total, dpi);

    let offset_x = x - scale_by_dpi(10, dpi);
    if offset_x < 0 {
        return true;
    }
    let slot_idx = (offset_x / item_w) as usize;
    let target_idx = start_idx + slot_idx;

    if target_idx < end_idx && target_idx < state.catalog.items.len() {
        state.select_image_by_index(target_idx);
    }
    true
}

fn show_context_menu(state: &mut AppState, hwnd: HWND, lparam: LPARAM) {
    unsafe {
        let x = (lparam.0 & 0xffff) as i16 as i32;
        let y = ((lparam.0 >> 16) & 0xffff) as i16 as i32;
        let mut pt = POINT { x, y };
        let _ = ClientToScreen(hwnd, &mut pt);

        if state.show_sidebar {
            let dpi = get_window_dpi(hwnd);
            let mut cr = RECT::default();
            let _ = GetClientRect(hwnd, &mut cr);
            let bar_h = if state.bar_visible { get_bar_height(dpi) } else { 0 };
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
                cr.right,
                cr.bottom,
                bar_h,
                state.show_filmstrip,
                state.view_mode,
                state.layout_frames.len(),
                state.selected_frame_idx,
                dpi,
            );

            if x >= layout.bounds.left && x <= layout.bounds.right {
                if y >= layout.list_rect.top && y <= layout.list_rect.bottom {
                    for row in layout.rows {
                        if y >= row.rect.top && y <= row.rect.bottom {
                            if let SidebarRowKind::FileItem { catalog_index, .. } = row.kind {
                                show_sidebar_context_menu(state, hwnd, pt.x, pt.y, catalog_index);
                                return;
                            }
                        }
                    }
                }
                return;
            }
        }

        show_context_menu_at(state, hwnd, pt.x, pt.y);
    }
}

fn show_sidebar_context_menu(
    state: &mut AppState,
    hwnd: HWND,
    screen_x: i32,
    screen_y: i32,
    target_catalog_idx: usize,
) {
    unsafe {
        let menu = CreatePopupMenu().unwrap();

        if state.view_mode == ViewMode::Composite && state.layout_frames.len() >= 2 {
            for f_idx in 0..state.layout_frames.len() {
                let label = if state.layout_frames.len() == 2 {
                    if f_idx == 0 { "左枠に反映" } else { "右枠に反映" }
                } else if state.layout_frames.len() == 3 {
                    if f_idx == 0 { "左枠に反映" } else if f_idx == 1 { "中枠に反映" } else { "右枠に反映" }
                } else {
                    "枠に反映"
                };
                let wide_label: Vec<u16> = format!("{}\0", label).encode_utf16().collect();
                let _ = AppendMenuW(
                    menu,
                    MF_STRING,
                    3000 + f_idx,
                    PCWSTR(wide_label.as_ptr()),
                );
            }
            let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR(std::ptr::null()));
        }

        let _ = AppendMenuW(menu, MF_STRING, 3010, w!("この画像を選択"));
        let _ = AppendMenuW(menu, MF_STRING, 3011, w!("ファイルの場所を開く (エクスプローラー)"));
        let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR(std::ptr::null()));
        let _ = AppendMenuW(menu, MF_STRING, 3012, w!("除外リストに追加"));

        let _ = SetForegroundWindow(hwnd);
        let cmd = TrackPopupMenu(
            menu,
            TPM_RETURNCMD | TPM_RIGHTBUTTON,
            screen_x,
            screen_y,
            None,
            hwnd,
            None,
        );
        let _ = DestroyMenu(menu);

        let cmd_u = cmd.0 as u32;
        if (3000..3000 + state.layout_frames.len() as u32).contains(&cmd_u) {
            let f_idx = (cmd_u - 3000) as usize;
            state.apply_image_to_frame(target_catalog_idx, f_idx);
        } else {
            match cmd_u {
                3010 => {
                    state.select_image_by_index(target_catalog_idx);
                }
                3011 => {
                    if let Some(item) = state.catalog.items.get(target_catalog_idx) {
                        let path_str = item.path.to_string_lossy().to_string();
                        let _ = std::process::Command::new("explorer.exe")
                            .arg("/select,")
                            .arg(&path_str)
                            .spawn();
                    }
                }
                3012 => {
                    if let Some(item) = state.catalog.items.get(target_catalog_idx) {
                        let p = item.path.clone();
                        let is_now_excluded = state.catalog.toggle_exclude(&p);
                        state.sync_catalog_excludes();
                        state.set_osd(
                            if is_now_excluded {
                                "画像を除外しました".into()
                            } else {
                                "画像の除外を解除しました".into()
                            },
                            2,
                            false,
                        );
                        state.rebuild_layout(false, false);
                    }
                }
                _ => {}
            }
        }
    }
}

fn show_context_menu_at(state: &mut AppState, hwnd: HWND, screen_x: i32, screen_y: i32) {
    unsafe {
        let menu = CreatePopupMenu().unwrap();
        let _ = AppendMenuW(
            menu,
            MF_STRING,
            1011,
            w!("ファイルの場所を開く (エクスプローラー)"),
        );
        let _ = AppendMenuW(menu, MF_STRING, 1001, w!("フォルダを開く (Ctrl+O)"));

        if !state.config.library.recent_folders.is_empty() {
            let recent_menu = CreatePopupMenu().unwrap();
            for (idx, rf) in state.config.library.recent_folders.iter().take(5).enumerate() {
                let wide_rf: Vec<u16> = rf.encode_utf16().chain(std::iter::once(0)).collect();
                let _ = AppendMenuW(
                    recent_menu,
                    MF_STRING,
                    2000 + idx,
                    PCWSTR(wide_rf.as_ptr()),
                );
            }
            let _ = AppendMenuW(menu, MF_POPUP, recent_menu.0 as usize, w!("最近開いたフォルダ"));
        }

        let _ = AppendMenuW(menu, MF_STRING, 1002, w!("再読込 (F5)"));
        let _ = AppendMenuW(menu, MF_STRING, 1006, w!("画像をランダム入替 (Shift+R)"));
        let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR(std::ptr::null()));
        let _ = AppendMenuW(menu, MF_STRING, 1007, w!("スマートパン (被写体/顔自動合わせ: A)"));
        let _ = AppendMenuW(menu, MF_STRING, 1008, w!("フィルムストリップ表示切替 (T)"));
        let _ = AppendMenuW(menu, MF_STRING, 1009, w!("壁紙プリセット切替 (P)"));
        let _ = AppendMenuW(menu, MF_STRING, 1010, w!("現在の構成をプリセット保存 (Ctrl+P)"));
        let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR(std::ptr::null()));
        let _ = AppendMenuW(menu, MF_STRING, 1003, w!("除外一覧"));
        let _ = AppendMenuW(menu, MF_STRING, 1004, w!("設定"));
        let _ = AppendMenuW(menu, MF_STRING, 1005, w!("操作一覧 (F1)"));

        let _ = SetForegroundWindow(hwnd);
        let cmd = TrackPopupMenu(
            menu,
            TPM_RETURNCMD | TPM_RIGHTBUTTON,
            screen_x,
            screen_y,
            None,
            hwnd,
            None,
        );
        let _ = DestroyMenu(menu);

        match cmd.0 as u32 {
            1011 => state.open_in_explorer(),
            1001 => state.open_folder_dialog(),
            1002 => {
                state.catalog.reload();
                state.rebuild_layout(true, true);
            }
            1006 => state.random_image(),
            1007 => state.apply_smart_pan_current_frame(),
            1008 => {
                state.show_filmstrip = !state.show_filmstrip;
            }
            1009 => state.load_next_preset(),
            1010 => state.save_current_as_preset(),
            1003 => state.modal_dialog = ModalDialog::ExcludeList,
            1004 => state.modal_dialog = ModalDialog::Settings,
            1005 => state.modal_dialog = ModalDialog::Shortcuts,
            c if (2000..2010).contains(&c) => {
                let idx = (c - 2000) as usize;
                if let Some(folder) = state.config.library.recent_folders.get(idx).cloned() {
                    state.open_path(Path::new(&folder));
                }
            }
            _ => {}
        }
    }
}

