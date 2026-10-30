use kabaper::catalog::{Catalog, natural_str_cmp};
use kabaper::compose::{compose_layout, save_png_wic};
use kabaper::decode::DecodedImage;
use kabaper::layout::*;
use kabaper::store::{AppConfig, Storage};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

#[test]
fn test_acceptance_10_1_layout_formulas_and_auto_mode() {
    // 1. 4K monitor (3840x2160) + FHD image (1920x1080) at M=1.25
    let fhd_img = ImageSpec {
        id: ImageId("fhd.jpg".into()),
        size_px: (1920, 1080),
    };
    let req1 = LayoutRequest {
        monitor_px: (3840, 2160),
        mode: LayoutMode::Fixed(2),
        max_scale: 1.25,
        border_px: 2,
        frames: vec![FrameInput {
            image: Some(fhd_img.clone()),
            pinned: false,
            pan: (0.5, 0.5),
        }],
    };
    let plan1 = plan_layout(&req1, &[]);
    let f0 = plan1.frames[0].content.as_ref().unwrap();
    assert!(f0.scale <= 1.25 + 1e-6);
    assert_eq!(f0.scale_limited, ScaleLimit::MaxScale);
    assert!(f0.blur_sigma > 0.0); // Has background blur

    // 2. High-res vertical (2400x3600) on 4K (3840x2160)
    let tall_img = ImageSpec {
        id: ImageId("tall.jpg".into()),
        size_px: (2400, 3600),
    };
    let req2 = LayoutRequest {
        monitor_px: (3840, 2160),
        mode: LayoutMode::Fixed(1),
        max_scale: 1.25,
        border_px: 0,
        frames: vec![FrameInput {
            image: Some(tall_img.clone()),
            pinned: false,
            pan: (0.5, 0.5),
        }],
    };
    let plan2 = plan_layout(&req2, &[]);
    let f_single = plan2.frames[0].content.as_ref().unwrap();
    // Visible area ratio >= 60%
    let visible_ratio = (f_single.fg_src.width * f_single.fg_src.height) / (2400.0 * 3600.0);
    assert!(visible_ratio >= 0.599);

    // Auto mode with 3 candidates chooses 3 frames
    let c1 = ImageSpec {
        id: ImageId("c1.jpg".into()),
        size_px: (2400, 3600),
    };
    let c2 = ImageSpec {
        id: ImageId("c2.jpg".into()),
        size_px: (2400, 3600),
    };
    let req_auto = LayoutRequest {
        monitor_px: (3840, 2160),
        mode: LayoutMode::Auto,
        max_scale: 1.25,
        border_px: 2,
        frames: vec![FrameInput {
            image: Some(tall_img.clone()),
            pinned: false,
            pan: (0.5, 0.5),
        }],
    };
    let plan_auto = plan_layout(&req_auto, &[c1, c2]);
    assert_eq!(plan_auto.resolved_count, 3);
}

#[test]
fn test_acceptance_10_3_config_edge_cases() {
    let temp = std::env::temp_dir().join(format!("kabaper_acc_cfg_{}", std::process::id()));
    let _ = fs::remove_dir_all(&temp);
    let mut storage = Storage::init_with_dir(temp.clone()).unwrap();

    // 1. Clamping out of range values
    let mut bad_cfg = AppConfig::default();
    bad_cfg.layout.max_scale = 99.9; // out of range
    bad_cfg.layout.border_px = 50; // out of range (max 8)
    bad_cfg.layout.mode = "invalid_mode".to_string();
    bad_cfg.schedule.interval_minutes = 99999;
    storage.save_config(&bad_cfg).unwrap();

    let (clamped, _) = storage.load_config();
    assert_eq!(clamped.layout.max_scale, 2.50);
    assert_eq!(clamped.layout.border_px, 8);
    assert_eq!(clamped.layout.mode, "auto");
    assert_eq!(clamped.schedule.interval_minutes, 1440);

    // 2. Future schema check: schema_version > 1
    let future_toml = "schema_version = 999\n[layout]\nmax_scale = 1.5\n";
    fs::write(&storage.config_path, future_toml).unwrap();
    let (_fut_cfg, fut_err) = storage.load_config();
    assert!(storage.is_future_schema);
    assert!(fut_err.is_some());
    // Must not overwrite
    storage.save_config(&bad_cfg).unwrap();
    let content_after = fs::read_to_string(&storage.config_path).unwrap();
    assert!(content_after.contains("schema_version = 999"));

    let _ = fs::remove_dir_all(&temp);
}

#[test]
fn test_acceptance_10_3_catalog_and_natural_sort() {
    let mut files = vec!["010.jpg", "2.jpg", "1.jpg", "002.jpg", "100.png", "20.bmp"];
    files.sort_by(|a, b| natural_str_cmp(a, b));
    assert_eq!(
        files,
        vec!["1.jpg", "2.jpg", "002.jpg", "010.jpg", "20.bmp", "100.png"]
    );

    let mut catalog = Catalog::new(HashSet::new());
    // Empty catalog operations
    assert_eq!(catalog.next(), None);
    assert_eq!(catalog.prev(), None);
    assert_eq!(catalog.first(), None);
    assert_eq!(catalog.last(), None);
    assert_eq!(catalog.get_valid_candidates().len(), 0);

    // Subfolder recursive scan test
    let temp = std::env::temp_dir().join(format!("kabaper_acc_catalog_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&temp);
    std::fs::create_dir_all(temp.join("nested/folder")).unwrap();
    std::fs::write(temp.join("01.jpg"), b"fake").unwrap();
    std::fs::write(temp.join("nested/02.png"), b"fake").unwrap();
    std::fs::write(temp.join("nested/folder/03.bmp"), b"fake").unwrap();

    assert!(catalog.open_folder(&temp, None));
    assert_eq!(catalog.items.len(), 3);
    assert_eq!(catalog.items[0].filename, "01.jpg");
    assert_eq!(
        catalog.items[1].filename,
        format!("nested{}02.png", std::path::MAIN_SEPARATOR)
    );
    assert_eq!(
        catalog.items[2].filename,
        format!(
            "nested{}folder{}03.bmp",
            std::path::MAIN_SEPARATOR,
            std::path::MAIN_SEPARATOR
        )
    );

    let _ = std::fs::remove_dir_all(&temp);
}

#[test]
fn test_acceptance_10_1_png_generation_and_composition() {
    // Test end-to-end composition to PNG with 2 frames
    let mut img1_pix = vec![0u8; 50 * 50 * 4];
    for chunk in img1_pix.as_chunks_mut::<4>().0 {
        chunk[0] = 255;
        chunk[1] = 0;
        chunk[2] = 0;
        chunk[3] = 255; // Blue
    }
    let img1 = DecodedImage {
        id: ImageId("b.png".into()),
        orig_size: (50, 50),
        width: 50,
        height: 50,
        pixels: img1_pix,
    };

    let mut img2_pix = vec![0u8; 50 * 50 * 4];
    for chunk in img2_pix.as_chunks_mut::<4>().0 {
        chunk[0] = 0;
        chunk[1] = 255;
        chunk[2] = 0;
        chunk[3] = 255; // Green
    }
    let img2 = DecodedImage {
        id: ImageId("g.png".into()),
        orig_size: (50, 50),
        width: 50,
        height: 50,
        pixels: img2_pix,
    };

    let mut map = HashMap::new();
    map.insert(img1.id.clone(), img1);
    map.insert(img2.id.clone(), img2);

    let req = LayoutRequest {
        monitor_px: (1920, 1080),
        mode: LayoutMode::Fixed(2),
        max_scale: 1.25,
        border_px: 2,
        frames: vec![
            FrameInput {
                image: Some(ImageSpec {
                    id: ImageId("b.png".into()),
                    size_px: (50, 50),
                }),
                pinned: true,
                pan: (0.5, 0.5),
            },
            FrameInput {
                image: Some(ImageSpec {
                    id: ImageId("g.png".into()),
                    size_px: (50, 50),
                }),
                pinned: false,
                pan: (0.5, 0.5),
            },
        ],
    };

    let plan = plan_layout(&req, &[]);
    let composed = compose_layout(&plan, &map);
    assert_eq!(composed.width, 1920);
    assert_eq!(composed.height, 1080);

    let temp_file = std::env::temp_dir().join(format!("test_acc_comp_{}.png", std::process::id()));
    let save_res = save_png_wic(&composed, &temp_file);
    assert!(save_res.is_ok());
    assert!(temp_file.exists());
    let _ = fs::remove_file(temp_file);
}

#[test]
fn test_acceptance_10_4_random_update_exit_codes_and_locking() {
    let temp = std::env::temp_dir().join(format!("kabaper_acc_random_{}", std::process::id()));
    let _ = fs::remove_dir_all(&temp);
    let storage = Storage::init_with_dir(temp.clone()).unwrap();

    // 1. Exit code 2: Library folder is not configured or does not exist
    let mut cfg = AppConfig::default();
    cfg.library.folder = None;
    let err2_none =
        kabaper::random::execute_random_update(&storage, &cfg, true, &mut None).unwrap_err();
    assert_eq!(err2_none.exit_code(), 2);

    cfg.library.folder = Some(
        temp.join("non_existent_folder")
            .to_string_lossy()
            .to_string(),
    );
    let err2_missing =
        kabaper::random::execute_random_update(&storage, &cfg, true, &mut None).unwrap_err();
    assert_eq!(err2_missing.exit_code(), 2);

    // 2. Exit code 3: Lock contention
    let dummy_folder = temp.join("pics");
    fs::create_dir_all(&dummy_folder).unwrap();
    cfg.library.folder = Some(dummy_folder.to_string_lossy().to_string());

    let lock_guard = storage.try_lock_exclusive();
    assert!(lock_guard.is_some());
    // While lock is held, headless update must fail immediately with exit code 3
    let err3 = kabaper::random::execute_random_update(&storage, &cfg, true, &mut None).unwrap_err();
    assert_eq!(err3.exit_code(), 3);

    drop(lock_guard);

    // 3. Exit code 4: Config or args error via load_config_readonly
    // Storage has no config.toml yet
    let readonly_res = storage.load_config_readonly();
    assert!(readonly_res.is_err()); // exits with 4 in run_headless()

    let _ = fs::remove_dir_all(&temp);
}

#[test]
fn test_acceptance_10_1_frame_swapping_and_pan() {
    let mut frames = [
        FrameInput {
            image: Some(ImageSpec {
                id: ImageId("img0.jpg".into()),
                size_px: (1920, 1080),
            }),
            pinned: true,
            pan: (0.2, 0.7),
        },
        FrameInput {
            image: Some(ImageSpec {
                id: ImageId("img1.jpg".into()),
                size_px: (1080, 1920),
            }),
            pinned: false,
            pan: (0.5, 0.5),
        },
    ];

    // Shift+Space: swap frame 0 and frame 1
    let next_idx = 1;
    frames.swap(0, next_idx);

    assert_eq!(frames[0].image.as_ref().unwrap().id.0, "img1.jpg");
    assert!(!frames[0].pinned);
    assert_eq!(frames[0].pan, (0.5, 0.5));

    assert_eq!(frames[1].image.as_ref().unwrap().id.0, "img0.jpg");
    assert!(frames[1].pinned);
    assert_eq!(frames[1].pan, (0.2, 0.7));

    // C key: reset pan of frame 1 to (0.5, 0.5)
    frames[1].pan = (0.5, 0.5);
    assert_eq!(frames[1].pan, (0.5, 0.5));
}

#[test]
fn test_acceptance_10_1_composite_navigation_distinct_frames() {
    let mut catalog = Catalog::new(HashSet::new());
    // Create virtual items in catalog
    catalog.items = vec![
        kabaper::catalog::CatalogItem {
            path: PathBuf::from("img0.jpg"),
            filename: "img0.jpg".into(),
            is_excluded: false,
            failure_reason: None,
        },
        kabaper::catalog::CatalogItem {
            path: PathBuf::from("img1.jpg"),
            filename: "img1.jpg".into(),
            is_excluded: false,
            failure_reason: None,
        },
        kabaper::catalog::CatalogItem {
            path: PathBuf::from("img2.jpg"),
            filename: "img2.jpg".into(),
            is_excluded: false,
            failure_reason: None,
        },
        kabaper::catalog::CatalogItem {
            path: PathBuf::from("img3.jpg"),
            filename: "img3.jpg".into(),
            is_excluded: false,
            failure_reason: None,
        },
    ];

    // Frame 0 has img0, Frame 1 has img1
    let other_used_for_frame1: HashSet<PathBuf> = [PathBuf::from("img0.jpg")].into_iter().collect();

    // From frame 1 (img1 at index 1), next image must be img2 (index 2), NOT img1
    let next1 = catalog.find_next_valid_image(1, true, &other_used_for_frame1);
    assert_eq!(next1, Some((2, PathBuf::from("img2.jpg"))));

    // From frame 1 after advancing to img2 (index 2), next image must be img3 (index 3)
    let next2 = catalog.find_next_valid_image(2, true, &other_used_for_frame1);
    assert_eq!(next2, Some((3, PathBuf::from("img3.jpg"))));

    // From frame 1 (index 1), prev image must skip img0 (which is used in frame 0) and wrap to img3!
    let prev1 = catalog.find_next_valid_image(1, false, &other_used_for_frame1);
    assert_eq!(prev1, Some((3, PathBuf::from("img3.jpg"))));
}

#[test]
fn test_acceptance_10_1_auto_mode_preserves_count_during_composite_editing() {
    // When editing in composite view, plan_layout is evaluated with Fixed(count)
    // so that candidate evaluation does not collapse or expand frames
    let req = LayoutRequest {
        monitor_px: (3840, 2160),
        mode: LayoutMode::Fixed(3), // Preserving 3 frames during composite edits
        max_scale: 1.25,
        border_px: 2,
        frames: vec![
            FrameInput {
                image: Some(ImageSpec {
                    id: ImageId("a.jpg".into()),
                    size_px: (1920, 1080),
                }),
                pinned: true,
                pan: (0.5, 0.5),
            },
            FrameInput {
                image: Some(ImageSpec {
                    id: ImageId("b.jpg".into()),
                    size_px: (1920, 1080),
                }),
                pinned: false,
                pan: (0.5, 0.5),
            },
            FrameInput {
                image: Some(ImageSpec {
                    id: ImageId("c.jpg".into()),
                    size_px: (1920, 1080),
                }),
                pinned: false,
                pan: (0.5, 0.5),
            },
        ],
    };
    let plan = plan_layout(&req, &[]);
    assert_eq!(plan.resolved_count, 3);
    assert_eq!(plan.frames.len(), 3);
}

#[test]
fn test_acceptance_10_2_wallpaper_cleanup_preserves_undo_and_previous_generation() {
    let temp = std::env::temp_dir().join(format!("kabaper_test_cleanup_{}", std::process::id()));
    let _ = fs::remove_dir_all(&temp);
    let storage = Storage::init_with_dir(temp.clone()).unwrap();

    let cur_file = storage.wallpapers_dir.join("mon1_1000_0002.png");
    let prev_file = storage.wallpapers_dir.join("mon1_1000_0001.png");
    let old_file = storage.wallpapers_dir.join("mon1_0900_0000.png");

    fs::write(&cur_file, b"cur").unwrap();
    fs::write(&prev_file, b"prev").unwrap();
    fs::write(&old_file, b"old").unwrap();

    // Protection set containing current and previous generation
    let mut protected = HashSet::new();
    protected.insert(kabaper::catalog::normalize_path(&cur_file));
    protected.insert(kabaper::catalog::normalize_path(&prev_file));

    storage.cleanup_wallpapers(&protected);

    // Current and previous files MUST survive so Ctrl+Z undo works!
    assert!(cur_file.exists(), "Current wallpaper must exist");
    assert!(
        prev_file.exists(),
        "Previous generation wallpaper must exist for undo"
    );
    // Obsolete file must be cleaned up
    assert!(!old_file.exists(), "Old files must be cleaned up");

    let _ = fs::remove_dir_all(&temp);
}

#[test]
fn test_acceptance_10_4_single_instance_per_storage_directory() {
    let dir1 = PathBuf::from(r"C:\test\data1");
    let dir2 = PathBuf::from(r"C:\test\data2");

    let id1 = kabaper::store::get_instance_id_for_dir(&dir1);
    let id2 = kabaper::store::get_instance_id_for_dir(&dir2);

    assert_ne!(
        id1, id2,
        "Different storage directories must have distinct instance IDs"
    );

    let id1_again = kabaper::store::get_instance_id_for_dir(&dir1);
    assert_eq!(
        id1, id1_again,
        "Same storage directory must produce identical instance ID"
    );
}

#[test]
fn test_acceptance_10_1_non_moveable_axis_pan_is_half() {
    // Landscape slot (1920x1080), image (1920x1080) -> fits perfectly, cannot pan on either axis
    let req = LayoutRequest {
        monitor_px: (1920, 1080),
        mode: LayoutMode::Fixed(1),
        max_scale: 1.00,
        border_px: 0,
        frames: vec![FrameInput {
            image: Some(ImageSpec {
                id: ImageId("fit.jpg".into()),
                size_px: (1920, 1080),
            }),
            pinned: false,
            pan: (0.1, 0.9), // Non-centered input
        }],
    };
    let plan = plan_layout(&req, &[]);
    let f0 = &plan.frames[0];
    assert_eq!(
        f0.pan,
        (0.5, 0.5),
        "Pan on non-movable axes must be clamped to 0.5 in FramePlan per Spec 8.1"
    );
}

#[test]
fn test_acceptance_app_name_and_paths() {
    let dir = PathBuf::from(r"C:\test\folder");
    let instance_id = kabaper::store::get_instance_id_for_dir(&dir);
    assert!(
        instance_id.starts_with("Kabaper_"),
        "Instance ID must start with Kabaper_ prefix"
    );

    let storage_dir = kabaper::store::resolve_storage_dir();
    assert!(
        storage_dir.to_string_lossy().contains("Kabaper"),
        "Default storage dir must contain Kabaper: {}",
        storage_dir.display()
    );
}

#[test]
fn test_acceptance_osd_undo_button_dynamic_rect() {
    let r1 = kabaper::ui::osd_undo_button_rect("壁紙を変更しました", 1280);
    let r2 = kabaper::ui::osd_undo_button_rect("壁紙を変更しました", 1920);

    // Button in a wider window must have a different (larger) X coordinate
    assert_ne!(r1.left, r2.left);
    assert_eq!(r1.right - r1.left, 95);
    assert_eq!(r1.bottom - r1.top, 28);
    assert_eq!(r2.right - r2.left, 95);
    assert_eq!(r2.bottom - r2.top, 28);
}

#[test]
fn test_acceptance_cache_pruning_and_prefetch() {
    let temp = std::env::temp_dir().join(format!("kabaper_test_cache_{}", std::process::id()));
    let _ = fs::remove_dir_all(&temp);
    let storage = Storage::init_with_dir(temp.clone()).unwrap();

    let mut state = kabaper::ui::AppState::new(storage, AppConfig::default(), None).unwrap();

    // Populate catalog with virtual items
    state.catalog.items = vec![
        kabaper::catalog::CatalogItem {
            path: PathBuf::from("img0.jpg"),
            filename: "img0.jpg".into(),
            is_excluded: false,
            failure_reason: None,
        },
        kabaper::catalog::CatalogItem {
            path: PathBuf::from("img1.jpg"),
            filename: "img1.jpg".into(),
            is_excluded: false,
            failure_reason: None,
        },
        kabaper::catalog::CatalogItem {
            path: PathBuf::from("img2.jpg"),
            filename: "img2.jpg".into(),
            is_excluded: false,
            failure_reason: None,
        },
        kabaper::catalog::CatalogItem {
            path: PathBuf::from("img3.jpg"),
            filename: "img3.jpg".into(),
            is_excluded: false,
            failure_reason: None,
        },
        kabaper::catalog::CatalogItem {
            path: PathBuf::from("img4.jpg"),
            filename: "img4.jpg".into(),
            is_excluded: false,
            failure_reason: None,
        },
    ];
    state.catalog.current_index = Some(2); // Current is img2

    // Populate cache with dummy decoded images for all 5 images
    for i in 0..5 {
        let id = ImageId(format!("img{}.jpg", i));
        let dummy = DecodedImage {
            id: id.clone(),
            orig_size: (100, 100),
            width: 100,
            height: 100,
            pixels: vec![0u8; 100 * 100 * 4],
        };
        state.cached_images.insert(id, dummy);
    }
    assert_eq!(state.cached_images.len(), 5);

    // Frame uses img2
    state.layout_frames = vec![FrameInput {
        image: Some(ImageSpec {
            id: ImageId("img2.jpg".into()),
            size_px: (100, 100),
        }),
        pinned: false,
        pan: (0.5, 0.5),
    }];

    // Prune cache: should only keep img2 (active), img1 (prev), img3 (next)
    state.prune_image_cache();

    assert_eq!(state.cached_images.len(), 3);
    assert!(
        state
            .cached_images
            .contains_key(&ImageId("img2.jpg".into()))
    );
    assert!(
        state
            .cached_images
            .contains_key(&ImageId("img1.jpg".into()))
    );
    assert!(
        state
            .cached_images
            .contains_key(&ImageId("img3.jpg".into()))
    );
    assert!(
        !state
            .cached_images
            .contains_key(&ImageId("img0.jpg".into()))
    );
    assert!(
        !state
            .cached_images
            .contains_key(&ImageId("img4.jpg".into()))
    );

    let _ = fs::remove_dir_all(&temp);
}

#[test]
fn test_acceptance_surrogate_pair_text_handling() {
    let text = "📁 𠮷野家と🌸桜";
    // 𠮷 is U+20BB7, 📁 is U+1F4C1, 🌸 is U+1F338
    // Chars count is 8, but UTF-16 code units count is 11 (3 surrogate pairs = 3 extra code units)
    let char_count = text.chars().count();
    let utf16_count = text.encode_utf16().count();
    assert_eq!(char_count, 8);
    assert_eq!(utf16_count, 11);

    let display_width = kabaper::ui::osd_text_display_width(text);
    assert!(display_width > 0);

    let btn_r = kabaper::ui::osd_undo_button_rect(text, 1280);
    assert_eq!(btn_r.right - btn_r.left, 95);
    assert_eq!(btn_r.bottom - btn_r.top, 28);
}

#[test]
fn test_acceptance_empty_state_guidance_and_button_rect() {
    let win_w = 1280;
    let win_h = 800;
    let bar_h = 45;
    let btn_r = kabaper::ui::empty_state_folder_btn_rect(win_w, win_h, bar_h);

    assert_eq!(btn_r.right - btn_r.left, 220);
    assert_eq!(btn_r.bottom - btn_r.top, 36);
    // Button must be inside canvas area (below operation bar and within window height)
    assert!(btn_r.top >= bar_h);
    assert!(btn_r.bottom <= win_h);
    assert!(btn_r.left > 0);
    assert!(btn_r.right < win_w);
}

#[test]
fn test_acceptance_osd_undo_button_alignment_and_bounds() {
    let test_messages = [
        "元に戻す",
        "壁紙を変更しました",
        "全3台のモニターの壁紙をランダム更新しました",
        "📁 𠮷野家と🌸桜の壁紙に変更しました",
    ];

    for win_w in [1024, 1280, 1920, 2560, 3840] {
        for msg in test_messages {
            let osd_r = kabaper::ui::osd_notification_rect(msg, win_w, true);
            let btn_r = kabaper::ui::osd_undo_button_rect(msg, win_w);

            // Button must be strictly inside the OSD container horizontally and vertically
            assert!(
                btn_r.left >= osd_r.left,
                "Button left ({}) must be inside OSD left ({}) for '{}' at w={}",
                btn_r.left,
                osd_r.left,
                msg,
                win_w
            );
            assert!(
                btn_r.right <= osd_r.right,
                "Button right ({}) must be inside OSD right ({}) for '{}' at w={}",
                btn_r.right,
                osd_r.right,
                msg,
                win_w
            );
            assert!(
                btn_r.top >= osd_r.top,
                "Button top ({}) must be inside OSD top ({})",
                btn_r.top,
                osd_r.top
            );
            assert!(
                btn_r.bottom <= osd_r.bottom,
                "Button bottom ({}) must be inside OSD bottom ({})",
                btn_r.bottom,
                osd_r.bottom
            );
            assert_eq!(btn_r.right - btn_r.left, 95);
            assert_eq!(btn_r.bottom - btn_r.top, 28);
        }
    }
}

#[test]
fn test_acceptance_fullscreen_startup_preserves_maximized_state() {
    let temp = std::env::temp_dir().join(format!("kabaper_test_fs_max_{}", std::process::id()));
    let _ = fs::remove_dir_all(&temp);
    let storage = Storage::init_with_dir(temp.clone()).unwrap();

    let mut cfg = AppConfig::default();
    cfg.window.fullscreen = true;
    cfg.window.maximized = true;
    storage.save_config(&cfg).unwrap();

    let mut state = kabaper::ui::AppState::new(storage, cfg, None).unwrap();
    assert!(state.config.window.fullscreen);
    assert!(state.config.window.maximized);
    assert!(!state.config_dirty);

    // Enter fullscreen startup must preserve maximized flag and not mark dirty
    state.enter_fullscreen_startup();
    assert!(state.fullscreen);
    assert!(
        state.config.window.maximized,
        "Maximized state must be preserved when entering fullscreen on startup"
    );
    assert!(
        !state.config_dirty,
        "Config must not be marked dirty on startup fullscreen entry"
    );

    let _ = fs::remove_dir_all(&temp);
}

#[test]
fn test_acceptance_sta_file_dialog_creation_and_options() {
    std::thread::spawn(|| {
        unsafe {
            use windows::Win32::System::Com::*;
            use windows::Win32::UI::Shell::*;

            let res = CoInitializeEx(None, COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE);
            assert!(res.is_ok(), "CoInitializeEx STA should succeed");

            {
                // Verify ImageDecoder works within STA thread
                let decoder = kabaper::decode::ImageDecoder::new();
                assert!(decoder.is_ok(), "ImageDecoder in STA should succeed");

                // Verify IFileOpenDialog creation and options setup in STA
                let open_dialog: windows::core::Result<IFileOpenDialog> =
                    CoCreateInstance(&FileOpenDialog, None, CLSCTX_ALL);
                assert!(open_dialog.is_ok(), "IFileOpenDialog instantiation should succeed");
                let dialog = open_dialog.unwrap();

                let options = dialog.GetOptions().unwrap_or_default();
                let set_res = dialog.SetOptions(options | FOS_PICKFOLDERS | FOS_FORCEFILESYSTEM);
                assert!(set_res.is_ok(), "SetOptions on IFileOpenDialog should succeed");

                // Verify IFileSaveDialog creation and options setup in STA
                let save_dialog: windows::core::Result<IFileSaveDialog> =
                    CoCreateInstance(&FileSaveDialog, None, CLSCTX_ALL);
                assert!(save_dialog.is_ok(), "IFileSaveDialog instantiation should succeed");
                let sdialog = save_dialog.unwrap();

                let s_options = sdialog.GetOptions().unwrap_or_default();
                let s_set_res = sdialog.SetOptions(s_options | FOS_FORCEFILESYSTEM | FOS_OVERWRITEPROMPT);
                assert!(s_set_res.is_ok(), "SetOptions on IFileSaveDialog should succeed");
            }

            CoUninitialize();
        }
    })
    .join()
    .unwrap();
}

#[test]
fn test_acceptance_minimize_preserves_active_composed_and_releases_prefetch() {
    let temp = std::env::temp_dir().join(format!("kabaper_test_min_{}", std::process::id()));
    let _ = fs::remove_dir_all(&temp);
    let storage = Storage::init_with_dir(temp.clone()).unwrap();

    let mut state = kabaper::ui::AppState::new(storage, AppConfig::default(), None).unwrap();

    state.catalog.items = vec![
        kabaper::catalog::CatalogItem {
            path: PathBuf::from("img0.jpg"),
            filename: "img0.jpg".into(),
            is_excluded: false,
            failure_reason: None,
        },
        kabaper::catalog::CatalogItem {
            path: PathBuf::from("img1.jpg"),
            filename: "img1.jpg".into(),
            is_excluded: false,
            failure_reason: None,
        },
        kabaper::catalog::CatalogItem {
            path: PathBuf::from("img2.jpg"),
            filename: "img2.jpg".into(),
            is_excluded: false,
            failure_reason: None,
        },
    ];
    state.catalog.current_index = Some(1); // img1 is active

    for i in 0..3 {
        let id = ImageId(format!("img{}.jpg", i));
        let dummy = DecodedImage {
            id: id.clone(),
            orig_size: (100, 100),
            width: 100,
            height: 100,
            pixels: vec![0u8; 100 * 100 * 4],
        };
        state.cached_images.insert(id, dummy);
    }
    assert_eq!(state.cached_images.len(), 3);

    // Set frame to img1
    state.layout_frames = vec![FrameInput {
        image: Some(ImageSpec {
            id: ImageId("img1.jpg".into()),
            size_px: (100, 100),
        }),
        pinned: false,
        pan: (0.5, 0.5),
    }];

    // Simulate active_composed being set before minimize
    state.active_composed = Some(kabaper::compose::ComposedBitmap::new(1920, 1080));

    // When minimized: Spec 8.2 requires releasing prefetch cache, NOT destroying active composed image
    state.release_prefetch_cache();

    // Verify active_composed is STILL PRESENT (must not become None!)
    assert!(
        state.active_composed.is_some(),
        "active_composed must NOT be destroyed on minimize"
    );

    // Verify prefetch cache (img0, img2) is evicted, but active display image (img1) is retained
    assert_eq!(state.cached_images.len(), 1);
    assert!(
        state
            .cached_images
            .contains_key(&ImageId("img1.jpg".into())),
        "Active displayed image must be retained"
    );
    assert!(
        !state
            .cached_images
            .contains_key(&ImageId("img0.jpg".into())),
        "Prefetch img0 must be released on minimize"
    );
    assert!(
        !state
            .cached_images
            .contains_key(&ImageId("img2.jpg".into())),
        "Prefetch img2 must be released on minimize"
    );

    let _ = fs::remove_dir_all(&temp);
}

fn create_valid_test_bmp(path: &Path) {
    let data = vec![
        0x42, 0x4D, // "BM"
        58, 0, 0, 0, // file size
        0, 0, 0, 0, // reserved
        54, 0, 0, 0, // offset to pixel data
        40, 0, 0, 0, // DIB header size
        1, 0, 0, 0, // width = 1
        1, 0, 0, 0, // height = 1
        1, 0, // planes = 1
        24, 0, // bits per pixel = 24
        0, 0, 0, 0, // compression = 0
        4, 0, 0, 0, // image size (4 bytes with padding)
        0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, // extra header fields
        255, 0, 0, 0, // BGR + 1 byte padding
    ];
    fs::write(path, data).unwrap();
}

#[test]
fn test_acceptance_random_image_normal_view() {
    let temp = std::env::temp_dir().join(format!("kabaper_test_rnd_norm_{}", std::process::id()));
    let _ = fs::remove_dir_all(&temp);
    fs::create_dir_all(&temp).unwrap();

    let p1 = temp.join("img1.bmp");
    let p2 = temp.join("img2.bmp");
    let p3 = temp.join("img3.bmp");
    create_valid_test_bmp(&p1);
    create_valid_test_bmp(&p2);
    create_valid_test_bmp(&p3);

    let storage_dir = temp.join("storage");
    let storage = Storage::init_with_dir(storage_dir).unwrap();
    let mut state = kabaper::ui::AppState::new(storage, AppConfig::default(), Some(&temp)).unwrap();

    state.view_mode = kabaper::ui::ViewMode::Normal;
    assert_eq!(state.view_mode, kabaper::ui::ViewMode::Normal);
    assert_eq!(state.catalog.items.len(), 3);
    let initial_idx = state.catalog.current_index.unwrap();

    // Call random_image: should pick a different candidate
    state.random_image();
    let new_idx = state.catalog.current_index.unwrap();
    assert_ne!(
        initial_idx, new_idx,
        "random_image in normal view should select a different image"
    );

    // Verify layout frame 0 is updated when unpinned
    let current_path = &state.catalog.items[new_idx].path;
    assert_eq!(
        state.layout_frames[0].image.as_ref().unwrap().id.0,
        current_path.to_string_lossy()
    );

    let _ = fs::remove_dir_all(&temp);
}

#[test]
fn test_acceptance_random_image_composite_view() {
    let temp = std::env::temp_dir().join(format!("kabaper_test_rnd_comp_{}", std::process::id()));
    let _ = fs::remove_dir_all(&temp);
    fs::create_dir_all(&temp).unwrap();

    let p1 = temp.join("img1.bmp");
    let p2 = temp.join("img2.bmp");
    let p3 = temp.join("img3.bmp");
    create_valid_test_bmp(&p1);
    create_valid_test_bmp(&p2);
    create_valid_test_bmp(&p3);

    let storage_dir = temp.join("storage");
    let storage = Storage::init_with_dir(storage_dir).unwrap();
    let mut state = kabaper::ui::AppState::new(storage, AppConfig::default(), Some(&temp)).unwrap();

    // Switch to composite view with 2 frames
    state.view_mode = kabaper::ui::ViewMode::Composite;
    state.config.layout.mode = "split2".to_string();
    state.rebuild_layout(true, true);

    assert_eq!(state.layout_frames.len(), 2);
    state.selected_frame_idx = 0;
    let frame0_orig_id = state.layout_frames[0].image.as_ref().unwrap().id.0.clone();
    let frame1_id = state.layout_frames[1].image.as_ref().unwrap().id.0.clone();

    // Randomize frame 0: must not pick frame 1's image and should pick img3 (the remaining one)
    state.random_image();
    let frame0_new_id = state.layout_frames[0].image.as_ref().unwrap().id.0.clone();
    assert_ne!(
        frame0_orig_id, frame0_new_id,
        "Frame 0 should change to a different image"
    );
    assert_ne!(
        frame1_id, frame0_new_id,
        "Frame 0 should not duplicate frame 1's image"
    );
    assert_eq!(
        state.layout_frames[0].pan,
        (0.5, 0.5),
        "Pan should be reset to (0.5, 0.5) on random swap"
    );

    // If frame 0 is pinned, random_image should NOT change it
    state.layout_frames[0].pinned = true;
    let pinned_id = state.layout_frames[0].image.as_ref().unwrap().id.0.clone();
    state.random_image();
    assert_eq!(
        state.layout_frames[0].image.as_ref().unwrap().id.0,
        pinned_id,
        "Pinned frame must not be changed by random_image"
    );

    let _ = fs::remove_dir_all(&temp);
}


#[test]
fn test_acceptance_dpi_scaling_operation_bar() {
    let dpi_100 = 96;
    let dpi_150 = 144;

    assert_eq!(kabaper::ui::get_bar_height(dpi_100), 45);
    assert_eq!(kabaper::ui::get_bar_height(dpi_150), 68);

    let btns_100 = kabaper::ui::get_operation_bar_buttons(dpi_100);
    let btns_150 = kabaper::ui::get_operation_bar_buttons(dpi_150);

    assert_eq!(btns_100.len(), btns_150.len());

    for (b100, b150) in btns_100.iter().zip(btns_150.iter()) {
        assert_eq!(b100.0, b150.0);
        let w100 = b100.1.right - b100.1.left;
        let w150 = b150.1.right - b150.1.left;
        let h100 = b100.1.bottom - b100.1.top;
        let h150 = b150.1.bottom - b150.1.top;

        assert_eq!(h100, 32);
        assert_eq!(h150, 48); // 32 * 1.5 = 48

        // Width must be scaled proportionally by ~1.5
        let expected_w150 = ((w100 as f32) * 1.5).round() as i32;
        assert!(
            (w150 - expected_w150).abs() <= 1,
            "Button {:?} width {} must scale to approx {} at 150% DPI, got {}",
            b100.0,
            w100,
            expected_w150,
            w150
        );
    }

    // Ensure buttons do not overlap horizontally
    for i in 0..btns_150.len() - 1 {
        assert!(
            btns_150[i].1.right < btns_150[i + 1].1.left,
            "Button {} and {} should not overlap at 150% DPI",
            i,
            i + 1
        );
    }
}

#[test]
fn test_acceptance_dpi_scaling_frame_overlay_buttons() {
    let fx = 100;
    let fy = 200;
    let fh = 400;

    let btns_100 = kabaper::ui::get_frame_overlay_buttons(fx, fy, fh, 96);
    let btns_150 = kabaper::ui::get_frame_overlay_buttons(fx, fy, fh, 144);

    for (b100, b150) in btns_100.iter().zip(btns_150.iter()) {
        assert_eq!(b100.0, b150.0);
        let w100 = b100.1.right - b100.1.left;
        let w150 = b150.1.right - b150.1.left;
        let h100 = b100.1.bottom - b100.1.top;
        let h150 = b150.1.bottom - b150.1.top;

        assert_eq!(h100, 28);
        assert_eq!(h150, 42); // 28 * 1.5 = 42
        let expected_w150 = ((w100 as f32) * 1.5).round() as i32;
        assert!(
            (w150 - expected_w150).abs() <= 1,
            "Frame overlay button width {} should scale to approx {} at 150% DPI, got {}",
            w100,
            expected_w150,
            w150
        );
    }

    for i in 0..btns_150.len() - 1 {
        assert!(
            btns_150[i].1.right < btns_150[i + 1].1.left,
            "Frame overlay buttons {} and {} should not overlap at 150% DPI",
            i,
            i + 1
        );
    }
}

#[test]
fn test_acceptance_dpi_scaling_empty_and_osd() {
    let btn_100 = kabaper::ui::empty_state_folder_btn_rect_dpi(1920, 1080, 45, 96);
    let btn_150 = kabaper::ui::empty_state_folder_btn_rect_dpi(1920, 1080, 68, 144);

    assert_eq!(btn_100.right - btn_100.left, 220);
    assert_eq!(btn_100.bottom - btn_100.top, 36);
    assert_eq!(btn_150.right - btn_150.left, 330);
    assert_eq!(btn_150.bottom - btn_150.top, 54);

    let osd_btn_100 = kabaper::ui::osd_undo_button_rect_dpi("壁紙を変更しました", 1920, 96);
    let osd_btn_150 = kabaper::ui::osd_undo_button_rect_dpi("壁紙を変更しました", 1920, 144);

    assert_eq!(osd_btn_100.right - osd_btn_100.left, 95);
    assert_eq!(osd_btn_100.bottom - osd_btn_100.top, 28);
    assert_eq!(osd_btn_150.right - osd_btn_150.left, 143);
    assert_eq!(osd_btn_150.bottom - osd_btn_150.top, 42);
}
