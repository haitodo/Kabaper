#![windows_subsystem = "windows"]

pub mod apply;
pub mod catalog;
pub mod compose;
pub mod decode;
pub mod layout;
pub mod monitor;
pub mod random;
pub mod schedule;
pub mod smart_pan;
pub mod store;
pub mod ui;

use std::path::{Path, PathBuf};
use windows::Win32::Foundation::*;
use windows::Win32::System::Com::*;
use windows::Win32::System::DataExchange::COPYDATASTRUCT;
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::*;

use crate::random::execute_random_update;
use crate::store::{AppConfig, Storage, get_instance_id_for_dir};
use crate::ui::{AppState, run_gui};

fn main() {
    let args: Vec<String> = std::env::args().collect();

    // Spec 6.2: Command line interface
    // kabaper.exe -> GUI
    // kabaper.exe <path> -> GUI with path
    // kabaper.exe --random -> Headless update
    // Unknown option or invalid args -> exit code 4 without GUI

    if args.len() > 1 && args[1] == "--random" {
        if args.len() > 2 {
            std::process::exit(4);
        }
        run_headless();
    } else if args.iter().skip(1).any(|a| a.starts_with('-')) || args.len() > 2 {
        // Unknown option or too many arguments -> exit code 4
        eprintln!("Invalid command line arguments");
        std::process::exit(4);
    } else {
        let start_path = if args.len() > 1 {
            Some(PathBuf::from(&args[1]))
        } else {
            None
        };
        run_gui_mode(start_path.as_deref());
    }
}

fn run_headless() -> ! {
    let storage = match Storage::init() {
        Ok(s) => s,
        Err(e) => {
            eprintln!("Failed to initialize storage: {}", e);
            std::process::exit(4);
        }
    };

    let cfg = match storage.load_config_readonly() {
        Ok(c) => c,
        Err(err_msg) => {
            // Spec 6.2: 設定が破損・未作成・未知の新スキーマ・必須項目欠落の場合は退避も行わず終了コード4
            storage.log("ERROR", &format!("Config error in headless: {}", err_msg));
            eprintln!("Config error in headless: {}", err_msg);
            std::process::exit(4);
        }
    };

    let mut wallpaper_mgr = None;
    match execute_random_update(&storage, &cfg, true, &mut wallpaper_mgr) {
        Ok(succ) => {
            storage.log(
                "INFO",
                &format!(
                    "Headless update finished successfully ({} monitors applied)",
                    succ.applied_monitors.len()
                ),
            );
            std::process::exit(0);
        }
        Err(e) => {
            let code = e.exit_code();
            storage.log(
                "ERROR",
                &format!("Headless update failed (exit code {}): {:?}", code, e),
            );
            eprintln!("Headless update failed with code {}: {:?}", code, e);
            std::process::exit(code);
        }
    }
}

fn run_gui_mode(start_path: Option<&Path>) {
    // Initialize COM as Single-Threaded Apartment (STA) for UI thread, Shell dialogs, and WIC.
    // If COM is not initialized as STA, IFileOpenDialog / IFileSaveDialog will deadlock when Show() is called from MTA.
    struct ComGuard(bool);
    impl Drop for ComGuard {
        fn drop(&mut self) {
            if self.0 {
                unsafe {
                    CoUninitialize();
                }
            }
        }
    }

    let _com_guard = unsafe {
        let hr = CoInitializeEx(None, COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE);
        ComGuard(hr.is_ok())
    };

    let storage = match Storage::init() {
        Ok(s) => s,
        Err(e) => {
            unsafe {
                let err_msg = format!("保存先ディレクトリの初期化に失敗しました:\n{}", e);
                let wide: Vec<u16> = err_msg.encode_utf16().chain(std::iter::once(0)).collect();
                let _ = MessageBoxW(
                    None,
                    PCWSTR(wide.as_ptr()),
                    w!("エラー"),
                    MB_OK | MB_ICONERROR,
                );
            }
            std::process::exit(1);
        }
    };

    let class_name = get_instance_id_for_dir(&storage.root_dir);

    // Single instance check per storage directory (Spec 2.1)
    unsafe {
        let wide_class: Vec<u16> = class_name
            .encode_utf16()
            .chain(std::iter::once(0))
            .collect();
        if let Ok(existing_hwnd) = FindWindowW(PCWSTR(wide_class.as_ptr()), PCWSTR::null())
            && existing_hwnd.0 as isize != 0
        {
            let _ = AllowSetForegroundWindow(ASFW_ANY);

            let path_str = start_path
                .map(|p| p.to_string_lossy().to_string())
                .unwrap_or_default();
            let bytes = path_str.as_bytes();

            let cds = COPYDATASTRUCT {
                dwData: 1,
                cbData: bytes.len() as u32,
                lpData: bytes.as_ptr() as *mut _,
            };

            let mut result: usize = 0;
            let send_res = SendMessageTimeoutW(
                existing_hwnd,
                WM_COPYDATA,
                WPARAM(0),
                LPARAM(&cds as *const _ as isize),
                SMTO_ABORTIFHUNG,
                1000,
                Some(&mut result),
            );

            if send_res.0 != 0 {
                // Successfully transmitted to existing instance
                std::process::exit(0);
            } else {
                let err_msg = "既存プロセスが1秒以内に応答しませんでした。";
                let wide: Vec<u16> = err_msg.encode_utf16().chain(std::iter::once(0)).collect();
                let _ = MessageBoxW(
                    None,
                    PCWSTR(wide.as_ptr()),
                    w!("エラー"),
                    MB_OK | MB_ICONERROR,
                );
                std::process::exit(1);
            }
        }
    }

    let state = match AppState::new(storage, AppConfig::default(), start_path) {
        Ok(s) => s,
        Err(e) => {
            unsafe {
                let err_msg = format!("アプリケーションの初期化に失敗しました:\n{}", e);
                let wide: Vec<u16> = err_msg.encode_utf16().chain(std::iter::once(0)).collect();
                let _ = MessageBoxW(
                    None,
                    PCWSTR(wide.as_ptr()),
                    w!("エラー"),
                    MB_OK | MB_ICONERROR,
                );
            }
            std::process::exit(1);
        }
    };

    if let Err(e) = run_gui(state, &class_name) {
        eprintln!("Application terminated with error: {}", e);
    }
}
