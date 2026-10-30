use std::path::{Path, PathBuf};
use windows::Win32::System::Com::*;
use windows::Win32::UI::Shell::*;
use windows::core::*;

#[derive(Debug, Clone)]
pub struct MonitorWallpaperSnapshot {
    pub monitor_id: String,
    pub wallpaper_path: PathBuf,
}

#[derive(Debug, Clone)]
pub struct UndoSnapshot {
    pub position: DESKTOP_WALLPAPER_POSITION,
    pub monitor_wallpapers: Vec<MonitorWallpaperSnapshot>,
}

pub struct WallpaperManager {
    wallpaper_api: IDesktopWallpaper,
    last_undo_snapshot: Option<UndoSnapshot>,
}

impl WallpaperManager {
    pub fn new() -> Result<Self> {
        unsafe {
            let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
            let wallpaper_api: IDesktopWallpaper =
                CoCreateInstance(&DesktopWallpaper, None, CLSCTX_ALL)?;
            Ok(Self {
                wallpaper_api,
                last_undo_snapshot: None,
            })
        }
    }

    pub fn get_position(&self) -> Result<DESKTOP_WALLPAPER_POSITION> {
        unsafe { self.wallpaper_api.GetPosition() }
    }

    pub fn set_position(&self, pos: DESKTOP_WALLPAPER_POSITION) -> Result<()> {
        unsafe { self.wallpaper_api.SetPosition(pos) }
    }

    pub fn get_current_wallpaper(&self, monitor_id: &str) -> Result<PathBuf> {
        unsafe {
            let wide_id: Vec<u16> = monitor_id
                .encode_utf16()
                .chain(std::iter::once(0))
                .collect();
            let pwstr = self.wallpaper_api.GetWallpaper(PCWSTR(wide_id.as_ptr()))?;
            let s = pwstr.to_string()?;
            CoTaskMemFree(Some(pwstr.0 as *const _ as *mut _));
            Ok(PathBuf::from(s))
        }
    }

    /// Check if position needs changing to Fill (DWPOS_FILL).
    /// Returns true if position is Span, Tile, or Center.
    pub fn needs_fill_change(&self) -> bool {
        if let Ok(pos) = self.get_position() {
            matches!(pos, DWPOS_SPAN | DWPOS_TILE | DWPOS_CENTER)
        } else {
            false
        }
    }

    /// Capture snapshot before wallpaper modification for undo (Spec 5.4)
    pub fn capture_snapshot(&mut self, monitor_ids: &[String]) {
        let current_pos = self.get_position().unwrap_or(DWPOS_FILL);
        let mut monitor_wallpapers = Vec::new();

        for id in monitor_ids {
            if let Ok(p) = self.get_current_wallpaper(id) {
                // If path is not empty
                if !p.as_os_str().is_empty() {
                    monitor_wallpapers.push(MonitorWallpaperSnapshot {
                        monitor_id: id.clone(),
                        wallpaper_path: p,
                    });
                }
            }
        }

        self.last_undo_snapshot = Some(UndoSnapshot {
            position: current_pos,
            monitor_wallpapers,
        });
    }

    pub fn has_undo(&self) -> bool {
        self.last_undo_snapshot.is_some()
    }

    pub fn last_undo_snapshot(&self) -> Option<&UndoSnapshot> {
        self.last_undo_snapshot.as_ref()
    }

    /// Undo last wallpaper change (Ctrl+Z)
    pub fn undo(&mut self) -> Result<Vec<String>> {
        let Some(snapshot) = self.last_undo_snapshot.take() else {
            return Err(Error::new(
                HRESULT(0x80004005u32 as i32),
                "No wallpaper history to undo",
            ));
        };

        // Restore position
        let _ = self.set_position(snapshot.position);

        let mut restored_monitors = Vec::new();
        let mut unrecoverable_monitors = Vec::new();

        for item in snapshot.monitor_wallpapers {
            // Check if file still exists
            if !item.wallpaper_path.exists() {
                unrecoverable_monitors.push(item.monitor_id);
                continue;
            }

            if self
                .apply_wallpaper_raw(&item.monitor_id, &item.wallpaper_path)
                .is_ok()
            {
                restored_monitors.push(item.monitor_id);
            } else {
                unrecoverable_monitors.push(item.monitor_id);
            }
        }

        if !unrecoverable_monitors.is_empty() && restored_monitors.is_empty() {
            return Err(Error::new(
                HRESULT(0x80004005u32 as i32),
                "Previous wallpaper file was deleted or cannot be restored",
            ));
        }

        Ok(restored_monitors)
    }

    pub fn apply_wallpaper(
        &mut self,
        monitor_id: &str,
        path: &Path,
        ensure_fill: bool,
    ) -> Result<()> {
        if ensure_fill && self.needs_fill_change() {
            let _ = self.set_position(DWPOS_FILL);
        }

        self.apply_wallpaper_raw(monitor_id, path)
    }

    fn apply_wallpaper_raw(&self, monitor_id: &str, path: &Path) -> Result<()> {
        unsafe {
            let wide_id: Vec<u16> = monitor_id
                .encode_utf16()
                .chain(std::iter::once(0))
                .collect();
            let wide_path: Vec<u16> = path
                .as_os_str()
                .encode_wide()
                .chain(std::iter::once(0))
                .collect();

            self.wallpaper_api
                .SetWallpaper(PCWSTR(wide_id.as_ptr()), PCWSTR(wide_path.as_ptr()))
        }
    }

    /// List active monitor device IDs from IDesktopWallpaper
    pub fn get_wallpaper_monitor_ids(&self) -> Vec<String> {
        let mut result = Vec::new();
        unsafe {
            if let Ok(count) = self.wallpaper_api.GetMonitorDevicePathCount() {
                for i in 0..count {
                    if let Ok(pwstr) = self.wallpaper_api.GetMonitorDevicePathAt(i) {
                        if let Ok(s) = pwstr.to_string() {
                            result.push(s);
                        }
                        CoTaskMemFree(Some(pwstr.0 as *const _ as *mut _));
                    }
                }
            }
        }
        result
    }
}

use std::os::windows::ffi::OsStrExt;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_wallpaper_manager_instantiation() {
        let mgr = WallpaperManager::new();
        assert!(mgr.is_ok());
        if let Ok(m) = mgr {
            let pos = m.get_position();
            println!("Current desktop wallpaper position: {:?}", pos);
            let ids = m.get_wallpaper_monitor_ids();
            println!("Desktop wallpaper monitor ids count: {}", ids.len());
        }
    }
}
