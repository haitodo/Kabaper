use std::collections::HashMap;
use std::sync::Mutex;
use windows::Win32::Devices::Display::*;
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::*;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MonitorInfo {
    pub number: usize, // 1-based number
    pub id: String,    // Unique device path or GDI device name
    pub name: String,  // Friendly name (e.g. "DELL U2720Q" or "Display 1")
    pub rect: Rect,    // Virtual desktop rect (left, top, right, bottom)
    pub width: u32,    // Physical px width
    pub height: u32,   // Physical px height
    pub is_primary: bool,
    pub gdi_name: String, // \\.\DISPLAY1
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rect {
    pub left: i32,
    pub top: i32,
    pub right: i32,
    pub bottom: i32,
}

impl Rect {
    pub fn width(&self) -> u32 {
        (self.right - self.left).max(0) as u32
    }
    pub fn height(&self) -> u32 {
        (self.bottom - self.top).max(0) as u32
    }
    pub fn contains_point(&self, x: i32, y: i32) -> bool {
        x >= self.left && x < self.right && y >= self.top && y < self.bottom
    }
}

// Global registry to keep monitor numbers stable during application session (Spec 5.1)
static MONITOR_NUMBER_REGISTRY: Mutex<Option<HashMap<String, usize>>> = Mutex::new(None);

pub fn enumerate_monitors() -> Vec<MonitorInfo> {
    unsafe {
        let mut raw_monitors: Vec<RawMonitor> = Vec::new();
        let _ = EnumDisplayMonitors(
            None,
            None,
            Some(enum_monitor_callback),
            LPARAM(&mut raw_monitors as *mut _ as isize),
        );

        // Fetch friendly names and device paths from QueryDisplayConfig
        let display_configs = query_display_configurations();

        // Correlate GDI devices with friendly names
        let mut monitors: Vec<MonitorInfo> = Vec::new();

        for raw in raw_monitors {
            let (name, device_id) = if let Some(cfg) = display_configs.get(&raw.gdi_name) {
                (cfg.name.clone(), cfg.device_path.clone())
            } else {
                let fallback_name = format!("Display {}", raw.gdi_name.replace(r"\\.\DISPLAY", ""));
                (fallback_name, raw.gdi_name.clone())
            };

            monitors.push(MonitorInfo {
                number: 0, // Assigned below
                id: device_id,
                name,
                rect: raw.rect,
                width: raw.rect.width(),
                height: raw.rect.height(),
                is_primary: raw.is_primary,
                gdi_name: raw.gdi_name,
            });
        }

        // Sort by virtual desktop left ascending, then top ascending (Spec 5.1)
        monitors.sort_by(|a, b| {
            a.rect
                .left
                .cmp(&b.rect.left)
                .then(a.rect.top.cmp(&b.rect.top))
        });

        // Assign stable numbers
        let mut reg_guard = MONITOR_NUMBER_REGISTRY.lock().unwrap();
        let registry = reg_guard.get_or_insert_with(HashMap::new);

        for m in &mut monitors {
            if let Some(&num) = registry.get(&m.id) {
                m.number = num;
            } else {
                // Find smallest unused number starting from 1
                let mut candidate = 1;
                while registry.values().any(|&v| v == candidate) {
                    candidate += 1;
                }
                registry.insert(m.id.clone(), candidate);
                m.number = candidate;
            }
        }

        monitors
    }
}

struct RawMonitor {
    rect: Rect,
    is_primary: bool,
    gdi_name: String,
}

unsafe extern "system" fn enum_monitor_callback(
    hmonitor: HMONITOR,
    _hdc: HDC,
    _lprect: *mut RECT,
    lparam: LPARAM,
) -> BOOL {
    unsafe {
        let list = &mut *(lparam.0 as *mut Vec<RawMonitor>);

        let mut mi = MONITORINFOEXW::default();
        mi.monitorInfo.cbSize = std::mem::size_of::<MONITORINFOEXW>() as u32;

        if GetMonitorInfoW(hmonitor, &mut mi.monitorInfo as *mut _).as_bool() {
            let gdi_name = String::from_utf16_lossy(&mi.szDevice)
                .trim_matches('\0')
                .to_string();

            let rect = Rect {
                left: mi.monitorInfo.rcMonitor.left,
                top: mi.monitorInfo.rcMonitor.top,
                right: mi.monitorInfo.rcMonitor.right,
                bottom: mi.monitorInfo.rcMonitor.bottom,
            };

            let is_primary = (mi.monitorInfo.dwFlags & MONITORINFOF_PRIMARY) != 0;

            list.push(RawMonitor {
                rect,
                is_primary,
                gdi_name,
            });
        }

        BOOL(1)
    }
}

struct DisplayConfigTarget {
    name: String,
    device_path: String,
}

unsafe fn query_display_configurations() -> HashMap<String, DisplayConfigTarget> {
    let mut map = HashMap::new();
    unsafe {
        let mut path_count = 0u32;
        let mut mode_count = 0u32;

        if GetDisplayConfigBufferSizes(QDC_ONLY_ACTIVE_PATHS, &mut path_count, &mut mode_count)
            != ERROR_SUCCESS
        {
            return map;
        }

        let mut paths = vec![DISPLAYCONFIG_PATH_INFO::default(); path_count as usize];
        let mut modes = vec![DISPLAYCONFIG_MODE_INFO::default(); mode_count as usize];

        if QueryDisplayConfig(
            QDC_ONLY_ACTIVE_PATHS,
            &mut path_count,
            paths.as_mut_ptr(),
            &mut mode_count,
            modes.as_mut_ptr(),
            None,
        ) != ERROR_SUCCESS
        {
            return map;
        }

        for path in &paths[..path_count as usize] {
            // Source (GDI name)
            let mut source_name = DISPLAYCONFIG_SOURCE_DEVICE_NAME::default();
            source_name.header.r#type = DISPLAYCONFIG_DEVICE_INFO_GET_SOURCE_NAME;
            source_name.header.size =
                std::mem::size_of::<DISPLAYCONFIG_SOURCE_DEVICE_NAME>() as u32;
            source_name.header.adapterId = path.sourceInfo.adapterId;
            source_name.header.id = path.sourceInfo.id;

            if DisplayConfigGetDeviceInfo(&mut source_name.header) != 0 {
                continue;
            }

            let gdi_name = String::from_utf16_lossy(&source_name.viewGdiDeviceName)
                .trim_matches('\0')
                .to_string();

            // Target (Friendly name & device path)
            let mut target_name = DISPLAYCONFIG_TARGET_DEVICE_NAME::default();
            target_name.header.r#type = DISPLAYCONFIG_DEVICE_INFO_GET_TARGET_NAME;
            target_name.header.size =
                std::mem::size_of::<DISPLAYCONFIG_TARGET_DEVICE_NAME>() as u32;
            target_name.header.adapterId = path.targetInfo.adapterId;
            target_name.header.id = path.targetInfo.id;

            let (friendly_name, dev_path) =
                if DisplayConfigGetDeviceInfo(&mut target_name.header) == 0 {
                    let name = String::from_utf16_lossy(&target_name.monitorFriendlyDeviceName)
                        .trim_matches('\0')
                        .to_string();
                    let dev_path = String::from_utf16_lossy(&target_name.monitorDevicePath)
                        .trim_matches('\0')
                        .to_string();
                    (
                        if name.is_empty() {
                            format!("Display {}", gdi_name.replace(r"\\.\DISPLAY", ""))
                        } else {
                            name
                        },
                        if dev_path.is_empty() {
                            gdi_name.clone()
                        } else {
                            dev_path
                        },
                    )
                } else {
                    (
                        format!("Display {}", gdi_name.replace(r"\\.\DISPLAY", "")),
                        gdi_name.clone(),
                    )
                };

            map.insert(
                gdi_name,
                DisplayConfigTarget {
                    name: friendly_name,
                    device_path: dev_path,
                },
            );
        }
    }

    map
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_enumerate_monitors() {
        let monitors = enumerate_monitors();
        // There should be at least 1 monitor in an active Windows interactive session
        // (or in a test container, may be 1)
        println!("Detected monitors count: {}", monitors.len());
        for m in &monitors {
            println!(
                "Monitor #{}: {} ({}x{}) id: {}",
                m.number, m.name, m.width, m.height, m.id
            );
            assert!(m.number >= 1);
            assert!(m.width > 0);
            assert!(m.height > 0);
        }
    }

    #[test]
    fn test_rect_functions() {
        let r = Rect {
            left: 0,
            top: 0,
            right: 1920,
            bottom: 1080,
        };
        assert_eq!(r.width(), 1920);
        assert_eq!(r.height(), 1080);
        assert!(r.contains_point(100, 100));
        assert!(!r.contains_point(2000, 100));
    }
}
