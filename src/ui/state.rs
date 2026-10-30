use std::collections::{HashMap, HashSet};
use std::hash::{DefaultHasher, Hash, Hasher};
use std::os::windows::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use rayon::prelude::*;
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::System::Com::*;
use windows::Win32::UI::Shell::*;
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::*;

use crate::apply::WallpaperManager;
use crate::catalog::{Catalog, is_supported_image, normalize_path};
use crate::compose::{
    ComposedBitmap, compose_base_layer, compose_foreground_layer,
    compose_single_frame_foreground, save_png_wic,
};
use crate::decode::{DecodedImage, ImageDecoder};
use crate::layout::{
    FrameInput, ImageId, ImageSpec, LayoutMode, LayoutPlan, LayoutRequest, plan_layout,
};
use crate::monitor::{MonitorInfo, enumerate_monitors};
use crate::random::execute_random_update;
use crate::schedule::SchedulerManager;
use crate::smart_pan::calculate_smart_pan;
use crate::store::{AppConfig, FramePreset, Storage, WallpaperPreset};
use crate::ui::render::show_monitor_overlays;
use crate::ui::types::{ModalDialog, SEQ_COUNTER, ViewMode};
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
    pub active_base_layer: Option<ComposedBitmap>,
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
    pub normal_zoom: f64,
    pub normal_pan_offset: (f64, f64),
    pub normal_pan_start_offset: (f64, f64),
    pub modal_dialog: ModalDialog,
    pub hwnd: HWND,
    pub config_dirty: bool,
    pub last_config_change: Instant,
    pub recent_wallpapers: HashMap<String, Vec<PathBuf>>,
    pub selected_preset_idx: usize,
    pub show_filmstrip: bool,
    pub show_sidebar: bool,
    pub sidebar_scroll_offset: i32,
    pub collapsed_folders: HashSet<PathBuf>,
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
            active_base_layer: None,
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
            normal_zoom: 1.0,
            normal_pan_offset: (0.0, 0.0),
            normal_pan_start_offset: (0.0, 0.0),
            modal_dialog: ModalDialog::None,
            hwnd: HWND(std::ptr::null_mut()),
            config_dirty: false,
            last_config_change: Instant::now(),
            recent_wallpapers: HashMap::new(),
            selected_preset_idx: 0,
            show_filmstrip: false,
            show_sidebar: false,
            sidebar_scroll_offset: 0,
            collapsed_folders: HashSet::new(),
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

    pub fn add_recent_folder(&mut self, folder: &Path) {
        let s = folder.to_string_lossy().to_string();
        self.config.library.recent_folders.retain(|f| f != &s);
        self.config.library.recent_folders.insert(0, s);
        self.config.library.recent_folders.truncate(10);
    }

    pub fn open_path(&mut self, p: &Path) {
        if p.is_file() {
            if is_supported_image(p) {
                if let Some(parent) = p.parent()
                    && self.catalog.open_folder(parent, Some(p))
                {
                    self.add_recent_folder(parent);
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
                self.add_recent_folder(p);
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

        // Re-compose layout bitmap using base layer cache for instant pan updates
        let base_layer = compose_base_layer(&plan, &self.cached_images, self.config.layout.dim_ratio);
        let mut composed = base_layer.clone();
        compose_foreground_layer(&mut composed, &plan, &self.cached_images);

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
        self.active_base_layer = Some(base_layer);
        self.active_composed = Some(composed);

        self.prefetch_adjacent_images();
        self.prune_image_cache();
    }

    /// Fast pan update: updates only the foreground of the selected frame without recomputing background blur.
    pub fn update_pan_fast(&mut self, frame_idx: usize, new_pan: (f64, f64)) {
        if frame_idx >= self.layout_frames.len() {
            return;
        }
        self.layout_frames[frame_idx].pan = new_pan;

        let Some(monitor) = self.target_monitor() else {
            return;
        };
        let (mw, mh) = (monitor.width, monitor.height);
        let layout_mode = match self.config.layout.mode.as_str() {
            "single" => LayoutMode::Fixed(1),
            "split2" => LayoutMode::Fixed(2),
            "split3" => LayoutMode::Fixed(3),
            _ => {
                let count = self.layout_frames.len().clamp(1, 3) as u8;
                LayoutMode::Fixed(count)
            }
        };

        let req = LayoutRequest {
            monitor_px: (mw, mh),
            mode: layout_mode,
            max_scale: self.config.layout.max_scale,
            border_px: self.config.layout.border_px,
            frames: self.layout_frames.clone(),
        };

        let plan = plan_layout(&req, &[]);

        if let (Some(composed), Some(base)) = (&mut self.active_composed, &self.active_base_layer) {
            if let Some(frame_plan) = plan.frames.get(frame_idx) {
                compose_single_frame_foreground(composed, frame_plan, base, &self.cached_images);
            }
        }

        self.active_plan = Some(plan);
    }

    /// Preload and decode the current normal view image if missing, plus trigger prefetch and pruning.
    pub fn ensure_current_image_loaded(&mut self) {
        if let Some(item) = self.catalog.current_item() {
            let img_id = ImageId(item.path.to_string_lossy().to_string());
            if !self.cached_images.contains_key(&img_id)
                && let Ok(dec) = self.decoder.decode_image(&item.path, Some(4096))
            {
                self.cached_images.insert(img_id, dec);
            }
            self.prefetch_adjacent_images();
            self.prune_image_cache();
        }
    }

    /// Parallel prefetch of adjacent images using Rayon worker threads
    pub fn prefetch_adjacent_images(&mut self) {
        if self.catalog.items.is_empty() {
            return;
        }
        let n = self.catalog.items.len();
        let cur = self.catalog.current_index.unwrap_or(0);
        let indices = [(cur + n - 1) % n, (cur + 1) % n];

        let to_fetch: Vec<PathBuf> = indices
            .iter()
            .filter_map(|&idx| {
                let item = &self.catalog.items[idx];
                if item.failure_reason.is_none() {
                    let id = ImageId(item.path.to_string_lossy().to_string());
                    if !self.cached_images.contains_key(&id) {
                        return Some(item.path.clone());
                    }
                }
                None
            })
            .collect();

        if to_fetch.is_empty() {
            return;
        }

        let decoded_items: Vec<(ImageId, DecodedImage)> = to_fetch
            .into_par_iter()
            .filter_map(|path| {
                if let Ok(decoder) = ImageDecoder::new()
                    && let Ok(dec) = decoder.decode_image(&path, Some(4096))
                {
                    Some((ImageId(path.to_string_lossy().to_string()), dec))
                } else {
                    None
                }
            })
            .collect();

        for (id, dec) in decoded_items {
            self.cached_images.insert(id, dec);
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

    /// Releases prefetch cache (±1 adjacent images) when the window is minimized (Spec 8.2),
    /// while retaining currently displayed images.
    pub fn release_prefetch_cache(&mut self) {
        let mut keep = HashSet::new();

        // 1. Current normal view image
        if let Some(item) = self.catalog.current_item() {
            keep.insert(ImageId(item.path.to_string_lossy().to_string()));
        }

        // 2. Composite layout frames (all active frames currently displayed)
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

        // Evict any cached image not in keep (i.e. evict prefetch cache)
        self.cached_images.retain(|id, _| keep.contains(id));
    }

    pub fn next_image(&mut self) {
        if self.view_mode == ViewMode::Normal {
            self.reset_normal_zoom();
            self.catalog.next();
            if !self.has_pinned_frames() {
                self.rebuild_layout(true, true);
            } else {
                self.ensure_current_image_loaded();
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
        self.ensure_current_item_visible_in_sidebar();
    }

    pub fn prev_image(&mut self) {
        if self.view_mode == ViewMode::Normal {
            self.reset_normal_zoom();
            self.catalog.prev();
            if !self.has_pinned_frames() {
                self.rebuild_layout(true, true);
            } else {
                self.ensure_current_image_loaded();
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
        self.ensure_current_item_visible_in_sidebar();
    }

    pub fn first_image(&mut self) {
        if self.view_mode == ViewMode::Normal {
            self.reset_normal_zoom();
            self.catalog.first();
            if !self.has_pinned_frames() {
                self.rebuild_layout(true, true);
            } else {
                self.ensure_current_image_loaded();
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
        self.ensure_current_item_visible_in_sidebar();
    }

    pub fn last_image(&mut self) {
        if self.view_mode == ViewMode::Normal {
            self.reset_normal_zoom();
            self.catalog.last();
            if !self.has_pinned_frames() {
                self.rebuild_layout(true, true);
            } else {
                self.ensure_current_image_loaded();
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
        self.ensure_current_item_visible_in_sidebar();
    }

    pub fn random_image(&mut self) {
        if self.view_mode == ViewMode::Normal {
            self.reset_normal_zoom();
            if self.catalog.items.is_empty() {
                self.set_osd("画像がありません".to_string(), 2, false);
                return;
            }
            if self.catalog.items.len() == 1 {
                self.set_osd("画像が1枚のみです".to_string(), 2, false);
                return;
            }

            let cur_path = self.catalog.current_item().map(|it| it.path.clone());
            let empty_set = HashSet::new();
            if let Some((idx, _)) =
                self.catalog
                    .choose_random_candidate(cur_path.as_deref(), &empty_set)
            {
                self.catalog.current_index = Some(idx);
                if !self.has_pinned_frames() {
                    self.rebuild_layout(true, true);
                } else {
                    self.ensure_current_image_loaded();
                }
                if let Some(item) = self.catalog.current_item() {
                    self.set_osd(format!("ランダム: {}", item.filename), 2, false);
                }
            } else {
                self.set_osd("候補画像がありません".to_string(), 2, false);
            }
        } else {
            if let Some(frame) = self.layout_frames.get(self.selected_frame_idx)
                && frame.pinned
            {
                self.set_osd("ピン留め中".to_string(), 2, false);
                return;
            }

            if self.catalog.items.is_empty() {
                self.set_osd("画像がありません".to_string(), 2, false);
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

            if let Some((_, next_path)) =
                self.catalog
                    .choose_random_candidate(current_path.as_deref(), &other_used)
            {
                if current_path.as_ref() == Some(&next_path) {
                    self.set_osd("変更可能な別候補がありません".to_string(), 2, false);
                    return;
                }
                if let Ok(spec) = self.decoder.read_image_spec(&next_path) {
                    if self.selected_frame_idx < self.layout_frames.len() {
                        self.layout_frames[self.selected_frame_idx].image = Some(spec);
                        self.layout_frames[self.selected_frame_idx].pan = (0.5, 0.5);
                        if self.selected_frame_idx == 0 {
                            self.catalog.select_by_path(&next_path);
                        }
                    }
                    self.rebuild_layout(false, false);
                    let filename = next_path
                        .file_name()
                        .and_then(|s| s.to_str())
                        .unwrap_or("");
                    self.set_osd(
                        format!("枠{} ランダム: {}", self.selected_frame_idx + 1, filename),
                        2,
                        false,
                    );
                } else {
                    self.set_osd("画像の読み込みに失敗しました".to_string(), 2, true);
                }
            } else {
                self.set_osd("変更可能な別候補がありません".to_string(), 2, false);
            }
        }
        self.ensure_current_item_visible_in_sidebar();
    }

    pub fn switch_view_mode(&mut self) {
        self.reset_normal_zoom();
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
            self.set_osd("パン位置を中央にリセットしました".into(), 2, false);
            self.update_pan_fast(self.selected_frame_idx, (0.5, 0.5));
        }
    }

    pub fn reset_normal_zoom(&mut self) {
        self.normal_zoom = 1.0;
        self.normal_pan_offset = (0.0, 0.0);
    }

    pub fn zoom_normal(&mut self, factor: f64) {
        let old_zoom = self.normal_zoom;
        let new_zoom = (self.normal_zoom * factor).clamp(1.0, 10.0);
        self.normal_zoom = new_zoom;
        if new_zoom <= 1.0 {
            self.normal_pan_offset = (0.0, 0.0);
            self.set_osd("ズーム: 全体表示 (Fit)".into(), 1, false);
        } else {
            let scale_change = new_zoom / old_zoom;
            self.normal_pan_offset.0 *= scale_change;
            self.normal_pan_offset.1 *= scale_change;
            let percent = (self.normal_zoom * 100.0).round() as i32;
            self.set_osd(format!("ズーム: {}%", percent), 1, false);
        }
    }

    pub fn toggle_normal_100_percent(&mut self, canvas_w: i32, canvas_h: i32) {
        if let Some(item) = self.catalog.current_item() {
            let img_id = ImageId(item.path.to_string_lossy().to_string());
            if let Some(dec) = self.cached_images.get(&img_id) {
                let fit_scale = (canvas_w as f64 / dec.width as f64)
                    .min(canvas_h as f64 / dec.height as f64)
                    .min(1.0);
                if fit_scale > 0.0 {
                    let target_zoom = 1.0 / fit_scale;
                    if (self.normal_zoom - target_zoom).abs() < 0.05 {
                        self.reset_normal_zoom();
                        self.set_osd("ズーム: 全体表示 (Fit)".into(), 1, false);
                    } else {
                        self.normal_zoom = target_zoom.clamp(1.0, 10.0);
                        self.normal_pan_offset = (0.0, 0.0);
                        self.set_osd("ズーム: 100% (等倍)".into(), 1, false);
                    }
                }
            }
        }
    }

    pub fn cycle_dim_ratio(&mut self) {
        let cur = (self.config.layout.dim_ratio * 10.0).round() as i32;
        let next_dim = match cur {
            0 => 0.2,
            2 => 0.4,
            4 => 0.6,
            _ => 0.0,
        };
        self.config.layout.dim_ratio = next_dim;
        self.mark_config_dirty();
        let pct = (next_dim * 100.0).round() as i32;
        self.set_osd(format!("背景ぼかし減光: {}%", pct), 2, false);
        self.rebuild_layout(false, false);
    }

    pub fn save_current_as_preset(&mut self) {
        let preset_num = self.config.presets.len() + 1;
        let name = format!("プリセット {}", preset_num);

        let frames = self
            .layout_frames
            .iter()
            .map(|f| FramePreset {
                image_path: f.image.as_ref().map(|img| img.id.0.clone()),
                pinned: f.pinned,
                pan: f.pan,
            })
            .collect();

        let preset = WallpaperPreset {
            name: name.clone(),
            monitor_id: self.target_monitor().map(|m| m.id.clone()),
            mode: self.config.layout.mode.clone(),
            max_scale: self.config.layout.max_scale,
            border_px: self.config.layout.border_px,
            dim_ratio: self.config.layout.dim_ratio,
            frames,
        };

        self.config.presets.push(preset);
        self.mark_config_dirty();
        let _ = self.storage.save_config(&self.config);
        self.set_osd(format!("「{}」を保存しました", name), 3, false);
    }

    pub fn load_next_preset(&mut self) {
        if self.config.presets.is_empty() {
            self.set_osd("保存されたプリセットがありません (Ctrl+Pで保存)".into(), 2, false);
            return;
        }

        let idx = self.selected_preset_idx % self.config.presets.len();
        let preset = self.config.presets[idx].clone();
        self.selected_preset_idx = (idx + 1) % self.config.presets.len();

        self.config.layout.mode = preset.mode;
        self.config.layout.max_scale = preset.max_scale;
        self.config.layout.border_px = preset.border_px;
        self.config.layout.dim_ratio = preset.dim_ratio;

        if let Some(target_id) = &preset.monitor_id {
            if let Some(pos) = self.monitors.iter().position(|m| &m.id == target_id) {
                self.target_monitor_idx = pos;
            }
        }

        self.layout_frames.clear();
        for fp in &preset.frames {
            let spec = if let Some(path_str) = &fp.image_path {
                self.decoder.read_image_spec(Path::new(path_str)).ok()
            } else {
                None
            };
            self.layout_frames.push(FrameInput {
                image: spec,
                pinned: fp.pinned,
                pan: fp.pan,
            });
        }

        self.view_mode = ViewMode::Composite;
        self.rebuild_layout(false, false);
        self.set_osd(
            format!(
                "プリセット適用: {} ({}/{})",
                preset.name,
                idx + 1,
                self.config.presets.len()
            ),
            3,
            false,
        );
    }

    pub fn apply_smart_pan_current_frame(&mut self) {
        if self.view_mode != ViewMode::Composite {
            self.set_osd("合成表示でのみ有効です (Vキーで切替)".into(), 2, false);
            return;
        }

        if let Some(frame) = self.layout_frames.get_mut(self.selected_frame_idx)
            && let Some(spec) = &frame.image
        {
            let path = PathBuf::from(&spec.id.0);
            let img_id = spec.id.clone();
            if !self.cached_images.contains_key(&img_id) {
                if let Ok(dec) = self.decoder.decode_image(&path, Some(4096)) {
                    self.cached_images.insert(img_id.clone(), dec);
                }
            }

            if let Some(dec) = self.cached_images.get(&img_id)
                && let Some(plan) = &self.active_plan
                && let Some(frame_plan) = plan.frames.get(self.selected_frame_idx)
                && let Some(content) = &frame_plan.content
            {
                let slot = frame_plan.slot;
                let (pan_x, pan_y) = calculate_smart_pan(dec, slot.width, slot.height, content.scale);
                self.update_pan_fast(self.selected_frame_idx, (pan_x, pan_y));
                self.set_osd(
                    format!(
                        "スマートパン: 枠{}を被写体・顔に自動調整しました",
                        self.selected_frame_idx + 1
                    ),
                    2,
                    false,
                );
            }
        }
    }

    pub fn current_selected_image_path(&self) -> Option<PathBuf> {
        if self.view_mode == ViewMode::Composite {
            self.layout_frames
                .get(self.selected_frame_idx)
                .and_then(|f| f.image.as_ref())
                .map(|img| PathBuf::from(&img.id.0))
        } else {
            self.catalog.current_item().map(|it| it.path.clone())
        }
    }

    pub fn open_in_explorer(&mut self) {
        if let Some(path) = self.current_selected_image_path() {
            let norm_path = normalize_path(&path);
            if norm_path.exists() {
                use std::os::windows::process::CommandExt;
                let arg = format!("/select,\"{}\"", norm_path.to_string_lossy());
                if let Err(e) = std::process::Command::new("explorer").raw_arg(arg).spawn() {
                    self.set_osd(format!("エクスプローラーの起動に失敗しました: {e}"), 3, true);
                } else {
                    let fname = norm_path
                        .file_name()
                        .map(|n| n.to_string_lossy().to_string())
                        .unwrap_or_default();
                    self.set_osd(format!("ファイルの場所を開きました: {fname}"), 2, false);
                }
                return;
            }
        }

        if let Some(folder) = &self.catalog.folder {
            let norm_folder = normalize_path(folder);
            if norm_folder.exists() {
                use std::os::windows::process::CommandExt;
                let arg = format!("\"{}\"", norm_folder.to_string_lossy());
                if let Err(e) = std::process::Command::new("explorer").raw_arg(arg).spawn() {
                    self.set_osd(format!("エクスプローラーの起動に失敗しました: {e}"), 3, true);
                } else {
                    self.set_osd(format!("フォルダを開きました: {}", norm_folder.display()), 2, false);
                }
                return;
            }
        }

        self.set_osd("開く画像またはフォルダがありません".into(), 2, true);
    }

    pub fn toggle_exclude_current(&mut self) {
        let path = self.current_selected_image_path();

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

    pub fn trigger_random_update(&mut self) {
        let storage_ref = &self.storage;
        let cfg_ref = &self.config;
        let mut mgr_ref = self.wallpaper_mgr.take();
        match execute_random_update(storage_ref, cfg_ref, false, &mut mgr_ref) {
            Ok(succ) => {
                self.wallpaper_mgr = mgr_ref;
                self.set_osd(
                    format!(
                        "全{}台の壁紙をランダム更新しました",
                        succ.applied_monitors.len()
                    ),
                    6,
                    true,
                );
                self.rebuild_layout(false, false);
            }
            Err(e) => {
                self.wallpaper_mgr = mgr_ref;
                self.set_osd(format!("ランダム更新失敗: {:?}", e), 4, false);
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

            let target_folder = self
                .current_selected_image_path()
                .and_then(|p| p.parent().map(|p| p.to_path_buf()))
                .or_else(|| self.catalog.folder.clone());

            if let Some(folder) = &target_folder {
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

    pub fn toggle_sidebar(&mut self) {
        self.show_sidebar = !self.show_sidebar;
        if self.show_sidebar {
            self.ensure_current_item_visible_in_sidebar();
        }
        self.set_osd(
            format!(
                "サイドバー: {}",
                if self.show_sidebar {
                    "表示"
                } else {
                    "非表示"
                }
            ),
            2,
            false,
        );
    }

    pub fn toggle_folder_collapse(&mut self, folder: &Path) {
        let pb = folder.to_path_buf();
        if self.collapsed_folders.contains(&pb) {
            self.collapsed_folders.remove(&pb);
        } else {
            self.collapsed_folders.insert(pb);
        }
    }

    pub fn scroll_sidebar(&mut self, delta: i32, list_h: i32, total_h: i32) {
        let max_scroll = (total_h - list_h).max(0);
        self.sidebar_scroll_offset = (self.sidebar_scroll_offset - delta).clamp(0, max_scroll);
    }

    pub fn select_image_by_index(&mut self, target_idx: usize) {
        if target_idx >= self.catalog.items.len() {
            return;
        }
        if self.view_mode == ViewMode::Normal {
            self.catalog.select_by_index(target_idx);
            self.reset_normal_zoom();
            if !self.has_pinned_frames() {
                self.rebuild_layout(true, true);
            } else {
                self.ensure_current_image_loaded();
            }
        } else {
            let next_path = self.catalog.items[target_idx].path.clone();
            if let Ok(spec) = self.decoder.read_image_spec(&next_path) {
                if self.selected_frame_idx < self.layout_frames.len() {
                    self.layout_frames[self.selected_frame_idx].image = Some(spec);
                    self.layout_frames[self.selected_frame_idx].pan = (0.5, 0.5);
                    if self.selected_frame_idx == 0 {
                        self.catalog.select_by_path(&next_path);
                    }
                    self.rebuild_layout(false, false);
                }
            }
        }
        self.ensure_current_item_visible_in_sidebar();
    }

    pub fn ensure_current_item_visible_in_sidebar(&mut self) {
        let Some(cur_idx) = self.catalog.current_index else {
            return;
        };
        let Some(cur_item) = self.catalog.items.get(cur_idx) else {
            return;
        };

        // If parent folder is collapsed, expand it
        if let Some(parent) = cur_item.path.parent() {
            self.collapsed_folders.remove(parent);
        }

        let dpi = if !self.hwnd.0.is_null() {
            crate::ui::get_window_dpi(self.hwnd)
        } else {
            96
        };

        let folder_h = crate::ui::scale_by_dpi(28, dpi);
        let item_h = crate::ui::scale_by_dpi(44, dpi);

        let mut groups: Vec<(PathBuf, Vec<usize>)> = Vec::new();
        for (idx, it) in self.catalog.items.iter().enumerate() {
            let parent = it.path.parent().unwrap_or(Path::new("")).to_path_buf();
            if let Some(last) = groups.last_mut() {
                if last.0 == parent {
                    last.1.push(idx);
                    continue;
                }
            }
            groups.push((parent, vec![idx]));
        }

        let mut y_accum = 0;
        let mut target_y = None;

        for (folder_path, item_indices) in groups {
            let is_collapsed = self.collapsed_folders.contains(&folder_path);
            y_accum += folder_h;
            if !is_collapsed {
                for idx in item_indices {
                    if idx == cur_idx {
                        target_y = Some(y_accum);
                        break;
                    }
                    y_accum += item_h;
                }
                if target_y.is_some() {
                    break;
                }
            }
        }

        if let Some(ty) = target_y {
            let list_h = if !self.hwnd.0.is_null() {
                let mut cr = RECT::default();
                let _ = unsafe { windows::Win32::UI::WindowsAndMessaging::GetClientRect(self.hwnd, &mut cr) };
                let bar_h = if self.bar_visible { crate::ui::get_bar_height(dpi) } else { 0 };
                let strip_h = if self.show_filmstrip { crate::ui::scale_by_dpi(92, dpi) } else { 0 };
                let header_h = crate::ui::scale_by_dpi(38, dpi);
                (cr.bottom - bar_h - strip_h - header_h).max(100)
            } else {
                400
            };

            let padding = crate::ui::scale_by_dpi(40, dpi);
            if ty < self.sidebar_scroll_offset {
                self.sidebar_scroll_offset = ty.saturating_sub(padding);
            } else if ty + item_h > self.sidebar_scroll_offset + list_h {
                self.sidebar_scroll_offset = (ty + item_h + padding - list_h).max(0);
            }
        }
    }
}
