use rand::seq::SliceRandom;
use std::collections::{HashMap, HashSet};
use std::hash::{DefaultHasher, Hash, Hasher};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::apply::WallpaperManager;
use crate::catalog::{Catalog, normalize_path};
use crate::compose::{compose_layout, save_png_wic};
use crate::decode::ImageDecoder;
use crate::layout::{FrameInput, ImageSpec, LayoutMode, LayoutRequest, plan_layout};

use crate::monitor::{MonitorInfo, enumerate_monitors};
use crate::store::{AppConfig, Storage};

static SEQ_COUNTER: AtomicU32 = AtomicU32::new(1);

#[derive(Debug)]
pub enum RandomUpdateError {
    Success,                // 0
    Failure(String),        // 1: decode, compose, save, or OS apply failure
    ZeroCandidates(String), // 2: 0 valid candidates
    Locked,                 // 3: update already running
    ConfigOrArgs(String),   // 4: config or args error
}

impl RandomUpdateError {
    pub fn exit_code(&self) -> i32 {
        match self {
            Self::Success => 0,
            Self::Failure(_) => 1,
            Self::ZeroCandidates(_) => 2,
            Self::Locked => 3,
            Self::ConfigOrArgs(_) => 4,
        }
    }
}

#[derive(Debug)]
pub struct RandomUpdateSuccess {
    pub applied_monitors: Vec<(String, PathBuf)>,
    pub failed_monitors: Vec<(String, String)>,
}

pub fn execute_random_update(
    storage: &Storage,
    cfg: &AppConfig,
    is_headless: bool,
    wallpaper_mgr: &mut Option<WallpaperManager>,
) -> Result<RandomUpdateSuccess, RandomUpdateError> {
    // 1. Exclusive Lock (Spec 6.4)
    let _lock_guard = if is_headless {
        storage
            .try_lock_exclusive()
            .ok_or(RandomUpdateError::Locked)?
    } else {
        storage
            .lock_exclusive_timeout(std::time::Duration::from_secs(5))
            .ok_or_else(|| {
                RandomUpdateError::Failure("定期更新が実行中のため適用できませんでした".to_string())
            })?
    };

    // 2. Validate folder (Spec 6.1, 6.2)
    let folder_str = cfg
        .library
        .folder
        .as_ref()
        .ok_or_else(|| RandomUpdateError::ZeroCandidates("対象フォルダ未設定".to_string()))?;
    let folder = PathBuf::from(folder_str);
    if !folder.is_dir() {
        return Err(RandomUpdateError::ZeroCandidates(format!(
            "対象フォルダが存在しません: {}",
            folder.display()
        )));
    }

    // 3. Catalog and candidate enumeration (Spec 6.1)
    let excluded_set: HashSet<PathBuf> = cfg
        .library
        .excluded
        .iter()
        .map(|s| normalize_path(Path::new(s)))
        .collect();

    let mut catalog = Catalog::new(excluded_set);
    if !catalog.open_folder(&folder, None) {
        return Err(RandomUpdateError::ZeroCandidates(
            "フォルダの読込に失敗しました".to_string(),
        ));
    }

    let mut valid_candidates = catalog.get_valid_candidates();
    if valid_candidates.is_empty() {
        return Err(RandomUpdateError::ZeroCandidates(
            "有効な候補画像が0枚です".to_string(),
        ));
    }

    // 4. Enumerate monitors (Spec 5.1)
    let monitors = enumerate_monitors();
    if monitors.is_empty() {
        return Err(RandomUpdateError::Failure(
            "接続モニターが0台です".to_string(),
        ));
    }

    // 5. Shuffle candidate images (Spec 6.1)
    let mut rng = rand::rng();
    valid_candidates.shuffle(&mut rng);

    let decoder = ImageDecoder::new().map_err(|e| {
        RandomUpdateError::Failure(format!("Failed to initialize image decoder: {}", e))
    })?;

    // Determine layout mode
    let layout_mode = if valid_candidates.len() == 1 {
        // Spec 6.1: 有効候補が1枚なら全モニターを1枠として処理する
        LayoutMode::Fixed(1)
    } else {
        match cfg.random.mode.as_str() {
            "single" => LayoutMode::Fixed(1),
            "split2" => LayoutMode::Fixed(2),
            "split3" => LayoutMode::Fixed(3),
            _ => LayoutMode::Auto,
        }
    };

    let now_utc_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();

    // Map of generated outputs: monitor_id -> output_png_path
    let mut planned_outputs: Vec<(MonitorInfo, PathBuf)> = Vec::new();

    // Spec 5.3: 全モニター分のPNG生成・保存がすべて成功してから適用する
    // 生成段階で1台でも失敗した場合は適用を開始せず既存壁紙を維持する
    let mut candidate_pool = valid_candidates.clone();

    for monitor in &monitors {
        let (mw, mh) = (monitor.width, monitor.height);
        if mw == 0 || mh == 0 {
            return Err(RandomUpdateError::Failure(format!(
                "Invalid monitor resolution: {}x{}",
                mw, mh
            )));
        }

        // Try to allocate valid images for this monitor
        let mut used_for_this_monitor: Vec<PathBuf> = Vec::new();
        let mut candidate_specs: Vec<ImageSpec> = Vec::new();

        // Peek or test decodes from candidate_pool
        let mut checked_idx = 0;
        while candidate_specs.len() < 3 && checked_idx < candidate_pool.len() {
            let path = &candidate_pool[checked_idx];
            checked_idx += 1;

            match decoder.read_image_spec(path) {
                Ok(spec) => {
                    candidate_specs.push(spec);
                    used_for_this_monitor.push(path.clone());
                }
                Err(e) => {
                    // Mark failed in catalog and continue (Spec 6.1)
                    catalog.mark_failed(path, format!("{}", e));
                    storage.log(
                        "WARN",
                        &format!("Image decode failed for {}: {}", path.display(), e),
                    );
                }
            }
        }

        if candidate_specs.is_empty() {
            return Err(RandomUpdateError::ZeroCandidates(
                "デコード可能な有効候補画像がありません".to_string(),
            ));
        }

        // Build LayoutRequest
        let first_frame = FrameInput {
            image: Some(candidate_specs[0].clone()),
            pinned: false,
            pan: (0.5, 0.5),
        };

        let req = LayoutRequest {
            monitor_px: (mw, mh),
            mode: layout_mode,
            max_scale: cfg.layout.max_scale,
            border_px: cfg.layout.border_px,
            frames: vec![first_frame],
        };

        let plan = plan_layout(&req, &candidate_specs[1..]);

        // Decode required images
        let mut images_map = HashMap::new();
        for frame in &plan.frames {
            if let Some(placed) = &frame.content {
                let p = PathBuf::from(&placed.id.0);
                if !images_map.contains_key(&placed.id) {
                    let decoded = decoder.decode_image(&p, Some(4096)).map_err(|e| {
                        RandomUpdateError::Failure(format!(
                            "Failed to decode image {}: {}",
                            p.display(),
                            e
                        ))
                    })?;
                    images_map.insert(placed.id.clone(), decoded);
                }
            }
        }

        // Compose layout
        let composed = compose_layout(&plan, &images_map);

        // Generate unique file path: <monitor_hash>_<utc_ms>_<seq>.png (Spec 5.3)
        let mut hasher = DefaultHasher::new();
        monitor.id.hash(&mut hasher);
        let mon_hash = hasher.finish() as u32;
        let seq = SEQ_COUNTER.fetch_add(1, Ordering::Relaxed);
        let filename = format!("{:08x}_{}_{:04}.png", mon_hash, now_utc_ms, seq);
        let output_path = storage.wallpapers_dir.join(filename);

        save_png_wic(&composed, &output_path).map_err(|e| {
            RandomUpdateError::Failure(format!(
                "Failed to save wallpaper PNG {}: {}",
                output_path.display(),
                e
            ))
        })?;

        planned_outputs.push((monitor.clone(), output_path));

        // Spec 6.1: Move only adopted images for this monitor to the back of the candidate pool
        // to avoid duplicate images across monitors
        let adopted_images: Vec<PathBuf> = plan
            .frames
            .iter()
            .filter_map(|f| f.content.as_ref())
            .map(|c| PathBuf::from(&c.id.0))
            .collect();

        for used in &adopted_images {
            if let Some(pos) = candidate_pool.iter().position(|p| p == used) {
                let removed = candidate_pool.remove(pos);
                candidate_pool.push(removed);
            }
        }
    }

    // 6. Apply to all monitors (Spec 5.3)
    if wallpaper_mgr.is_none()
        && let Ok(mgr) = WallpaperManager::new()
    {
        *wallpaper_mgr = Some(mgr);
    }

    let mgr = wallpaper_mgr.as_mut().ok_or_else(|| {
        RandomUpdateError::Failure("Failed to initialize wallpaper manager".to_string())
    })?;

    // Capture snapshot for undo before modifying (Spec 5.4)
    let mon_ids: Vec<String> = monitors.iter().map(|m| m.id.clone()).collect();
    mgr.capture_snapshot(&mon_ids);

    let mut applied = Vec::new();
    let mut failed = Vec::new();

    for (monitor, path) in planned_outputs {
        // Spec 5.3: In random/all-monitors update, auto change to Fill if needed
        match mgr.apply_wallpaper(&monitor.id, &path, true) {
            Ok(_) => {
                applied.push((monitor.id.clone(), path));
            }
            Err(e) => {
                storage.log(
                    "ERROR",
                    &format!(
                        "Failed to apply wallpaper to monitor {}: {}",
                        monitor.name, e
                    ),
                );
                failed.push((monitor.id.clone(), format!("{}", e)));
            }
        }
    }

    // 7. Cleanup old wallpapers (Spec 7.3)
    let mut protected = HashSet::new();
    for (_, path) in &applied {
        protected.insert(normalize_path(path));
        protected.insert(path.clone());
    }
    if let Some(snap) = mgr.last_undo_snapshot() {
        for item in &snap.monitor_wallpapers {
            protected.insert(normalize_path(&item.wallpaper_path));
            protected.insert(item.wallpaper_path.clone());
        }
    }
    for mon_id in &mon_ids {
        if let Ok(p) = mgr.get_current_wallpaper(mon_id) {
            protected.insert(normalize_path(&p));
            protected.insert(p.clone());
        }
    }
    for dev_id in mgr.get_wallpaper_monitor_ids() {
        if let Ok(p) = mgr.get_current_wallpaper(&dev_id) {
            protected.insert(normalize_path(&p));
            protected.insert(p.clone());
        }
    }
    storage.cleanup_wallpapers(&protected);

    if !failed.is_empty() {
        storage.log(
            "ERROR",
            &format!(
                "Random update partially failed: {} succeeded, {} failed",
                applied.len(),
                failed.len()
            ),
        );
        return Err(RandomUpdateError::Failure(format!(
            "Failed to apply wallpaper to {} monitors",
            failed.len()
        )));
    }

    storage.log(
        "INFO",
        &format!("Random update succeeded for {} monitors", applied.len()),
    );

    Ok(RandomUpdateSuccess {
        applied_monitors: applied,
        failed_monitors: failed,
    })
}
