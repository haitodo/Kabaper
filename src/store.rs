use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use windows::Win32::Foundation::*;
use windows::Win32::Storage::FileSystem::*;
use windows::Win32::System::IO::OVERLAPPED;
use windows::Win32::UI::Shell::*;
use windows::core::*;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct AppConfig {
    pub schema_version: u32,
    pub library: LibraryConfig,
    pub layout: LayoutConfig,
    pub target: TargetConfig,
    pub random: RandomConfig,
    pub schedule: ScheduleConfig,
    pub window: WindowConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct LibraryConfig {
    pub folder: Option<String>,
    pub last_image: Option<String>,
    pub excluded: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct LayoutConfig {
    pub max_scale: f64,
    pub border_px: u32,
    pub mode: String, // "auto" | "single" | "split2" | "split3"
}

impl Default for LayoutConfig {
    fn default() -> Self {
        Self {
            max_scale: 1.25,
            border_px: 2,
            mode: "auto".to_string(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct TargetConfig {
    pub monitor_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct RandomConfig {
    pub mode: String, // "auto" | "single" | "split2" | "split3"
}

impl Default for RandomConfig {
    fn default() -> Self {
        Self {
            mode: "auto".to_string(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ScheduleConfig {
    pub enabled: bool,
    pub on_logon: bool,
    pub interval_minutes: u32, // 0 or 5..=1440
}

impl Default for ScheduleConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            on_logon: true,
            interval_minutes: 60,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct WindowConfig {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
    pub maximized: bool,
    pub fullscreen: bool,
    pub show_info: bool,
}

impl Default for WindowConfig {
    fn default() -> Self {
        Self {
            x: 100,
            y: 100,
            width: 1280,
            height: 800,
            maximized: false,
            fullscreen: false,
            show_info: false,
        }
    }
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            schema_version: 1,
            library: LibraryConfig::default(),
            layout: LayoutConfig::default(),
            target: TargetConfig::default(),
            random: RandomConfig::default(),
            schedule: ScheduleConfig::default(),
            window: WindowConfig::default(),
        }
    }
}

pub struct Storage {
    pub root_dir: PathBuf,
    pub wallpapers_dir: PathBuf,
    pub logs_dir: PathBuf,
    pub lock_path: PathBuf,
    pub config_path: PathBuf,
    pub is_future_schema: bool,
}

impl Storage {
    pub fn init() -> Result<Self> {
        let root = resolve_storage_dir();
        Self::init_with_dir(root)
    }

    pub fn init_with_dir(root: PathBuf) -> Result<Self> {
        fs::create_dir_all(&root).map_err(|e| {
            Error::new(
                HRESULT(0x80004005u32 as i32),
                format!(
                    "Failed to create storage directory {}: {}",
                    root.display(),
                    e
                ),
            )
        })?;

        let wallpapers_dir = root.join("wallpapers");
        let logs_dir = root.join("logs");
        let lock_path = root.join("lock");
        let config_path = root.join("config.toml");

        fs::create_dir_all(&wallpapers_dir).map_err(|e| {
            Error::new(
                HRESULT(0x80004005u32 as i32),
                format!("Failed to create wallpapers directory: {}", e),
            )
        })?;
        fs::create_dir_all(&logs_dir).map_err(|e| {
            Error::new(
                HRESULT(0x80004005u32 as i32),
                format!("Failed to create logs directory: {}", e),
            )
        })?;

        Ok(Self {
            root_dir: root,
            wallpapers_dir,
            logs_dir,
            lock_path,
            config_path,
            is_future_schema: false,
        })
    }

    pub fn load_config(&mut self) -> (AppConfig, Option<String>) {
        if !self.config_path.exists() {
            let def = AppConfig::default();
            let _ = self.save_config(&def);
            return (def, None);
        }

        let content = match fs::read_to_string(&self.config_path) {
            Ok(c) => c,
            Err(e) => {
                let err_msg = format!("Cannot read config file: {}", e);
                return (AppConfig::default(), Some(err_msg));
            }
        };

        // Check TOML parsing
        let parsed = toml::from_str::<toml::Value>(&content);
        let value = match parsed {
            Ok(v) => v,
            Err(_e) => {
                // Spec 7.2: If TOML syntax error, backup to config.toml.corrupt.<utc_ms>
                let now_ms = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_millis();
                let corrupt_path = self
                    .root_dir
                    .join(format!("config.toml.corrupt.{}", now_ms));
                let _ = fs::copy(&self.config_path, &corrupt_path);
                let def = AppConfig::default();
                let _ = self.save_config(&def);
                return (
                    def,
                    Some(format!(
                        "Config file was corrupt and has been backed up to {}",
                        corrupt_path.display()
                    )),
                );
            }
        };

        // Check schema_version
        if let Some(schema) = value.get("schema_version").and_then(|v| v.as_integer())
            && schema > 1
        {
            // Spec 7.2: schema_version > 1 => do not overwrite, start with read-only defaults
            self.is_future_schema = true;
            return (
                AppConfig::default(),
                Some(format!(
                    "Config file uses schema_version {} (higher than supported). Running with default settings in read-only mode.",
                    schema
                )),
            );
        }

        // Parse AppConfig, tolerating missing or invalid fields by falling back to default
        let mut cfg: AppConfig = toml::from_str(&content).unwrap_or_default();

        // Clamp values to valid ranges (Spec 7.2)
        cfg.layout.max_scale = (cfg.layout.max_scale * 20.0).round() / 20.0;
        cfg.layout.max_scale = cfg.layout.max_scale.clamp(1.00, 2.50);
        cfg.layout.border_px = cfg.layout.border_px.clamp(0, 8);
        if !matches!(
            cfg.layout.mode.as_str(),
            "auto" | "single" | "split2" | "split3"
        ) {
            cfg.layout.mode = "auto".to_string();
        }
        if !matches!(
            cfg.random.mode.as_str(),
            "auto" | "single" | "split2" | "split3"
        ) {
            cfg.random.mode = "auto".to_string();
        }
        if cfg.schedule.interval_minutes != 0 {
            cfg.schedule.interval_minutes = cfg.schedule.interval_minutes.clamp(5, 1440);
        }

        (cfg, None)
    }

    /// Read-only config loader for headless mode (Spec 6.2).
    /// Does not write files, does not backup corrupt files, fails on missing/corrupt/future schema.
    pub fn load_config_readonly(&self) -> std::result::Result<AppConfig, String> {
        if !self.config_path.exists() {
            return Err("Config file does not exist".to_string());
        }

        let content = fs::read_to_string(&self.config_path)
            .map_err(|e| format!("Cannot read config file: {}", e))?;

        // Check TOML parsing
        let value: toml::Value =
            toml::from_str(&content).map_err(|e| format!("Config file is corrupt: {}", e))?;

        // Check schema_version
        if let Some(schema) = value.get("schema_version").and_then(|v| v.as_integer()) {
            if schema > 1 {
                return Err(format!("Unknown future schema_version: {}", schema));
            }
        } else {
            return Err("Missing required field 'schema_version'".to_string());
        }

        let mut cfg: AppConfig = toml::from_str(&content)
            .map_err(|e| format!("Failed to parse config schema: {}", e))?;

        // Clamp values to valid ranges (Spec 7.2)
        cfg.layout.max_scale = (cfg.layout.max_scale * 20.0).round() / 20.0;
        cfg.layout.max_scale = cfg.layout.max_scale.clamp(1.00, 2.50);
        cfg.layout.border_px = cfg.layout.border_px.clamp(0, 8);
        if !matches!(
            cfg.layout.mode.as_str(),
            "auto" | "single" | "split2" | "split3"
        ) {
            cfg.layout.mode = "auto".to_string();
        }
        if !matches!(
            cfg.random.mode.as_str(),
            "auto" | "single" | "split2" | "split3"
        ) {
            cfg.random.mode = "auto".to_string();
        }
        if cfg.schedule.interval_minutes != 0 {
            cfg.schedule.interval_minutes = cfg.schedule.interval_minutes.clamp(5, 1440);
        }

        Ok(cfg)
    }

    pub fn save_config(&self, cfg: &AppConfig) -> Result<()> {
        if self.is_future_schema {
            return Ok(()); // Do not overwrite future schema
        }

        let toml_str = toml::to_string_pretty(cfg).map_err(|e| {
            Error::new(
                HRESULT(0x80004005u32 as i32),
                format!("Failed to serialize config: {}", e),
            )
        })?;

        // Atomic write via temp file in same directory (Spec 7.2)
        let temp_path = self
            .root_dir
            .join(format!(".config.tmp.{}", std::process::id()));
        {
            let mut f = File::create(&temp_path).map_err(|e| {
                Error::new(
                    HRESULT(0x80004005u32 as i32),
                    format!("Failed to create config temp file: {}", e),
                )
            })?;
            f.write_all(toml_str.as_bytes()).map_err(|e| {
                Error::new(
                    HRESULT(0x80004005u32 as i32),
                    format!("Failed to write config: {}", e),
                )
            })?;
            f.sync_all().map_err(|e| {
                Error::new(
                    HRESULT(0x80004005u32 as i32),
                    format!("Failed to sync config: {}", e),
                )
            })?;
        }

        if let Err(e) = fs::rename(&temp_path, &self.config_path) {
            if fs::copy(&temp_path, &self.config_path).is_ok() {
                let _ = fs::remove_file(&temp_path);
            } else {
                let _ = fs::remove_file(&temp_path);
                return Err(Error::new(
                    HRESULT(0x80004005u32 as i32),
                    format!(
                        "Failed to save config to {}: {}",
                        self.config_path.display(),
                        e
                    ),
                ));
            }
        }

        Ok(())
    }

    /// Try non-blocking lock (headless --random). If already locked, returns None immediately.
    pub fn try_lock_exclusive(&self) -> Option<ExclusiveLockGuard> {
        ExclusiveLockGuard::acquire(&self.lock_path, Duration::from_millis(0))
    }

    /// Lock with timeout (GUI apply, up to 5s).
    pub fn lock_exclusive_timeout(&self, timeout: Duration) -> Option<ExclusiveLockGuard> {
        ExclusiveLockGuard::acquire(&self.lock_path, timeout)
    }

    /// Clean up old wallpapers in wallpapers/ (Spec 7.3)
    pub fn cleanup_wallpapers(&self, protected_files: &HashSet<PathBuf>) {
        if let Ok(entries) = fs::read_dir(&self.wallpapers_dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_file() {
                    let norm = crate::catalog::normalize_path(&path);
                    if !protected_files.contains(&norm) && !protected_files.contains(&path) {
                        let _ = fs::remove_file(&path);
                    }
                }
            }
        }
    }

    /// Append log line with rotation (Spec 7.3: 1MiB per file, max 5 files)
    pub fn log(&self, level: &str, message: &str) {
        let log_file = self.logs_dir.join("app.log");
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        // Check file size for rotation (1MiB)
        if let Ok(meta) = fs::metadata(&log_file)
            && meta.len() >= 1024 * 1024
        {
            self.rotate_logs();
        }

        if let Ok(mut f) = OpenOptions::new().create(true).append(true).open(&log_file) {
            let line = format!("[{}] [{}] {}\n", now, level, message);
            let _ = f.write_all(line.as_bytes());
        }
    }

    fn rotate_logs(&self) {
        // Rotates: app.log.4 -> delete, app.log.3 -> 4, 2 -> 3, 1 -> 2, app.log -> 1
        let max_files = 5;
        let last_log = self.logs_dir.join(format!("app.log.{}", max_files - 1));
        let _ = fs::remove_file(last_log);

        for i in (1..max_files - 1).rev() {
            let from = self.logs_dir.join(format!("app.log.{}", i));
            let to = self.logs_dir.join(format!("app.log.{}", i + 1));
            let _ = fs::rename(from, to);
        }

        let main_log = self.logs_dir.join("app.log");
        let first_rotated = self.logs_dir.join("app.log.1");
        let _ = fs::rename(main_log, first_rotated);
    }
}

pub struct ExclusiveLockGuard {
    handle: HANDLE,
    #[allow(dead_code)]
    lock_file: PathBuf,
}

impl ExclusiveLockGuard {
    pub fn acquire(path: &Path, timeout: Duration) -> Option<Self> {
        let start = Instant::now();
        loop {
            if let Some(guard) = Self::try_acquire_once(path) {
                return Some(guard);
            }
            if start.elapsed() >= timeout {
                return None;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    fn try_acquire_once(path: &Path) -> Option<Self> {
        unsafe {
            let wide_path: Vec<u16> = path
                .as_os_str()
                .encode_wide()
                .chain(std::iter::once(0))
                .collect();

            let handle = CreateFileW(
                PCWSTR(wide_path.as_ptr()),
                GENERIC_READ.0 | GENERIC_WRITE.0,
                FILE_SHARE_READ | FILE_SHARE_WRITE,
                None,
                OPEN_ALWAYS,
                FILE_ATTRIBUTE_NORMAL,
                None,
            )
            .ok()?;

            let mut overlapped = OVERLAPPED::default();
            let flags = LOCKFILE_EXCLUSIVE_LOCK | LOCKFILE_FAIL_IMMEDIATELY;

            let locked = LockFileEx(handle, flags, None, 1, 0, &mut overlapped);
            if locked.is_ok() {
                Some(Self {
                    handle,
                    lock_file: path.to_path_buf(),
                })
            } else {
                let _ = CloseHandle(handle);
                None
            }
        }
    }
}

impl Drop for ExclusiveLockGuard {
    fn drop(&mut self) {
        unsafe {
            let mut overlapped = OVERLAPPED::default();
            let _ = UnlockFileEx(self.handle, None, 1, 0, &mut overlapped);
            let _ = CloseHandle(self.handle);
        }
    }
}

unsafe impl Send for ExclusiveLockGuard {}
unsafe impl Sync for ExclusiveLockGuard {}

pub fn resolve_storage_dir() -> PathBuf {
    // Check if portable.flag exists in the same directory as exe
    if let Ok(exe_path) = std::env::current_exe()
        && let Some(exe_dir) = exe_path.parent()
        && exe_dir.join("portable.flag").exists()
    {
        return exe_dir.join("data");
    }

    // Default: %LOCALAPPDATA%\Kabaper
    if let Ok(local_appdata) = std::env::var("LOCALAPPDATA") {
        return PathBuf::from(local_appdata).join("Kabaper");
    }

    // Fallback: SHGetKnownFolderPath(FOLDERID_LocalAppData)
    unsafe {
        if let Ok(pwstr) = SHGetKnownFolderPath(&FOLDERID_LocalAppData, KNOWN_FOLDER_FLAG(0), None)
        {
            let res = pwstr.to_string();
            windows::Win32::System::Com::CoTaskMemFree(Some(pwstr.0 as *const _ as *mut _));
            if let Ok(s) = res {
                return PathBuf::from(s).join("Kabaper");
            }
        }
    }

    PathBuf::from(r"C:\ProgramData\Kabaper")
}

pub fn get_instance_id_for_dir(dir: &Path) -> String {
    use std::hash::{DefaultHasher, Hash, Hasher};
    let mut hasher = DefaultHasher::new();
    crate::catalog::normalize_path(dir).hash(&mut hasher);
    format!("Kabaper_{:016x}", hasher.finish())
}

use std::os::windows::ffi::OsStrExt;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_storage_init_and_config() {
        let temp =
            std::env::temp_dir().join(format!("kabaper_test_storage_{}", std::process::id()));
        let _ = fs::remove_dir_all(&temp);

        let mut storage = Storage::init_with_dir(temp.clone()).unwrap();
        let (cfg, err) = storage.load_config();
        assert!(err.is_none());
        assert_eq!(cfg.schema_version, 1);
        assert_eq!(cfg.layout.max_scale, 1.25);

        // Test saving
        let mut modified = cfg;
        modified.layout.max_scale = 1.35;
        assert!(storage.save_config(&modified).is_ok());

        let (reloaded, _) = storage.load_config();
        assert!((reloaded.layout.max_scale - 1.35).abs() < 1e-6);

        // Test exclusive lock
        let lock1 = storage.try_lock_exclusive();
        assert!(lock1.is_some());
        // Second lock attempt should fail immediately
        let lock2 = storage.try_lock_exclusive();
        assert!(lock2.is_none());

        // Release lock1
        drop(lock1);
        let lock3 = storage.try_lock_exclusive();
        assert!(lock3.is_some());

        let _ = fs::remove_dir_all(&temp);
    }

    #[test]
    fn test_corrupt_config_backup() {
        let temp =
            std::env::temp_dir().join(format!("kabaper_test_corrupt_{}", std::process::id()));
        let _ = fs::remove_dir_all(&temp);

        let mut storage = Storage::init_with_dir(temp.clone()).unwrap();
        // Write invalid toml
        fs::write(&storage.config_path, "invalid toml {[[[[").unwrap();

        let (cfg, err) = storage.load_config();
        assert!(err.is_some());
        assert_eq!(cfg.schema_version, 1);

        // Verify a .corrupt file was created
        let mut corrupt_found = false;
        for entry in fs::read_dir(&temp).unwrap().flatten() {
            if entry
                .file_name()
                .to_string_lossy()
                .contains("config.toml.corrupt")
            {
                corrupt_found = true;
                break;
            }
        }
        assert!(corrupt_found);

        let _ = fs::remove_dir_all(&temp);
    }

    #[test]
    fn test_load_config_readonly_spec_6_2() {
        let temp =
            std::env::temp_dir().join(format!("kabaper_test_readonly_{}", std::process::id()));
        let _ = fs::remove_dir_all(&temp);

        let storage = Storage::init_with_dir(temp.clone()).unwrap();

        // 1. Missing file: must return Err, must NOT create config.toml
        let res_missing = storage.load_config_readonly();
        assert!(res_missing.is_err());
        assert!(!storage.config_path.exists());

        // 2. Corrupt file: must return Err, must NOT create .corrupt file
        fs::write(&storage.config_path, "corrupted = [[[toml syntax").unwrap();
        let res_corrupt = storage.load_config_readonly();
        assert!(res_corrupt.is_err());
        let corrupt_found = fs::read_dir(&temp).unwrap().flatten().any(|e| {
            e.file_name()
                .to_string_lossy()
                .contains("config.toml.corrupt")
        });
        assert!(
            !corrupt_found,
            "Readonly mode must not create corrupt backup files"
        );

        // 3. Future schema: must return Err
        fs::write(&storage.config_path, "schema_version = 2\n").unwrap();
        let res_future = storage.load_config_readonly();
        assert!(res_future.is_err());

        // 4. Valid config: must succeed and clamp
        let valid_toml = r#"
schema_version = 1
[layout]
max_scale = 3.0
border_px = 20
mode = "split2"
"#;
        fs::write(&storage.config_path, valid_toml).unwrap();
        let res_valid = storage.load_config_readonly();
        assert!(res_valid.is_ok());
        let cfg = res_valid.unwrap();
        assert_eq!(cfg.layout.max_scale, 2.50);
        assert_eq!(cfg.layout.border_px, 8);
        assert_eq!(cfg.layout.mode, "split2");

        let _ = fs::remove_dir_all(&temp);
    }
}
