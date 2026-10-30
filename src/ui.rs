use std::collections::{HashMap, HashSet};
use std::hash::{DefaultHasher, Hash, Hasher};
use std::os::windows::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::System::Com::*;
use windows::Win32::System::DataExchange::COPYDATASTRUCT;
use windows::Win32::UI::HiDpi::*;
use windows::Win32::UI::Input::KeyboardAndMouse::*;
use windows::Win32::UI::Shell::*;
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::*;

use crate::apply::WallpaperManager;
use crate::catalog::{Catalog, is_supported_image, normalize_path};
use crate::compose::{ComposedBitmap, compose_layout, save_png_wic};
use crate::decode::{DecodedImage, ImageDecoder};
use crate::layout::{
    FrameInput, ImageId, ImageSpec, LayoutMode, LayoutPlan, LayoutRequest, ScaleLimit, plan_layout,
};
use crate::monitor::{MonitorInfo, enumerate_monitors};
use crate::random::execute_random_update;
use crate::schedule::SchedulerManager;
use crate::store::{AppConfig, Storage};

pub const OVERLAY_CLASS_NAME: PCWSTR = w!("KabaperOverlayClass");
static SEQ_COUNTER: AtomicU32 = AtomicU32::new(1);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ViewMode {
    Normal,
    Composite,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModalDialog {
    None,
    Settings,
    ExcludeList,
    Shortcuts,
}

pub struct AppState {
    pub storage: Storage,
    pub config: AppConfig,
    pub catalog: Catalog,
    pub decoder: ImageDecoder,
    pub monitors: Vec<MonitorInfo>,
    pub target_monitor_idx: usize,
    pub view_mode: ViewMode,
    pub layout_frames: Vec<FrameInput>,
    pub selected_frame_idx: usize,
    pub cached_images: HashMap<ImageId, DecodedImage>,
    pub active_plan: Option<LayoutPlan>,
    pub active_composed: Option<ComposedBitmap>,
    pub wallpaper_mgr: Option<WallpaperManager>,
    pub scheduler_mgr: Option<SchedulerManager>,
    pub fullscreen: bool,
    pub normal_window_rect: RECT,
    pub show_info: bool,
    pub osd: Option<(String, Instant, Duration, bool)>, // (msg, start, duration, has_undo_button)
    pub error_banner: Vec<String>,
    pub bar_visible: bool,
    pub last_interaction: Instant,
    pub is_panning: bool,
    pub pan_start_mouse: (i32, i32),
    pub pan_start_val: (f64, f64),
    pub modal_dialog: ModalDialog,
    pub hwnd: HWND,
    pub config_dirty: bool,
    pub last_config_change: Instant,
    pub recent_wallpapers: HashMap<String, Vec<PathBuf>>,
}

impl AppState {
    pub fn new(
        mut storage: Storage,
        _initial_config: AppConfig,
        start_path: Option<&Path>,
    ) -> Result<Self> {
        let (config, cfg_err) = storage.load_config();

        let mut error_banner = Vec::new();
        if let Some(err) = cfg_err {
            error_banner.push(err);
        }

        let decoder = ImageDecoder::new()?;
        let monitors = enumerate_monitors();

        let excluded_set: HashSet<PathBuf> = config
            .library
            .excluded
            .iter()
            .map(|s| normalize_path(Path::new(s)))
            .collect();
        let catalog = Catalog::new(excluded_set);

        // Determine target monitor (Spec 5.1: prefer saved, then window center, then primary)
        let win_cx = config.window.x + (config.window.width as i32) / 2;
        let win_cy = config.window.y + (config.window.height as i32) / 2;

        let mut target_idx = None;
        if let Some(target_id) = &config.target.monitor_id {
            target_idx = monitors.iter().position(|m| &m.id == target_id);
        }
        if target_idx.is_none() {
            target_idx = monitors
                .iter()
                .position(|m| m.rect.contains_point(win_cx, win_cy));
        }
        if target_idx.is_none() {
            target_idx = monitors.iter().position(|m| m.is_primary);
        }
        let target_idx = target_idx.unwrap_or(0);

        let wallpaper_mgr = WallpaperManager::new().ok();
        let is_portable = storage.root_dir.ends_with("data");
        let scheduler_mgr = SchedulerManager::new(&storage.root_dir, is_portable).ok();

        // Check scheduler exe path repair
        if let Some(sched) = &scheduler_mgr {
            if let Ok(exe) = std::env::current_exe()
                && let Err(e) = sched.check_and_repair_exe_path(&exe)
            {
                error_banner.push(format!("タスクスケジューラ検証エラー: {}", e));
            }
        } else if config.schedule.enabled {
            error_banner.push("タスクスケジューラサービスに接続できません".into());
        }

        let mut state = Self {
            storage,
            config,
            catalog,
            decoder,
            monitors,
            target_monitor_idx: target_idx,
            view_mode: ViewMode::Composite,
            layout_frames: Vec::new(),
            selected_frame_idx: 0,
            cached_images: HashMap::new(),
            active_plan: None,
            active_composed: None,
            wallpaper_mgr,
            scheduler_mgr,
            fullscreen: false,
            normal_window_rect: RECT::default(),
            show_info: false,
            osd: None,
            error_banner,
            bar_visible: true,
            last_interaction: Instant::now(),
            is_panning: false,
            pan_start_mouse: (0, 0),
            pan_start_val: (0.5, 0.5),
            modal_dialog: ModalDialog::None,
            hwnd: HWND(std::ptr::null_mut()),
            config_dirty: false,
            last_config_change: Instant::now(),
            recent_wallpapers: HashMap::new(),
        };

        state.show_info = state.config.window.show_info;

        // Open library folder per Section 2.1
        if let Some(p) = start_path {
            state.open_path(p);
        } else if let Some(folder_str) = state.config.library.folder.clone() {
            let folder = PathBuf::from(&folder_str);
            if folder.is_dir() {
                let last_img = state.config.library.last_image.as_deref().map(Path::new);
                state.catalog.open_folder(&folder, last_img);
                state.rebuild_layout(true, true);
            } else {
                state.rebuild_layout(true, true);
            }
        } else {
            state.rebuild_layout(true, true);
        }

        if let Some(err) = state.error_banner.first() {
            state.set_osd(err.clone(), 5, false);
        }

        Ok(state)
    }

    pub fn set_osd(&mut self, text: String, duration_secs: u64, has_undo: bool) {
        self.osd = Some((
            text,
            Instant::now(),
            Duration::from_secs(duration_secs),
            has_undo,
        ));
    }

    pub fn mark_config_dirty(&mut self) {
        self.config_dirty = true;
        self.last_config_change = Instant::now();
    }

    pub fn target_monitor(&self) -> Option<&MonitorInfo> {
        self.monitors.get(self.target_monitor_idx)
    }

    pub fn has_pinned_frames(&self) -> bool {
        self.layout_frames.iter().any(|f| f.pinned)
    }

    pub fn open_path(&mut self, p: &Path) {
        if p.is_file() {
            if is_supported_image(p) {
                if let Some(parent) = p.parent()
                    && self.catalog.open_folder(parent, Some(p))
                {
                    self.config.library.folder = Some(parent.to_string_lossy().to_string());
                    self.config.library.last_image = Some(p.to_string_lossy().to_string());
                    self.mark_config_dirty();
                    self.rebuild_layout(true, true);
                    self.set_osd(format!("ファイルを開きました: {}", p.display()), 3, false);
                }
            } else {
                self.set_osd(
                    format!("非対応のファイル形式です: {}", p.display()),
                    3,
                    false,
                );
            }
        } else if p.is_dir() {
            if self.catalog.open_folder(p, None) {
                self.config.library.folder = Some(p.to_string_lossy().to_string());
                self.config.library.last_image = self
                    .catalog
                    .current_item()
                    .map(|it| it.path.to_string_lossy().to_string());
                self.mark_config_dirty();
                self.rebuild_layout(true, true);
                self.set_osd(format!("フォルダを開きました: {}", p.display()), 3, false);
            } else {
                self.set_osd(
                    format!("フォルダを開けませんでした: {}", p.display()),
                    3,
                    false,
                );
            }
        } else {
            self.set_osd(
                format!("指定されたパスが存在しません: {}", p.display()),
                3,
                false,
            );
        }
    }

    pub fn rebuild_layout(&mut self, reset_frames: bool, auto_evaluate: bool) {
        if self.catalog.items.is_empty() {
            self.layout_frames.clear();
            self.active_plan = None;
            self.active_composed = None;
            self.prune_image_cache();
            return;
        }

        let Some(monitor) = self.target_monitor() else {
            return;
        };
        let (mw, mh) = (monitor.width, monitor.height);

        // Spec 3.3: Do NOT change frame count during composite image browsing, pan, frame swap, or pin toggle
        let layout_mode = match self.config.layout.mode.as_str() {
            "single" => LayoutMode::Fixed(1),
            "split2" => LayoutMode::Fixed(2),
            "split3" => LayoutMode::Fixed(3),
            _ => {
                if auto_evaluate {
                    LayoutMode::Auto
                } else {
                    let count = self.layout_frames.len().clamp(1, 3) as u8;
                    LayoutMode::Fixed(count)
                }
            }
        };

        // Prepare frames
        if reset_frames || self.layout_frames.is_empty() {
            let cur_spec = self
                .catalog
                .current_item()
                .and_then(|it| self.decoder.read_image_spec(&it.path).ok());
            self.layout_frames = vec![FrameInput {
                image: cur_spec,
                pinned: false,
                pan: (0.5, 0.5),
            }];
            self.selected_frame_idx = 0;
        }

        // Collect candidates
        let mut candidate_specs = Vec::new();
        let mut used_paths: HashSet<PathBuf> = self
            .layout_frames
            .iter()
            .filter_map(|f| f.image.as_ref())
            .map(|img| PathBuf::from(&img.id.0))
            .collect();

        // Fill candidate specs from catalog
        if let Some(cur_idx) = self.catalog.current_index {
            for step in 1..self.catalog.items.len().min(10) {
                let idx = (cur_idx + step) % self.catalog.items.len();
                let item = &self.catalog.items[idx];
                if !item.is_excluded
                    && item.failure_reason.is_none()
                    && !used_paths.contains(&item.path)
                    && let Ok(spec) = self.decoder.read_image_spec(&item.path)
                {
                    used_paths.insert(item.path.clone());
                    candidate_specs.push(spec);
                }
            }
        }

        let req = LayoutRequest {
            monitor_px: (mw, mh),
            mode: layout_mode,
            max_scale: self.config.layout.max_scale,
            border_px: self.config.layout.border_px,
            frames: self.layout_frames.clone(),
        };

        let plan = plan_layout(&req, &candidate_specs);

        // Preload/decode images needed by the plan
        for frame in &plan.frames {
            if let Some(placed) = &frame.content {
                let p = PathBuf::from(&placed.id.0);
                if !self.cached_images.contains_key(&placed.id)
                    && let Ok(decoded) = self.decoder.decode_image(&p, Some(4096))
                {
                    self.cached_images.insert(placed.id.clone(), decoded);
                }
            }
        }

        // Re-compose layout bitmap
        let composed = compose_layout(&plan, &self.cached_images);

        // Sync frames back
        self.layout_frames = plan
            .frames
            .iter()
            .map(|f| FrameInput {
                image: f.content.as_ref().map(|c| ImageSpec {
                    id: c.id.clone(),
                    size_px: self
                        .cached_images
                        .get(&c.id)
                        .map(|img| img.orig_size)
                        .unwrap_or((1, 1)),
                }),
                pinned: f.pinned,
                pan: f.pan,
            })
            .collect();

        if self.selected_frame_idx >= self.layout_frames.len() {
            self.selected_frame_idx = self.layout_frames.len().saturating_sub(1);
        }

        self.active_plan = Some(plan);
        self.active_composed = Some(composed);

        self.prefetch_adjacent_images();
        self.prune_image_cache();
    }

    pub fn prefetch_adjacent_images(&mut self) {
        if self.catalog.items.is_empty() {
            return;
        }
        let n = self.catalog.items.len();
        let cur = self.catalog.current_index.unwrap_or(0);
        let indices = [(cur + n - 1) % n, (cur + 1) % n];

        for idx in indices {
            let item = &self.catalog.items[idx];
            if item.failure_reason.is_none() {
                let id = ImageId(item.path.to_string_lossy().to_string());
                if !self.cached_images.contains_key(&id)
                    && let Ok(dec) = self.decoder.decode_image(&item.path, Some(4096))
                {
                    self.cached_images.insert(id, dec);
                }
            }
        }
    }

    pub fn prune_image_cache(&mut self) {
        let mut keep = HashSet::new();

        // 1. Current normal view image
        if let Some(item) = self.catalog.current_item() {
            keep.insert(ImageId(item.path.to_string_lossy().to_string()));
        }

        // 2. Prefetch adjacent images in catalog (±1)
        if !self.catalog.items.is_empty()
            && let Some(idx) = self.catalog.current_index
        {
            let n = self.catalog.items.len();
            let prev_idx = (idx + n - 1) % n;
            let next_idx = (idx + 1) % n;
            keep.insert(ImageId(
                self.catalog.items[prev_idx]
                    .path
                    .to_string_lossy()
                    .to_string(),
            ));
            keep.insert(ImageId(
                self.catalog.items[next_idx]
                    .path
                    .to_string_lossy()
                    .to_string(),
            ));
        }

        // 3. Composite layout frames (all active frames)
        for frame in &self.layout_frames {
            if let Some(img) = &frame.image {
                keep.insert(img.id.clone());
            }
        }
        if let Some(plan) = &self.active_plan {
            for frame in &plan.frames {
                if let Some(c) = &frame.content {
                    keep.insert(c.id.clone());
                }
            }
        }

        // Evict any cached image not in keep
        self.cached_images.retain(|id, _| keep.contains(id));
    }

    pub fn next_image(&mut self) {
        if self.view_mode == ViewMode::Normal {
            self.catalog.next();
            if !self.has_pinned_frames() {
                self.rebuild_layout(true, true);
            }
        } else {
            // Composite view: change selected frame only (Spec 3.4)
            if let Some(frame) = self.layout_frames.get(self.selected_frame_idx)
                && frame.pinned
            {
                self.set_osd("ピン留め中".to_string(), 2, false);
                return;
            }

            // Exclude images used in other frames
            let mut other_used = HashSet::new();
            for (idx, f) in self.layout_frames.iter().enumerate() {
                if idx != self.selected_frame_idx
                    && let Some(img) = &f.image
                {
                    other_used.insert(PathBuf::from(&img.id.0));
                }
            }

            // Start search from current frame's image index
            let current_path = self
                .layout_frames
                .get(self.selected_frame_idx)
                .and_then(|f| f.image.as_ref())
                .map(|img| PathBuf::from(&img.id.0));

            let start_idx = current_path
                .as_ref()
                .and_then(|p| self.catalog.items.iter().position(|it| &it.path == p))
                .or(self.catalog.current_index)
                .unwrap_or(0);

            if let Some((_, next_path)) =
                self.catalog
                    .find_next_valid_image(start_idx, true, &other_used)
                && current_path.as_ref() != Some(&next_path)
                && let Ok(spec) = self.decoder.read_image_spec(&next_path)
            {
                if self.selected_frame_idx < self.layout_frames.len() {
                    self.layout_frames[self.selected_frame_idx].image = Some(spec);
                    self.layout_frames[self.selected_frame_idx].pan = (0.5, 0.5);
                    if self.selected_frame_idx == 0 {
                        self.catalog.select_by_path(&next_path);
                    }
                }
                self.rebuild_layout(false, false);
            }
        }
    }

    pub fn prev_image(&mut self) {
        if self.view_mode == ViewMode::Normal {
            self.catalog.prev();
            if !self.has_pinned_frames() {
                self.rebuild_layout(true, true);
            }
        } else {
            if let Some(frame) = self.layout_frames.get(self.selected_frame_idx)
                && frame.pinned
            {
                self.set_osd("ピン留め中".to_string(), 2, false);
                return;
            }

            let mut other_used = HashSet::new();
            for (idx, f) in self.layout_frames.iter().enumerate() {
                if idx != self.selected_frame_idx
                    && let Some(img) = &f.image
                {
                    other_used.insert(PathBuf::from(&img.id.0));
                }
            }

            let current_path = self
                .layout_frames
                .get(self.selected_frame_idx)
                .and_then(|f| f.image.as_ref())
                .map(|img| PathBuf::from(&img.id.0));

            let start_idx = current_path
                .as_ref()
                .and_then(|p| self.catalog.items.iter().position(|it| &it.path == p))
                .or(self.catalog.current_index)
                .unwrap_or(0);

            if let Some((_, prev_path)) =
                self.catalog
                    .find_next_valid_image(start_idx, false, &other_used)
                && current_path.as_ref() != Some(&prev_path)
                && let Ok(spec) = self.decoder.read_image_spec(&prev_path)
            {
                if self.selected_frame_idx < self.layout_frames.len() {
                    self.layout_frames[self.selected_frame_idx].image = Some(spec);
                    self.layout_frames[self.selected_frame_idx].pan = (0.5, 0.5);
                    if self.selected_frame_idx == 0 {
                        self.catalog.select_by_path(&prev_path);
                    }
                }
                self.rebuild_layout(false, false);
            }
        }
    }

    pub fn first_image(&mut self) {
        if self.view_mode == ViewMode::Normal {
            self.catalog.first();
            if !self.has_pinned_frames() {
                self.rebuild_layout(true, true);
            }
        } else {
            if let Some(frame) = self.layout_frames.get(self.selected_frame_idx)
                && frame.pinned
            {
                self.set_osd("ピン留め中".to_string(), 2, false);
                return;
            }
            let mut other_used = HashSet::new();
            for (idx, f) in self.layout_frames.iter().enumerate() {
                if idx != self.selected_frame_idx
                    && let Some(img) = &f.image
                {
                    other_used.insert(PathBuf::from(&img.id.0));
                }
            }
            if let Some(item) = self
                .catalog
                .items
                .iter()
                .find(|it| !other_used.contains(&it.path) && it.failure_reason.is_none())
            {
                let first_path = item.path.clone();
                let current_path = self
                    .layout_frames
                    .get(self.selected_frame_idx)
                    .and_then(|f| f.image.as_ref())
                    .map(|img| PathBuf::from(&img.id.0));
                if current_path.as_ref() != Some(&first_path)
                    && let Ok(spec) = self.decoder.read_image_spec(&first_path)
                {
                    if self.selected_frame_idx < self.layout_frames.len() {
                        self.layout_frames[self.selected_frame_idx].image = Some(spec);
                        self.layout_frames[self.selected_frame_idx].pan = (0.5, 0.5);
                        if self.selected_frame_idx == 0 {
                            self.catalog.select_by_path(&first_path);
                        }
                    }
                    self.rebuild_layout(false, false);
                }
            }
        }
    }

    pub fn last_image(&mut self) {
        if self.view_mode == ViewMode::Normal {
            self.catalog.last();
            if !self.has_pinned_frames() {
                self.rebuild_layout(true, true);
            }
        } else {
            if let Some(frame) = self.layout_frames.get(self.selected_frame_idx)
                && frame.pinned
            {
                self.set_osd("ピン留め中".to_string(), 2, false);
                return;
            }
            let mut other_used = HashSet::new();
            for (idx, f) in self.layout_frames.iter().enumerate() {
                if idx != self.selected_frame_idx
                    && let Some(img) = &f.image
                {
                    other_used.insert(PathBuf::from(&img.id.0));
                }
            }
            if let Some(item) = self
                .catalog
                .items
                .iter()
                .rev()
                .find(|it| !other_used.contains(&it.path) && it.failure_reason.is_none())
            {
                let last_path = item.path.clone();
                let current_path = self
                    .layout_frames
                    .get(self.selected_frame_idx)
                    .and_then(|f| f.image.as_ref())
                    .map(|img| PathBuf::from(&img.id.0));
                if current_path.as_ref() != Some(&last_path)
                    && let Ok(spec) = self.decoder.read_image_spec(&last_path)
                {
                    if self.selected_frame_idx < self.layout_frames.len() {
                        self.layout_frames[self.selected_frame_idx].image = Some(spec);
                        self.layout_frames[self.selected_frame_idx].pan = (0.5, 0.5);
                        if self.selected_frame_idx == 0 {
                            self.catalog.select_by_path(&last_path);
                        }
                    }
                    self.rebuild_layout(false, false);
                }
            }
        }
    }

    pub fn switch_view_mode(&mut self) {
        if self.view_mode == ViewMode::Normal {
            self.view_mode = ViewMode::Composite;
            self.set_osd("合成表示".into(), 2, false);
            self.rebuild_layout(false, false);
        } else {
            self.view_mode = ViewMode::Normal;
            if let Some(frame) = self.layout_frames.get(self.selected_frame_idx)
                && let Some(img) = &frame.image
            {
                self.catalog.select_by_path(Path::new(&img.id.0));
            }
            self.set_osd("通常表示".into(), 2, false);
        }
    }

    pub fn toggle_pin(&mut self) {
        if self.selected_frame_idx < self.layout_frames.len() {
            let is_pinned = !self.layout_frames[self.selected_frame_idx].pinned;
            self.layout_frames[self.selected_frame_idx].pinned = is_pinned;
            self.set_osd(
                if is_pinned {
                    "ピン留めしました".into()
                } else {
                    "ピン留めを解除しました".into()
                },
                2,
                false,
            );
            self.rebuild_layout(false, false);
        }
    }

    pub fn swap_frames(&mut self) {
        let len = self.layout_frames.len();
        if len <= 1 {
            return;
        }
        let next_idx = (self.selected_frame_idx + 1) % len;
        self.layout_frames.swap(self.selected_frame_idx, next_idx);
        self.selected_frame_idx = next_idx;
        self.set_osd("枠を交換しました".into(), 2, false);
        self.rebuild_layout(false, false);
    }

    pub fn reset_pan(&mut self) {
        if self.selected_frame_idx < self.layout_frames.len() {
            self.layout_frames[self.selected_frame_idx].pan = (0.5, 0.5);
            self.set_osd("パン位置を中央にリセットしました".into(), 2, false);
            self.rebuild_layout(false, false);
        }
    }

    pub fn toggle_exclude_current(&mut self) {
        let path = if self.view_mode == ViewMode::Composite {
            self.layout_frames
                .get(self.selected_frame_idx)
                .and_then(|f| f.image.as_ref())
                .map(|img| PathBuf::from(&img.id.0))
        } else {
            self.catalog.current_item().map(|it| it.path.clone())
        };

        if let Some(p) = path {
            let is_now_excluded = self.catalog.toggle_exclude(&p);
            self.sync_catalog_excludes();

            self.set_osd(
                if is_now_excluded {
                    "画像を除外しました (Delで解除)".into()
                } else {
                    "画像の除外を解除しました".into()
                },
                3,
                false,
            );
            self.rebuild_layout(false, false);
        }
    }

    pub fn sync_catalog_excludes(&mut self) {
        self.config.library.excluded = self
            .catalog
            .excluded_paths
            .iter()
            .map(|p| p.to_string_lossy().to_string())
            .collect();
        self.mark_config_dirty();
    }

    pub fn cycle_layout_mode(&mut self, forward: bool) {
        let prev_pinned = self.layout_frames.iter().filter(|f| f.pinned).count();

        let modes = ["auto", "single", "split2", "split3"];
        let cur = self.config.layout.mode.as_str();
        let idx = modes.iter().position(|&m| m == cur).unwrap_or(0);
        let next_idx = if forward {
            (idx + 1) % modes.len()
        } else {
            (idx + modes.len() - 1) % modes.len()
        };
        self.config.layout.mode = modes[next_idx].to_string();
        self.mark_config_dirty();
        let mode_name = match self.config.layout.mode.as_str() {
            "single" => "1枠",
            "split2" => "2枠",
            "split3" => "3枠",
            _ => "自動",
        };

        self.rebuild_layout(false, true);

        let new_pinned = self.layout_frames.iter().filter(|f| f.pinned).count();
        if new_pinned < prev_pinned {
            self.set_osd(
                format!("分割モード: {}（ピン留め枠が外れました）", mode_name),
                3,
                false,
            );
        } else {
            self.set_osd(format!("分割モード: {}", mode_name), 2, false);
        }
    }

    pub fn adjust_max_scale(&mut self, delta: f64) {
        let new_scale = ((self.config.layout.max_scale + delta) * 20.0).round() / 20.0;
        self.config.layout.max_scale = new_scale.clamp(1.00, 2.50);
        self.mark_config_dirty();
        self.set_osd(
            format!("最大拡大率 M: {:.2}", self.config.layout.max_scale),
            2,
            false,
        );
        self.rebuild_layout(false, true);
    }

    pub fn select_monitor(&mut self, number: usize) {
        if let Some(idx) = self.monitors.iter().position(|m| m.number == number) {
            self.target_monitor_idx = idx;
            self.config.target.monitor_id = Some(self.monitors[idx].id.clone());
            self.mark_config_dirty();
            self.view_mode = ViewMode::Composite;
            self.set_osd(
                format!("モニター #{}: {}", number, self.monitors[idx].name),
                2,
                false,
            );
            self.rebuild_layout(false, true);
            show_monitor_overlays(&self.monitors);
        } else {
            self.set_osd(format!("未接続のモニター番号です: {}", number), 2, false);
        }
    }

    pub fn apply_current_wallpaper(&mut self) {
        self.view_mode = ViewMode::Composite;

        // Re-enumerate monitors to verify target monitor is still connected (Spec 5.2)
        self.monitors = enumerate_monitors();
        let Some(monitor) = self.target_monitor().cloned() else {
            self.set_osd(
                "対象モニターが切断されているため適用を中止しました".into(),
                4,
                false,
            );
            return;
        };

        // Check lock
        let _guard = match self.storage.lock_exclusive_timeout(Duration::from_secs(5)) {
            Some(g) => g,
            None => {
                self.set_osd(
                    "定期更新が実行中のため適用できませんでした".into(),
                    4,
                    false,
                );
                return;
            }
        };

        // Re-compose to make sure PNG is up-to-date
        self.rebuild_layout(false, false);
        let Some(composed) = &self.active_composed else {
            self.set_osd("合成画像の生成に失敗しました".into(), 4, false);
            return;
        };

        let now_utc_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis();

        let mut hasher = DefaultHasher::new();
        monitor.id.hash(&mut hasher);
        let mon_hash = hasher.finish() as u32;
        let seq = SEQ_COUNTER.fetch_add(1, Ordering::Relaxed);
        let filename = format!("{:08x}_{}_{:04}.png", mon_hash, now_utc_ms, seq);
        let output_path = self.storage.wallpapers_dir.join(filename);

        if let Err(e) = save_png_wic(composed, &output_path) {
            self.set_osd(format!("PNG保存に失敗しました: {}", e), 4, false);
            return;
        }

        if self.wallpaper_mgr.is_none() {
            self.wallpaper_mgr = WallpaperManager::new().ok();
        }

        let mut mgr = match self.wallpaper_mgr.take() {
            Some(m) => m,
            None => {
                self.set_osd("壁紙APIの初期化に失敗しました".into(), 4, false);
                return;
            }
        };

        // Check if multiple monitors and single apply with Span/Tile/Center (Spec 5.3)
        if self.monitors.len() > 1 && mgr.needs_fill_change() {
            unsafe {
                let text = w!(
                    "他モニターの既存壁紙の配置方式が「画面に合わせて拡大 (Fill)」に変更されます。適用しますか？"
                );
                let title = w!("壁紙配置方式の変更確認");
                let res = MessageBoxW(Some(self.hwnd), text, title, MB_YESNO | MB_ICONQUESTION);
                if res != IDYES {
                    self.wallpaper_mgr = Some(mgr);
                    return;
                }
            }
        }

        mgr.capture_snapshot(std::slice::from_ref(&monitor.id));
        let apply_result = mgr.apply_wallpaper(&monitor.id, &output_path, true);

        match apply_result {
            Ok(_) => {
                let mut notice = format!("壁紙を適用しました: {}", monitor.name);
                let has_empty = self
                    .active_plan
                    .as_ref()
                    .is_some_and(|p| p.frames.iter().any(|f| f.content.is_none()));
                if has_empty {
                    notice.push_str("（空き枠あり）");
                }
                self.set_osd(notice, 6, true);

                // Track in recent applied wallpapers
                let entry = self
                    .recent_wallpapers
                    .entry(monitor.id.clone())
                    .or_default();
                entry.push(output_path.clone());
                if entry.len() > 2 {
                    entry.remove(0);
                }

                // Cleanup wallpapers with complete protection
                let mut protected = HashSet::new();
                protected.insert(normalize_path(&output_path));
                for paths in self.recent_wallpapers.values() {
                    for p in paths {
                        protected.insert(normalize_path(p));
                    }
                }
                if let Some(snap) = mgr.last_undo_snapshot() {
                    for item in &snap.monitor_wallpapers {
                        protected.insert(normalize_path(&item.wallpaper_path));
                    }
                }
                for dev_id in mgr.get_wallpaper_monitor_ids() {
                    if let Ok(p) = mgr.get_current_wallpaper(&dev_id) {
                        protected.insert(normalize_path(&p));
                    }
                }
                self.storage.cleanup_wallpapers(&protected);
            }
            Err(e) => {
                self.set_osd(format!("壁紙の適用に失敗しました: {}", e), 4, false);
            }
        }
        self.wallpaper_mgr = Some(mgr);
    }

    pub fn undo_wallpaper(&mut self) {
        if let Some(mgr) = self.wallpaper_mgr.as_mut() {
            match mgr.undo() {
                Ok(restored) => {
                    self.set_osd(
                        format!("{}台のモニターの壁紙を元に戻しました", restored.len()),
                        4,
                        false,
                    );
                }
                Err(e) => {
                    self.set_osd(format!("元に戻せませんでした: {}", e), 4, false);
                }
            }
        }
    }

    pub fn export_png_dialog(&mut self) {
        self.view_mode = ViewMode::Composite;
        self.rebuild_layout(false, false);
        let Some(composed) = &self.active_composed else {
            return;
        };

        unsafe {
            let Ok(save_dialog) =
                CoCreateInstance::<_, IFileSaveDialog>(&FileSaveDialog, None, CLSCTX_ALL)
            else {
                return;
            };

            let default_name = format!("wallpaper_{}x{}.png", composed.width, composed.height);
            let wide_name: Vec<u16> = default_name
                .encode_utf16()
                .chain(std::iter::once(0))
                .collect();
            let _ = save_dialog.SetFileName(PCWSTR(wide_name.as_ptr()));
            let _ = save_dialog.SetDefaultExtension(w!("png"));

            let options = save_dialog.GetOptions().unwrap_or_default();
            let _ = save_dialog.SetOptions(options | FOS_FORCEFILESYSTEM | FOS_OVERWRITEPROMPT);

            if save_dialog.Show(Some(self.hwnd)).is_ok()
                && let Ok(item) = save_dialog.GetResult()
                && let Ok(path_str) = item.GetDisplayName(SIGDN_FILESYSPATH)
            {
                let path = PathBuf::from(path_str.to_string().unwrap_or_default());
                CoTaskMemFree(Some(path_str.0 as *const _ as *mut _));
                if let Err(e) = save_png_wic(composed, &path) {
                    self.set_osd(format!("PNGの書き出しに失敗しました: {}", e), 4, false);
                } else {
                    let mut notice = format!("PNGを保存しました: {}", path.display());
                    if self
                        .active_plan
                        .as_ref()
                        .is_some_and(|p| p.frames.iter().any(|f| f.content.is_none()))
                    {
                        notice.push_str("（空き枠あり）");
                    }
                    self.set_osd(notice, 4, false);
                }
            }
        }
    }

    pub fn open_folder_dialog(&mut self) {
        unsafe {
            let Ok(dialog) =
                CoCreateInstance::<_, IFileOpenDialog>(&FileOpenDialog, None, CLSCTX_ALL)
            else {
                return;
            };

            let options = dialog.GetOptions().unwrap_or_default();
            let _ = dialog.SetOptions(options | FOS_PICKFOLDERS | FOS_FORCEFILESYSTEM);

            if let Some(folder) = &self.catalog.folder {
                let wide_folder: Vec<u16> = folder
                    .as_os_str()
                    .encode_wide()
                    .chain(std::iter::once(0))
                    .collect();
                if let Ok(item) = SHCreateItemFromParsingName::<_, _, IShellItem>(
                    PCWSTR(wide_folder.as_ptr()),
                    None,
                ) {
                    let _ = dialog.SetFolder(&item);
                }
            }

            if dialog.Show(Some(self.hwnd)).is_ok()
                && let Ok(item) = dialog.GetResult()
                && let Ok(path_str) = item.GetDisplayName(SIGDN_FILESYSPATH)
            {
                let path = PathBuf::from(path_str.to_string().unwrap_or_default());
                CoTaskMemFree(Some(path_str.0 as *const _ as *mut _));
                self.open_path(&path);
            }
        }
    }

    pub fn toggle_fullscreen(&mut self) {
        unsafe {
            if self.fullscreen {
                // Restore window
                let _ = SetWindowLongW(
                    self.hwnd,
                    GWL_STYLE,
                    (WS_OVERLAPPEDWINDOW | WS_VISIBLE).0 as i32,
                );
                if self.config.window.maximized {
                    let _ = ShowWindow(self.hwnd, SW_MAXIMIZE);
                } else {
                    let _ = SetWindowPos(
                        self.hwnd,
                        None,
                        self.normal_window_rect.left,
                        self.normal_window_rect.top,
                        self.normal_window_rect.right - self.normal_window_rect.left,
                        self.normal_window_rect.bottom - self.normal_window_rect.top,
                        SWP_NOZORDER | SWP_FRAMECHANGED,
                    );
                    let _ = ShowWindow(self.hwnd, SW_SHOWNORMAL);
                }
                self.fullscreen = false;
                self.bar_visible = true;
            } else {
                let mut wp = WINDOWPLACEMENT {
                    length: std::mem::size_of::<WINDOWPLACEMENT>() as u32,
                    ..Default::default()
                };
                if GetWindowPlacement(self.hwnd, &mut wp).is_ok() {
                    if wp.rcNormalPosition.right > wp.rcNormalPosition.left {
                        self.normal_window_rect = wp.rcNormalPosition;
                    }
                    if IsWindowVisible(self.hwnd).as_bool() {
                        self.config.window.maximized = wp.showCmd == SW_SHOWMAXIMIZED.0 as u32;
                    }
                } else {
                    let _ = GetWindowRect(self.hwnd, &mut self.normal_window_rect);
                }
                let _ = SetWindowLongW(self.hwnd, GWL_STYLE, (WS_POPUP | WS_VISIBLE).0 as i32);
                let monitor = MonitorFromWindow(self.hwnd, MONITOR_DEFAULTTONEAREST);
                let mut mi = MONITORINFO {
                    cbSize: std::mem::size_of::<MONITORINFO>() as u32,
                    ..Default::default()
                };
                let _ = GetMonitorInfoW(monitor, &mut mi);

                let _ = SetWindowPos(
                    self.hwnd,
                    Some(HWND_TOP),
                    mi.rcMonitor.left,
                    mi.rcMonitor.top,
                    mi.rcMonitor.right - mi.rcMonitor.left,
                    mi.rcMonitor.bottom - mi.rcMonitor.top,
                    SWP_FRAMECHANGED,
                );
                let _ = ShowWindow(self.hwnd, SW_SHOW);
                self.fullscreen = true;
                self.bar_visible = false;
            }
            self.config.window.fullscreen = self.fullscreen;
            self.mark_config_dirty();
        }
    }

    pub fn enter_fullscreen_startup(&mut self) {
        unsafe {
            let _ = SetWindowLongW(self.hwnd, GWL_STYLE, (WS_POPUP | WS_VISIBLE).0 as i32);
            let monitor = MonitorFromWindow(self.hwnd, MONITOR_DEFAULTTONEAREST);
            let mut mi = MONITORINFO {
                cbSize: std::mem::size_of::<MONITORINFO>() as u32,
                ..Default::default()
            };
            let _ = GetMonitorInfoW(monitor, &mut mi);

            let _ = SetWindowPos(
                self.hwnd,
                Some(HWND_TOP),
                mi.rcMonitor.left,
                mi.rcMonitor.top,
                mi.rcMonitor.right - mi.rcMonitor.left,
                mi.rcMonitor.bottom - mi.rcMonitor.top,
                SWP_FRAMECHANGED,
            );
            let _ = ShowWindow(self.hwnd, SW_SHOW);
            self.fullscreen = true;
            self.bar_visible = false;
        }
    }
}

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

        // Register main window class
        let wc = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            style: CS_HREDRAW | CS_VREDRAW | CS_DBLCLKS,
            lpfnWndProc: Some(wnd_proc),
            hInstance: instance.into(),
            hCursor: LoadCursorW(None, IDC_ARROW)?,
            hbrBackground: HBRUSH(GetStockObject(BLACK_BRUSH).0),
            lpszClassName: PCWSTR(wide_class.as_ptr()),
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
                    && state.view_mode == ViewMode::Composite
                    && state.modal_dialog == ModalDialog::None
                {
                    let mut pt = POINT::default();
                    let _ = GetCursorPos(&mut pt);
                    let _ = ScreenToClient(hwnd, &mut pt);

                    if is_over_pan_capable_frame(state, hwnd, pt.x, pt.y) {
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
                    if let Some(frame) = state.layout_frames.get_mut(state.selected_frame_idx) {
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
                            let bar_h = if state.bar_visible { 45 } else { 0 };
                            let canvas_h = (cr.bottom - bar_h).max(1);
                            let canvas_w = cr.right.max(1);

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

                        frame.pan = (pan_x, pan_y);
                        state.rebuild_layout(false, false);
                        let _ = InvalidateRect(Some(hwnd), None, false);
                    }
                }

                LRESULT(0)
            }

            WM_LBUTTONDOWN => {
                state.last_interaction = Instant::now();
                let x = (lparam.0 & 0xffff) as i16 as i32;
                let y = ((lparam.0 >> 16) & 0xffff) as i16 as i32;

                // Handle modal dialog clicks
                if state.modal_dialog != ModalDialog::None {
                    handle_modal_click(state, hwnd, x, y);
                    let _ = InvalidateRect(Some(hwnd), None, false);
                    return LRESULT(0);
                }

                // Handle operation bar clicks
                if state.bar_visible && y < 45 {
                    handle_bar_click(state, x, y);
                    let _ = InvalidateRect(Some(hwnd), None, false);
                    return LRESULT(0);
                }

                // Handle OSD Undo button click (dynamically calculated)
                if let Some((msg_str, _, _, true)) = &state.osd {
                    let mut cr = RECT::default();
                    let _ = GetClientRect(hwnd, &mut cr);
                    let btn_r = osd_undo_button_rect(msg_str, cr.right);
                    if x >= btn_r.left && x <= btn_r.right && y >= btn_r.top && y <= btn_r.bottom {
                        state.undo_wallpaper();
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
                    let bar_h = if state.bar_visible { 45 } else { 0 };
                    let btn_r = empty_state_folder_btn_rect(cr.right, cr.bottom, bar_h);
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
                    let bar_h = if state.bar_visible { 45 } else { 0 };
                    let canvas_h = (cr.bottom - bar_h).max(1);
                    let canvas_w = cr.right.max(1);

                    let (mw, mh) = plan.monitor_px;
                    let fit_scale = (canvas_w as f64 / mw as f64).min(canvas_h as f64 / mh as f64);
                    let dw = mw as f64 * fit_scale;
                    let dh = mh as f64 * fit_scale;
                    let ox = (canvas_w as f64 - dw) / 2.0;
                    let oy = bar_h as f64 + (canvas_h as f64 - dh) / 2.0;

                    // Check frame overlay buttons on selected frame
                    if let Some(sel_frame) = plan.frames.get(state.selected_frame_idx) {
                        let fx = (ox + sel_frame.slot.x as f64 * fit_scale).round() as i32;
                        let fy = (oy + sel_frame.slot.y as f64 * fit_scale).round() as i32;
                        let fh = (sel_frame.slot.height as f64 * fit_scale).round() as i32;
                        let btn_y = fy + fh - 34;

                        if y >= btn_y && y <= btn_y + 26 {
                            if x >= fx + 8 && x <= fx + 63 {
                                state.toggle_pin();
                                let _ = InvalidateRect(Some(hwnd), None, false);
                                return LRESULT(0);
                            } else if x >= fx + 68 && x <= fx + 133 {
                                state.swap_frames();
                                let _ = InvalidateRect(Some(hwnd), None, false);
                                return LRESULT(0);
                            } else if x >= fx + 138 && x <= fx + 193 {
                                state.toggle_exclude_current();
                                let _ = InvalidateRect(Some(hwnd), None, false);
                                return LRESULT(0);
                            }
                        }
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

                if ctrl_down {
                    let d = if delta > 0 { 0.05 } else { -0.05 };
                    state.adjust_max_scale(d);
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
                        if shift {
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
                        if shift {
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
                            state.set_osd("合成表示でのみ有効です (Vキーで切替)".into(), 2, false);
                        }
                    }
                    VK_V => state.switch_view_mode(),
                    VK_L => state.cycle_layout_mode(!shift),
                    VK_OEM_PLUS | VK_ADD => state.adjust_max_scale(0.05),
                    VK_OEM_MINUS | VK_SUBTRACT => state.adjust_max_scale(-0.05),
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
                        let storage_ref = &state.storage;
                        let cfg_ref = &state.config;
                        let mut mgr_ref = state.wallpaper_mgr.take();
                        match execute_random_update(storage_ref, cfg_ref, false, &mut mgr_ref) {
                            Ok(succ) => {
                                state.wallpaper_mgr = mgr_ref;
                                state.set_osd(
                                    format!(
                                        "全{}台の壁紙をランダム更新しました",
                                        succ.applied_monitors.len()
                                    ),
                                    6,
                                    true,
                                );
                                state.rebuild_layout(false, false);
                            }
                            Err(e) => {
                                state.wallpaper_mgr = mgr_ref;
                                state.set_osd(format!("ランダム更新失敗: {:?}", e), 4, false);
                            }
                        }
                    }
                    VK_Z if ctrl => state.undo_wallpaper(),
                    VK_S => state.export_png_dialog(),
                    VK_DELETE => state.toggle_exclude_current(),
                    VK_O if ctrl => state.open_folder_dialog(),
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

            WM_DISPLAYCHANGE | WM_DPICHANGED => {
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
                    state.active_composed = None;
                    state.prune_image_cache();
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

    if let Some(frame) = state.layout_frames.get_mut(state.selected_frame_idx) {
        let mut changed = false;
        match key {
            VK_LEFT if can_pan.0 => {
                frame.pan.0 = (frame.pan.0 - 0.05).clamp(0.0, 1.0);
                changed = true;
            }
            VK_RIGHT if can_pan.0 => {
                frame.pan.0 = (frame.pan.0 + 0.05).clamp(0.0, 1.0);
                changed = true;
            }
            VK_UP if can_pan.1 => {
                frame.pan.1 = (frame.pan.1 - 0.05).clamp(0.0, 1.0);
                changed = true;
            }
            VK_DOWN if can_pan.1 => {
                frame.pan.1 = (frame.pan.1 + 0.05).clamp(0.0, 1.0);
                changed = true;
            }
            _ => {}
        }
        if changed {
            state.rebuild_layout(false, false);
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
        let bar_h = if state.bar_visible { 45 } else { 0 };
        let canvas_h = (cr.bottom - bar_h).max(1);
        let canvas_w = cr.right.max(1);

        let (mw, mh) = plan.monitor_px;
        let fit_scale = (canvas_w as f64 / mw as f64).min(canvas_h as f64 / mh as f64);
        let dw = mw as f64 * fit_scale;
        let dh = mh as f64 * fit_scale;
        let ox = (canvas_w as f64 - dw) / 2.0;
        let oy = bar_h as f64 + (canvas_h as f64 - dh) / 2.0;

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

fn handle_bar_click(state: &mut AppState, x: i32, _y: i32) {
    match x {
        10..=90 => state.open_folder_dialog(),
        95..=160 => state.switch_view_mode(),
        165..=240 => {
            let next_num = if let Some(m) = state.target_monitor() {
                (m.number % state.monitors.len()) + 1
            } else {
                1
            };
            state.select_monitor(next_num);
        }
        245..=315 => state.cycle_layout_mode(true),
        320..=385 => state.adjust_max_scale(0.05),
        390..=450 => state.apply_current_wallpaper(),
        455..=520 => {
            let storage_ref = &state.storage;
            let cfg_ref = &state.config;
            let mut mgr_ref = state.wallpaper_mgr.take();
            let _ = execute_random_update(storage_ref, cfg_ref, false, &mut mgr_ref);
            state.wallpaper_mgr = mgr_ref;
            state.rebuild_layout(false, false);
        }
        525..=595 => state.undo_wallpaper(),
        600..=670 => state.export_png_dialog(),
        675..=730 => {
            state.modal_dialog = ModalDialog::Settings;
        }
        _ => {}
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
    let text_w = osd_text_display_width(text);
    let osd_w = (text_w + if has_undo { 160 } else { 50 }).max(if has_undo { 360 } else { 200 });
    let osd_h = 40;
    let ox = (win_w - osd_w) / 2;
    let oy = 60;
    RECT {
        left: ox,
        top: oy,
        right: ox + osd_w,
        bottom: oy + osd_h,
    }
}

pub fn osd_undo_button_rect(text: &str, win_w: i32) -> RECT {
    let r = osd_notification_rect(text, win_w, true);
    RECT {
        left: r.right - 110,
        top: r.top + 6,
        right: r.right - 110 + 95,
        bottom: r.top + 6 + 28,
    }
}

pub fn empty_state_folder_btn_rect(win_w: i32, win_h: i32, bar_h: i32) -> RECT {
    let btn_w = 220;
    let btn_h = 36;
    let bx = (win_w - btn_w) / 2;
    let by = bar_h + (win_h - bar_h) / 2 + 25;
    RECT {
        left: bx,
        top: by,
        right: bx + btn_w,
        bottom: by + btn_h,
    }
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
            w!("Segoe UI"),
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
                w!("Meiryo UI"),
            );
        }
        if font.0.is_null() {
            font = HFONT(GetStockObject(DEFAULT_GUI_FONT).0);
        }
        font
    }
}

unsafe fn draw_empty_state_guidance(hdc: HDC, win_w: i32, win_h: i32, bar_h: i32, msg: &str) {
    unsafe {
        let btn_r = empty_state_folder_btn_rect(win_w, win_h, bar_h);
        let mut tr = RECT {
            left: 20,
            top: bar_h + (win_h - bar_h) / 2 - 50,
            right: win_w - 20,
            bottom: btn_r.top - 10,
        };
        let _ = SetTextColor(hdc, COLORREF(0x00B0B0B0));
        let _ = SetBkMode(hdc, TRANSPARENT);
        draw_text(hdc, msg, &mut tr, DT_CENTER | DT_WORDBREAK);

        draw_bar_btn(
            hdc,
            btn_r.left,
            btn_r.top,
            btn_r.right - btn_r.left,
            btn_r.bottom - btn_r.top,
            "フォルダを選択 (Enter)",
        );
    }
}

fn handle_modal_click(state: &mut AppState, hwnd: HWND, x: i32, y: i32) {
    unsafe {
        let mut cr = RECT::default();
        let _ = GetClientRect(hwnd, &mut cr);
        let win_w = cr.right.max(1);
        let win_h = cr.bottom.max(1);

        match state.modal_dialog {
            ModalDialog::Settings => {
                let box_w = 520;
                let box_h = 430;
                let bx = (win_w - box_w) / 2;
                let by = (win_h - box_h) / 2;

                // Close button: bx + 200, by + 375, 120, 34
                if x >= bx + 200 && x <= bx + 320 && y >= by + 375 && y <= by + 409 {
                    state.modal_dialog = ModalDialog::None;
                    return;
                }

                // Row 1: Max Scale M [-] [+] (by + 60)
                if y >= by + 60 && y <= by + 88 {
                    if x >= bx + 160 && x <= bx + 200 {
                        state.adjust_max_scale(-0.05);
                    } else if x >= bx + 280 && x <= bx + 320 {
                        state.adjust_max_scale(0.05);
                    }
                }

                // Row 2: Border px [-] [+] (by + 105)
                if y >= by + 105 && y <= by + 133 {
                    if x >= bx + 160 && x <= bx + 200 {
                        state.config.layout.border_px =
                            state.config.layout.border_px.saturating_sub(1);
                        state.mark_config_dirty();
                        state.rebuild_layout(false, true);
                    } else if x >= bx + 280 && x <= bx + 320 {
                        state.config.layout.border_px = (state.config.layout.border_px + 1).min(8);
                        state.mark_config_dirty();
                        state.rebuild_layout(false, true);
                    }
                }

                // Row 3: Random mode (by + 150)
                if y >= by + 150 && y <= by + 178 {
                    if x >= bx + 160 && x <= bx + 220 {
                        state.config.random.mode = "auto".into();
                        state.mark_config_dirty();
                    } else if x >= bx + 225 && x <= bx + 275 {
                        state.config.random.mode = "single".into();
                        state.mark_config_dirty();
                    } else if x >= bx + 280 && x <= bx + 330 {
                        state.config.random.mode = "split2".into();
                        state.mark_config_dirty();
                    } else if x >= bx + 335 && x <= bx + 385 {
                        state.config.random.mode = "split3".into();
                        state.mark_config_dirty();
                    }
                }

                // Row 4: Schedule enabled (by + 195)
                if y >= by + 195 && y <= by + 223 && x >= bx + 160 && x <= bx + 260 {
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

                // Row 5: Schedule logon (by + 240)
                if y >= by + 240 && y <= by + 268 && x >= bx + 160 && x <= bx + 260 {
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

                // Row 6: Schedule interval cycle (by + 285)
                if y >= by + 285 && y <= by + 313 && x >= bx + 160 && x <= bx + 320 {
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
                let box_w = 540;
                let box_h = 440;
                let bx = (win_w - box_w) / 2;
                let by = (win_h - box_h) / 2;

                // Close button: bx + 210, by + 380, 120, 34
                if x >= bx + 210 && x <= bx + 330 && y >= by + 380 && y <= by + 414 {
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

                for (i, p) in excluded_in_folder.iter().take(5).enumerate() {
                    let btn_y = by + 60 + (i as i32) * 36;
                    if y >= btn_y && y <= btn_y + 26 && x >= bx + 440 && x <= bx + 500 {
                        state.catalog.toggle_exclude(p);
                        state.sync_catalog_excludes();
                        state.rebuild_layout(false, false);
                        return;
                    }
                }

                // Clear current folder excludes: bx + 30, by + 250, 220, 30
                if y >= by + 250 && y <= by + 280 && x >= bx + 30 && x <= bx + 250 {
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

                // Clean nonexistent paths: bx + 30, by + 325, 200, 30
                if y >= by + 325 && y <= by + 355 && x >= bx + 30 && x <= bx + 230 {
                    state.catalog.excluded_paths.retain(|p| p.exists());
                    state.sync_catalog_excludes();
                    state.set_osd("存在しないパスの除外設定を整理しました".into(), 2, false);
                }

                // Clear other folder excludes: bx + 245, by + 325, 200, 30
                if y >= by + 325
                    && y <= by + 355
                    && x >= bx + 245
                    && x <= bx + 445
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
                let box_w = 480;
                let box_h = 520;
                let bx = (win_w - box_w) / 2;
                let by = (win_h - box_h) / 2;

                if x >= bx + 180 && x <= bx + 300 && y >= by + 460 && y <= by + 496 {
                    state.modal_dialog = ModalDialog::None;
                }
            }
            ModalDialog::None => {}
        }
    }
}

fn paint_window(state: &mut AppState, hwnd: HWND, hdc: HDC) {
    unsafe {
        let mut cr = RECT::default();
        let _ = GetClientRect(hwnd, &mut cr);
        let win_w = cr.right.max(1);
        let win_h = cr.bottom.max(1);

        // Double-buffered memory DC
        let mem_dc = CreateCompatibleDC(Some(hdc));
        let mem_bmp = CreateCompatibleBitmap(hdc, win_w, win_h);
        let old_bmp = SelectObject(mem_dc, mem_bmp.into());

        // Modern system GUI font (Segoe UI with Meiryo UI fallback)
        let font = create_gui_font(hwnd, 10, 400);
        let old_font = SelectObject(mem_dc, font.into());

        // Fill background
        let bg_brush = CreateSolidBrush(COLORREF(0x00121212));
        let _ = FillRect(mem_dc, &cr, bg_brush);
        let _ = DeleteObject(bg_brush.into());

        let bar_h = if state.bar_visible { 45 } else { 0 };
        let canvas_h = (win_h - bar_h).max(1);
        let canvas_w = win_w;

        // Render Canvas
        if state.view_mode == ViewMode::Composite {
            if let Some(composed) = &state.active_composed {
                let (mw, mh) = (composed.width as f64, composed.height as f64);
                let fit_scale = (canvas_w as f64 / mw).min(canvas_h as f64 / mh);
                let dw = (mw * fit_scale).round() as i32;
                let dh = (mh * fit_scale).round() as i32;
                let ox = (canvas_w - dw) / 2;
                let oy = bar_h + (canvas_h - dh) / 2;

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

                            // Spec 4.1: Frame action buttons (ピン, 枠交換, 除外)
                            let btn_y = fy + fh - 34;
                            let pin_txt = if frame.pinned { "解除" } else { "ピン" };
                            draw_overlay_btn(mem_dc, fx + 8, btn_y, 55, 26, pin_txt);
                            draw_overlay_btn(mem_dc, fx + 68, btn_y, 65, 26, "枠交換");
                            let ex_txt =
                                if let Some(c) = &frame.content {
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
                            draw_overlay_btn(mem_dc, fx + 138, btn_y, 55, 26, ex_txt);
                        }

                        let mut badge_y = fy + 8;
                        if frame.pinned {
                            draw_badge(mem_dc, fx + 8, badge_y, "ピン留め", COLORREF(0x000088FF));
                            badge_y += 24;
                        }
                        if let Some(c) = &frame.content {
                            match c.scale_limited {
                                ScaleLimit::MaxScale => {
                                    draw_badge(
                                        mem_dc,
                                        fx + 8,
                                        badge_y,
                                        "倍率上限",
                                        COLORREF(0x0000A5FF),
                                    );
                                    badge_y += 24;
                                }
                                ScaleLimit::CropLimit => {
                                    draw_badge(
                                        mem_dc,
                                        fx + 8,
                                        badge_y,
                                        "切抜上限",
                                        COLORREF(0x007820D9),
                                    );
                                    badge_y += 24;
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
                                draw_badge(mem_dc, fx + 8, badge_y, "除外中", COLORREF(0x003333FF));
                            }
                        } else {
                            draw_badge(mem_dc, fx + 8, badge_y, "空き枠", COLORREF(0x00666666));
                        }
                    }
                }
            } else {
                let msg = if state.catalog.items.is_empty() {
                    "画像フォルダを開いてください（Ctrl+O）\nまたは画像をここにドロップ"
                } else {
                    "有効な候補画像がありません\n除外を解除するか（Delキー）、別のフォルダを選択してください"
                };
                draw_empty_state_guidance(mem_dc, win_w, win_h, bar_h, msg);
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
                state.prefetch_adjacent_images();
                state.prune_image_cache();

                if let Some(dec) = state.cached_images.get(&img_id) {
                    let fit_scale = (canvas_w as f64 / dec.width as f64)
                        .min(canvas_h as f64 / dec.height as f64)
                        .min(1.0);
                    let dw = (dec.width as f64 * fit_scale).round() as i32;
                    let dh = (dec.height as f64 * fit_scale).round() as i32;
                    let ox = (canvas_w - dw) / 2;
                    let oy = bar_h + (canvas_h - dh) / 2;

                    let mut bi = BITMAPINFO::default();
                    bi.bmiHeader.biSize = std::mem::size_of::<BITMAPINFOHEADER>() as u32;
                    bi.bmiHeader.biWidth = dec.width as i32;
                    bi.bmiHeader.biHeight = -(dec.height as i32);
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
                        dec.width as i32,
                        dec.height as i32,
                        Some(dec.pixels.as_ptr() as *const _),
                        &bi,
                        DIB_RGB_COLORS,
                        SRCCOPY,
                    );
                }
            } else {
                let msg = if state.catalog.items.is_empty() {
                    "画像フォルダを開いてください（Ctrl+O）\nまたは画像をここにドロップ"
                } else {
                    "有効な画像がありません\n別のフォルダを選択してください"
                };
                draw_empty_state_guidance(mem_dc, win_w, win_h, bar_h, msg);
            }
        }

        // Render Operation Bar
        if state.bar_visible {
            draw_operation_bar(state, mem_dc, win_w);
        }

        // Render Info Display
        if state.show_info {
            draw_info_overlay(state, mem_dc, win_h);
        }

        // Render OSD
        if let Some((msg_str, _, _, has_undo)) = &state.osd {
            draw_osd_notification(mem_dc, msg_str, win_w, *has_undo);
        }

        // Render Error Banner
        if !state.error_banner.is_empty() {
            draw_error_banner(mem_dc, &state.error_banner, win_w);
        }

        // Render Modal Overlays
        match state.modal_dialog {
            ModalDialog::Settings => draw_settings_modal(mem_dc, state, win_w, win_h),
            ModalDialog::ExcludeList => draw_exclude_list_modal(mem_dc, state, win_w, win_h),
            ModalDialog::Shortcuts => draw_shortcuts_modal(mem_dc, win_w, win_h),
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

fn draw_operation_bar(state: &AppState, hdc: HDC, width: i32) {
    unsafe {
        let bar_rect = RECT {
            left: 0,
            top: 0,
            right: width,
            bottom: 44,
        };
        let brush = CreateSolidBrush(COLORREF(0x00242424));
        let _ = FillRect(hdc, &bar_rect, brush);
        let _ = DeleteObject(brush.into());

        let line_brush = CreateSolidBrush(COLORREF(0x00333333));
        let line_r = RECT {
            left: 0,
            top: 43,
            right: width,
            bottom: 44,
        };
        let _ = FillRect(hdc, &line_r, line_brush);
        let _ = DeleteObject(line_brush.into());

        let _ = SetBkMode(hdc, TRANSPARENT);
        let _ = SetTextColor(hdc, COLORREF(0x00FFFFFF));

        draw_bar_btn(hdc, 10, 6, 80, 32, "開く (Ctrl+O)");
        let view_txt = if state.view_mode == ViewMode::Normal {
            "通常 (V)"
        } else {
            "合成 (V)"
        };
        draw_bar_btn(hdc, 95, 6, 65, 32, view_txt);

        let mon_str = format!(
            "モニタ: #{}",
            state.target_monitor().map_or(1, |m| m.number)
        );
        draw_bar_btn(hdc, 165, 6, 75, 32, &mon_str);

        let split_str = match state.config.layout.mode.as_str() {
            "single" => "分割: 1",
            "split2" => "分割: 2",
            "split3" => "分割: 3",
            _ => "分割: 自動",
        };
        draw_bar_btn(hdc, 245, 6, 70, 32, split_str);

        let scale_str = format!("M: {:.2}", state.config.layout.max_scale);
        draw_bar_btn(hdc, 320, 6, 65, 32, &scale_str);

        draw_bar_btn(hdc, 390, 6, 60, 32, "適用 ↵");
        draw_bar_btn(hdc, 455, 6, 65, 32, "ランダム R");
        draw_bar_btn(hdc, 525, 6, 70, 32, "戻す ^Z");
        draw_bar_btn(hdc, 600, 6, 70, 32, "PNG保存 S");
        draw_bar_btn(hdc, 675, 6, 55, 32, "設定");

        if state.has_pinned_frames() {
            draw_badge(hdc, width - 110, 10, "合成固定中", COLORREF(0x000088FF));
        }
    }
}

fn draw_bar_btn(hdc: HDC, x: i32, y: i32, w: i32, h: i32, text: &str) {
    unsafe {
        let r = RECT {
            left: x,
            top: y,
            right: x + w,
            bottom: y + h,
        };
        let brush = CreateSolidBrush(COLORREF(0x003A3A3A));
        let _ = FillRect(hdc, &r, brush);
        let _ = DeleteObject(brush.into());

        let mut tr = r;
        draw_text(hdc, text, &mut tr, DT_CENTER | DT_VCENTER | DT_SINGLELINE);
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
        fill_alpha_rect(hdc, &r, COLORREF(0x00202020), 190);

        let border_brush = CreateSolidBrush(COLORREF(0x00666666));
        let _ = FrameRect(hdc, &r, border_brush);
        let _ = DeleteObject(border_brush.into());

        let _ = SetBkMode(hdc, TRANSPARENT);
        let _ = SetTextColor(hdc, COLORREF(0x00FFFFFF));
        let mut tr = r;
        draw_text(hdc, text, &mut tr, DT_CENTER | DT_VCENTER | DT_SINGLELINE);
    }
}

fn draw_badge(hdc: HDC, x: i32, y: i32, text: &str, color: COLORREF) {
    unsafe {
        let w = osd_text_display_width(text) + 16;
        let h = 20;
        let r = RECT {
            left: x,
            top: y,
            right: x + w,
            bottom: y + h,
        };
        fill_alpha_rect(hdc, &r, color, 200);

        let border_brush = CreateSolidBrush(COLORREF(0x00444444));
        let _ = FrameRect(hdc, &r, border_brush);
        let _ = DeleteObject(border_brush.into());

        let _ = SetBkMode(hdc, TRANSPARENT);
        let _ = SetTextColor(hdc, COLORREF(0x00FFFFFF));
        let mut tr = r;
        draw_text(hdc, text, &mut tr, DT_CENTER | DT_VCENTER | DT_SINGLELINE);
    }
}

fn draw_info_overlay(state: &AppState, hdc: HDC, win_h: i32) {
    unsafe {
        let info_w = 340;
        let info_h = 100;
        let r = RECT {
            left: 16,
            top: win_h - info_h - 16,
            right: 16 + info_w,
            bottom: win_h - 16,
        };

        fill_alpha_rect(hdc, &r, COLORREF(0x00141414), 180);

        let border_brush = CreateSolidBrush(COLORREF(0x00555555));
        let _ = FrameRect(hdc, &r, border_brush);
        let _ = DeleteObject(border_brush.into());

        let _ = SetBkMode(hdc, TRANSPARENT);
        let _ = SetTextColor(hdc, COLORREF(0x00E0E0E0));

        let filename = state
            .catalog
            .current_item()
            .map_or("なし", |it| &it.filename);
        let idx = state.catalog.current_index.map_or(0, |i| i + 1);
        let total = state.catalog.items.len();

        let line1 = format!("{}  ({} / {})", filename, idx, total);
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

        let mut r1 = RECT {
            left: r.left + 10,
            top: r.top + 8,
            right: r.right - 10,
            bottom: r.top + 30,
        };
        let mut r2 = RECT {
            left: r.left + 10,
            top: r.top + 35,
            right: r.right - 10,
            bottom: r.top + 57,
        };
        let mut r3 = RECT {
            left: r.left + 10,
            top: r.top + 62,
            right: r.right - 10,
            bottom: r.top + 84,
        };

        draw_text(hdc, &line1, &mut r1, DT_LEFT | DT_SINGLELINE);
        draw_text(hdc, &line2, &mut r2, DT_LEFT | DT_SINGLELINE);
        draw_text(hdc, &line3, &mut r3, DT_LEFT | DT_SINGLELINE);
    }
}

fn draw_osd_notification(hdc: HDC, text: &str, win_w: i32, has_undo: bool) {
    unsafe {
        let r = osd_notification_rect(text, win_w, has_undo);

        let brush = CreateSolidBrush(COLORREF(0x002B2B2B));
        let _ = FillRect(hdc, &r, brush);
        let _ = DeleteObject(brush.into());

        let border_brush = CreateSolidBrush(COLORREF(0x00555555));
        let _ = FrameRect(hdc, &r, border_brush);
        let _ = DeleteObject(border_brush.into());

        let _ = SetBkMode(hdc, TRANSPARENT);
        let _ = SetTextColor(hdc, COLORREF(0x0000FF99));

        let mut tr = RECT {
            left: r.left + 16,
            top: r.top,
            right: r.right - if has_undo { 120 } else { 16 },
            bottom: r.bottom,
        };
        draw_text(hdc, text, &mut tr, DT_LEFT | DT_VCENTER | DT_SINGLELINE);

        if has_undo {
            let btn_r = osd_undo_button_rect(text, win_w);
            draw_bar_btn(
                hdc,
                btn_r.left,
                btn_r.top,
                btn_r.right - btn_r.left,
                btn_r.bottom - btn_r.top,
                "元に戻す ^Z",
            );
        }
    }
}

fn draw_error_banner(hdc: HDC, errors: &[String], win_w: i32) {
    unsafe {
        let banner_h = 36;
        let r = RECT {
            left: 0,
            top: 45,
            right: win_w,
            bottom: 45 + banner_h,
        };
        let brush = CreateSolidBrush(COLORREF(0x00202088));
        let _ = FillRect(hdc, &r, brush);
        let _ = DeleteObject(brush.into());

        let _ = SetBkMode(hdc, TRANSPARENT);
        let _ = SetTextColor(hdc, COLORREF(0x00FFFFFF));

        let msg = errors.join(" | ");
        let mut tr = RECT {
            left: 16,
            top: r.top,
            right: win_w - 50,
            bottom: r.bottom,
        };
        draw_text(hdc, &msg, &mut tr, DT_LEFT | DT_VCENTER | DT_SINGLELINE);
    }
}

fn draw_settings_modal(hdc: HDC, state: &AppState, win_w: i32, win_h: i32) {
    unsafe {
        let box_w = 520;
        let box_h = 430;
        let bx = (win_w - box_w) / 2;
        let by = (win_h - box_h) / 2;
        let modal_r = RECT {
            left: bx,
            top: by,
            right: bx + box_w,
            bottom: by + box_h,
        };

        let brush = CreateSolidBrush(COLORREF(0x001F1F1F));
        let _ = FillRect(hdc, &modal_r, brush);
        let _ = DeleteObject(brush.into());

        let border_brush = CreateSolidBrush(COLORREF(0x00555555));
        let _ = FrameRect(hdc, &modal_r, border_brush);
        let _ = DeleteObject(border_brush.into());

        let _ = SetBkMode(hdc, TRANSPARENT);
        let _ = SetTextColor(hdc, COLORREF(0x00FFFFFF));

        let mut tr = RECT {
            left: bx + 20,
            top: by + 16,
            right: bx + box_w - 20,
            bottom: by + 45,
        };
        draw_text(hdc, "設定", &mut tr, DT_LEFT | DT_SINGLELINE);

        // Row 1: Max Scale M
        let mut r1 = RECT {
            left: bx + 20,
            top: by + 65,
            right: bx + 150,
            bottom: by + 90,
        };
        draw_text(hdc, "最大拡大率 M:", &mut r1, DT_LEFT | DT_SINGLELINE);
        draw_bar_btn(hdc, bx + 160, by + 60, 40, 28, "-");
        let m_str = format!("{:.2}", state.config.layout.max_scale);
        draw_bar_btn(hdc, bx + 210, by + 60, 60, 28, &m_str);
        draw_bar_btn(hdc, bx + 280, by + 60, 40, 28, "+");

        // Row 2: Border px
        let mut r2 = RECT {
            left: bx + 20,
            top: by + 110,
            right: bx + 150,
            bottom: by + 135,
        };
        draw_text(hdc, "境界線幅:", &mut r2, DT_LEFT | DT_SINGLELINE);
        draw_bar_btn(hdc, bx + 160, by + 105, 40, 28, "-");
        let b_str = format!("{} px", state.config.layout.border_px);
        draw_bar_btn(hdc, bx + 210, by + 105, 60, 28, &b_str);
        draw_bar_btn(hdc, bx + 280, by + 105, 40, 28, "+");

        // Row 3: Random split mode
        let mut r3 = RECT {
            left: bx + 20,
            top: by + 155,
            right: bx + 150,
            bottom: by + 180,
        };
        draw_text(hdc, "ランダム分割:", &mut r3, DT_LEFT | DT_SINGLELINE);
        let r_mode = state.config.random.mode.as_str();
        draw_bar_btn(
            hdc,
            bx + 160,
            by + 150,
            60,
            28,
            if r_mode == "auto" {
                "【自動】"
            } else {
                "自動"
            },
        );
        draw_bar_btn(
            hdc,
            bx + 225,
            by + 150,
            50,
            28,
            if r_mode == "single" {
                "【1枠】"
            } else {
                "1枠"
            },
        );
        draw_bar_btn(
            hdc,
            bx + 280,
            by + 150,
            50,
            28,
            if r_mode == "split2" {
                "【2枠】"
            } else {
                "2枠"
            },
        );
        draw_bar_btn(
            hdc,
            bx + 335,
            by + 150,
            50,
            28,
            if r_mode == "split3" {
                "【3枠】"
            } else {
                "3枠"
            },
        );

        // Row 4: Schedule enabled
        let mut r4 = RECT {
            left: bx + 20,
            top: by + 200,
            right: bx + 150,
            bottom: by + 225,
        };
        draw_text(hdc, "定期更新:", &mut r4, DT_LEFT | DT_SINGLELINE);
        let en_str = if state.config.schedule.enabled {
            "【有効】"
        } else {
            "無効"
        };
        draw_bar_btn(hdc, bx + 160, by + 195, 100, 28, en_str);

        // Row 5: Schedule on logon
        let mut r5 = RECT {
            left: bx + 20,
            top: by + 245,
            right: bx + 150,
            bottom: by + 270,
        };
        draw_text(hdc, "ログオン時更新:", &mut r5, DT_LEFT | DT_SINGLELINE);
        let log_str = if state.config.schedule.on_logon {
            "【有効】"
        } else {
            "無効"
        };
        draw_bar_btn(hdc, bx + 160, by + 240, 100, 28, log_str);

        // Row 6: Schedule interval
        let mut r6 = RECT {
            left: bx + 20,
            top: by + 290,
            right: bx + 150,
            bottom: by + 315,
        };
        draw_text(hdc, "実行間隔:", &mut r6, DT_LEFT | DT_SINGLELINE);
        let int_str = match state.config.schedule.interval_minutes {
            0 => "間隔なし (クリックで変更)".to_string(),
            60 => "1時間 (クリックで変更)".to_string(),
            m if m >= 60 => format!("{}時間 (クリックで変更)", m / 60),
            m => format!("{}分 (クリックで変更)", m),
        };
        draw_bar_btn(hdc, bx + 160, by + 285, 200, 28, &int_str);

        // Close button
        draw_bar_btn(hdc, bx + 200, by + 375, 120, 34, "閉じる (Esc)");
    }
}

fn draw_exclude_list_modal(hdc: HDC, state: &AppState, win_w: i32, win_h: i32) {
    unsafe {
        let box_w = 540;
        let box_h = 440;
        let bx = (win_w - box_w) / 2;
        let by = (win_h - box_h) / 2;
        let modal_r = RECT {
            left: bx,
            top: by,
            right: bx + box_w,
            bottom: by + box_h,
        };

        let brush = CreateSolidBrush(COLORREF(0x001F1F1F));
        let _ = FillRect(hdc, &modal_r, brush);
        let _ = DeleteObject(brush.into());

        let border_brush = CreateSolidBrush(COLORREF(0x00555555));
        let _ = FrameRect(hdc, &modal_r, border_brush);
        let _ = DeleteObject(border_brush.into());

        let _ = SetBkMode(hdc, TRANSPARENT);
        let _ = SetTextColor(hdc, COLORREF(0x00FFFFFF));

        let mut tr = RECT {
            left: bx + 20,
            top: by + 16,
            right: bx + box_w - 20,
            bottom: by + 45,
        };
        draw_text(hdc, "除外画像一覧", &mut tr, DT_LEFT | DT_SINGLELINE);

        let excluded_in_folder: Vec<(&str, &PathBuf)> = state
            .catalog
            .items
            .iter()
            .filter(|it| it.is_excluded)
            .map(|it| (it.filename.as_str(), &it.path))
            .collect();

        if excluded_in_folder.is_empty() {
            let mut r_empty = RECT {
                left: bx + 30,
                top: by + 70,
                right: bx + box_w - 30,
                bottom: by + 100,
            };
            draw_text(
                hdc,
                "現在のフォルダに除外画像はありません。",
                &mut r_empty,
                DT_LEFT | DT_SINGLELINE,
            );
        } else {
            for (i, (fname, _)) in excluded_in_folder.iter().take(5).enumerate() {
                let row_y = by + 60 + (i as i32) * 36;
                let mut r_item = RECT {
                    left: bx + 30,
                    top: row_y + 4,
                    right: bx + 420,
                    bottom: row_y + 28,
                };
                draw_text(hdc, fname, &mut r_item, DT_LEFT | DT_SINGLELINE);
                draw_bar_btn(hdc, bx + 440, row_y, 60, 26, "解除");
            }
        }

        draw_bar_btn(hdc, bx + 30, by + 250, 220, 30, "このフォルダを一括解除");

        let total_ex = state.catalog.excluded_paths.len();
        let other_count = total_ex.saturating_sub(excluded_in_folder.len());
        let mut r_other = RECT {
            left: bx + 30,
            top: by + 295,
            right: bx + box_w - 30,
            bottom: by + 320,
        };
        let other_txt = format!("他フォルダの除外パス: {}件", other_count);
        draw_text(hdc, &other_txt, &mut r_other, DT_LEFT | DT_SINGLELINE);

        draw_bar_btn(hdc, bx + 30, by + 325, 200, 30, "存在しないパスを整理");
        draw_bar_btn(hdc, bx + 245, by + 325, 200, 30, "他フォルダ分を一括解除");

        draw_bar_btn(hdc, bx + 210, by + 380, 120, 34, "閉じる (Esc)");
    }
}

fn draw_shortcuts_modal(hdc: HDC, win_w: i32, win_h: i32) {
    unsafe {
        let box_w = 480;
        let box_h = 520;
        let bx = (win_w - box_w) / 2;
        let by = (win_h - box_h) / 2;
        let modal_r = RECT {
            left: bx,
            top: by,
            right: bx + box_w,
            bottom: by + box_h,
        };

        let brush = CreateSolidBrush(COLORREF(0x001F1F1F));
        let _ = FillRect(hdc, &modal_r, brush);
        let _ = DeleteObject(brush.into());

        let border_brush = CreateSolidBrush(COLORREF(0x00555555));
        let _ = FrameRect(hdc, &modal_r, border_brush);
        let _ = DeleteObject(border_brush.into());

        let _ = SetBkMode(hdc, TRANSPARENT);
        let _ = SetTextColor(hdc, COLORREF(0x00FFFFFF));

        let mut tr = RECT {
            left: bx + 20,
            top: by + 16,
            right: bx + box_w - 20,
            bottom: by + 45,
        };
        draw_text(hdc, "操作一覧", &mut tr, DT_LEFT | DT_SINGLELINE);

        let lines = [
            "ホイール / → / ← / ↑ / ↓ : 画像送り",
            "Home / End : 先頭 / 末尾",
            "V / 中クリック : 通常 / 合成 切替",
            "Tab / Shift+Tab : 枠選択",
            "Space : 選択枠のピン留め切替",
            "Shift+Space : 枠の入れ替え",
            "左ドラッグ / Shift+矢印 : パン調整",
            "C : パン位置リセット",
            "L / Shift+L : 分割モード切替",
            "Ctrl+ホイール / + / - : 拡大率変更",
            "1〜9 : モニター選択",
            "Enter : 壁紙適用",
            "Ctrl+1〜9 : 指定モニターへ即時適用",
            "R : 全モニターランダム更新",
            "Ctrl+Z : 元に戻す",
            "S / Ctrl+S : PNG保存",
            "Del : 除外切替",
            "Ctrl+O : フォルダ選択",
            "F5 : 再読込",
            "I : 情報表示切替",
            "F11 / Alt+Enter : 全画面切替",
            "Esc : 閉じる",
        ];

        for (i, line) in lines.iter().enumerate() {
            let mut lr = RECT {
                left: bx + 30,
                top: by + 55 + (i as i32) * 18,
                right: bx + box_w - 30,
                bottom: by + 75 + (i as i32) * 18,
            };
            draw_text(hdc, line, &mut lr, DT_LEFT | DT_SINGLELINE);
        }

        draw_bar_btn(hdc, bx + 180, by + 465, 120, 34, "閉じる (Esc)");
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

fn show_monitor_overlays(monitors: &[MonitorInfo]) {
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

unsafe extern "system" fn overlay_wnd_proc(
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

                let brush = CreateSolidBrush(COLORREF(0x00222222));
                let _ = FillRect(hdc, &cr, brush);
                let _ = DeleteObject(brush.into());

                let border_brush = CreateSolidBrush(COLORREF(0x0000CCFF));
                let _ = FrameRect(hdc, &cr, border_brush);
                let _ = DeleteObject(border_brush.into());

                let num = GetWindowLongPtrW(hwnd, GWLP_USERDATA);
                let text = format!("{}", num);

                let _ = SetBkMode(hdc, TRANSPARENT);
                let _ = SetTextColor(hdc, COLORREF(0x00FFFFFF));

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

fn show_context_menu(state: &mut AppState, hwnd: HWND, lparam: LPARAM) {
    unsafe {
        let x = (lparam.0 & 0xffff) as i16 as i32;
        let y = ((lparam.0 >> 16) & 0xffff) as i16 as i32;
        let mut pt = POINT { x, y };
        let _ = ClientToScreen(hwnd, &mut pt);
        show_context_menu_at(state, hwnd, pt.x, pt.y);
    }
}

fn show_context_menu_at(state: &mut AppState, hwnd: HWND, screen_x: i32, screen_y: i32) {
    unsafe {
        let menu = CreatePopupMenu().unwrap();
        let _ = AppendMenuW(menu, MF_STRING, 1001, w!("フォルダを開く (Ctrl+O)"));
        let _ = AppendMenuW(menu, MF_STRING, 1002, w!("再読込 (F5)"));
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
            1001 => state.open_folder_dialog(),
            1002 => {
                state.catalog.reload();
                state.rebuild_layout(true, true);
            }
            1003 => state.modal_dialog = ModalDialog::ExcludeList,
            1004 => state.modal_dialog = ModalDialog::Settings,
            1005 => state.modal_dialog = ModalDialog::Shortcuts,
            _ => {}
        }
    }
}
