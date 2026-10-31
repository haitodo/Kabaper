use std::cmp::Ordering;
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

use rayon::prelude::*;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FolderGroup {
    pub folder_path: PathBuf,
    pub display_name: String,
    pub item_indices: Vec<usize>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CatalogItem {
    pub path: PathBuf,
    pub filename: String,
    pub is_excluded: bool,
    pub failure_reason: Option<String>,
}

#[derive(Debug, Clone)]
pub struct Catalog {
    pub folder: Option<PathBuf>,
    pub items: Vec<CatalogItem>,
    pub current_index: Option<usize>,
    pub excluded_paths: HashSet<PathBuf>,
    pub failed_decodes: HashMap<PathBuf, String>,
    pub path_indices: HashMap<PathBuf, usize>,
    pub folder_groups: Vec<FolderGroup>,
}

impl Catalog {
    pub fn new(excluded_paths: HashSet<PathBuf>) -> Self {
        Self {
            folder: None,
            items: Vec::new(),
            current_index: None,
            excluded_paths,
            failed_decodes: HashMap::new(),
            path_indices: HashMap::new(),
            folder_groups: Vec::new(),
        }
    }

    pub fn sync_metadata(&mut self) {
        self.path_indices.clear();
        for (idx, item) in self.items.iter().enumerate() {
            self.path_indices.insert(item.path.clone(), idx);
        }
        self.rebuild_folder_groups();
    }

    pub fn rebuild_folder_groups(&mut self) {
        self.folder_groups.clear();
        let root_opt = &self.folder;
        for (idx, item) in self.items.iter().enumerate() {
            let parent = item.path.parent().unwrap_or(Path::new("")).to_path_buf();
            if let Some(last_group) = self.folder_groups.last_mut() {
                if last_group.folder_path == parent {
                    last_group.item_indices.push(idx);
                    continue;
                }
            }
            let display_name = if let Some(root) = root_opt {
                if &parent == root {
                    let name = root.file_name().and_then(|n| n.to_str()).unwrap_or("ルート");
                    format!("📁 {} (直下)", name)
                } else if let Ok(rel) = parent.strip_prefix(root) {
                    format!("📁 {}", rel.to_string_lossy())
                } else {
                    let name = parent.file_name().and_then(|n| n.to_str()).unwrap_or("フォルダ");
                    format!("📁 {}", name)
                }
            } else {
                let name = parent.file_name().and_then(|n| n.to_str()).unwrap_or("フォルダ");
                format!("📁 {}", name)
            };
            self.folder_groups.push(FolderGroup {
                folder_path: parent,
                display_name,
                item_indices: vec![idx],
            });
        }
    }

    pub fn find_index_by_path(&self, path: &Path) -> Option<usize> {
        if let Some(&idx) = self.path_indices.get(path) {
            return Some(idx);
        }
        let clean = strip_unc_prefix(path);
        if let Some(&idx) = self.path_indices.get(&clean) {
            return Some(idx);
        }
        let norm = normalize_path(path);
        if let Some(&idx) = self.path_indices.get(&norm) {
            return Some(idx);
        }
        self.items.iter().position(|it| it.path == clean || it.path == norm)
    }

    pub fn open_folder(&mut self, folder: &Path, select_image: Option<&Path>) -> bool {
        let norm_folder = normalize_path(folder);
        if !norm_folder.is_dir() {
            return false;
        }

        self.folder = Some(norm_folder.clone());
        // Spec 2.2: Re-test failed decodes on reload/open
        self.failed_decodes.clear();
        self.reload_internal(select_image);
        true
    }

    pub fn reload(&mut self) {
        let current_path = self.current_item().map(|it| it.path.clone());
        // Spec 2.2: Re-test failed decodes on reload
        self.failed_decodes.clear();
        self.reload_internal(current_path.as_deref());
    }

    fn reload_internal(&mut self, target_to_select: Option<&Path>) {
        let Some(folder) = &self.folder else {
            self.items.clear();
            self.current_index = None;
            return;
        };

        let mut found_paths = collect_images_recursive(folder);

        // Sort with natural sort order in parallel
        found_paths.par_sort_by(|a, b| natural_cmp(a, b));

        // Rebuild items
        self.items = found_paths
            .into_iter()
            .map(|p| {
                let filename = if let Ok(rel) = p.strip_prefix(folder) {
                    rel.to_string_lossy().to_string()
                } else {
                    p.file_name()
                        .and_then(|f| f.to_str())
                        .unwrap_or("")
                        .to_string()
                };
                let is_excluded = self.excluded_paths.contains(&p);
                let failure_reason = self.failed_decodes.get(&p).cloned();
                CatalogItem {
                    path: p,
                    filename,
                    is_excluded,
                    failure_reason,
                }
            })
            .collect();

        if self.items.is_empty() {
            self.current_index = None;
            self.sync_metadata();
            return;
        }

        self.sync_metadata();

        // Selection logic:
        // 1. If target_to_select is specified, try to find it.
        // 2. If deleted, select next image in natural order (or first if was at end).
        // 3. Otherwise select first (index 0).
        if let Some(target) = target_to_select {
            if let Some(idx) = self.find_index_by_path(target) {
                self.current_index = Some(idx);
                return;
            } else {
                let target_norm = normalize_path(target);
                // If previous image was deleted, find the one that would naturally come after it
                let next_idx = self
                    .items
                    .iter()
                    .position(|it| natural_cmp(&it.path, &target_norm) == Ordering::Greater)
                    .unwrap_or(0);
                self.current_index = Some(next_idx);
                return;
            }
        }

        self.current_index = Some(0);
    }

    pub fn current_item(&self) -> Option<&CatalogItem> {
        let idx = self.current_index?;
        self.items.get(idx)
    }

    #[allow(clippy::should_implement_trait)]
    pub fn next(&mut self) -> Option<&CatalogItem> {
        if self.items.is_empty() {
            return None;
        }
        let cur = self.current_index.unwrap_or(0);
        let next_idx = (cur + 1) % self.items.len();
        self.current_index = Some(next_idx);
        self.current_item()
    }

    pub fn prev(&mut self) -> Option<&CatalogItem> {
        if self.items.is_empty() {
            return None;
        }
        let cur = self.current_index.unwrap_or(0);
        let prev_idx = if cur == 0 {
            self.items.len() - 1
        } else {
            cur - 1
        };
        self.current_index = Some(prev_idx);
        self.current_item()
    }

    pub fn first(&mut self) -> Option<&CatalogItem> {
        if self.items.is_empty() {
            return None;
        }
        self.current_index = Some(0);
        self.current_item()
    }

    pub fn last(&mut self) -> Option<&CatalogItem> {
        if self.items.is_empty() {
            return None;
        }
        self.current_index = Some(self.items.len() - 1);
        self.current_item()
    }

    pub fn select_by_path(&mut self, path: &Path) -> bool {
        if let Some(idx) = self.find_index_by_path(path) {
            self.current_index = Some(idx);
            true
        } else {
            false
        }
    }

    pub fn select_by_index(&mut self, idx: usize) -> bool {
        if idx < self.items.len() {
            self.current_index = Some(idx);
            true
        } else {
            false
        }
    }

    pub fn toggle_exclude_current(&mut self) -> Option<bool> {
        let cur_path = self.current_item()?.path.clone();
        Some(self.toggle_exclude(&cur_path))
    }

    pub fn toggle_exclude(&mut self, path: &Path) -> bool {
        let norm = normalize_path(path);
        let is_now_excluded = if self.excluded_paths.contains(&norm) {
            self.excluded_paths.remove(&norm);
            false
        } else {
            self.excluded_paths.insert(norm.clone());
            true
        };

        if let Some(idx) = self.find_index_by_path(&norm) {
            self.items[idx].is_excluded = is_now_excluded;
        } else {
            for item in &mut self.items {
                if item.path == norm {
                    item.is_excluded = is_now_excluded;
                }
            }
        }

        is_now_excluded
    }

    pub fn mark_failed(&mut self, path: &Path, reason: String) {
        let norm = normalize_path(path);
        self.failed_decodes.insert(norm.clone(), reason.clone());
        if let Some(idx) = self.find_index_by_path(&norm) {
            self.items[idx].failure_reason = Some(reason);
        } else {
            for item in &mut self.items {
                if item.path == norm {
                    item.failure_reason = Some(reason.clone());
                }
            }
        }
    }

    pub fn get_valid_candidates(&self) -> Vec<PathBuf> {
        self.items
            .iter()
            .filter(|it| !it.is_excluded && it.failure_reason.is_none())
            .map(|it| it.path.clone())
            .collect()
    }

    pub fn find_next_valid_image(
        &self,
        from_idx: usize,
        forward: bool,
        excluded_ids: &HashSet<PathBuf>,
    ) -> Option<(usize, PathBuf)> {
        let len = self.items.len();
        if len == 0 {
            return None;
        }

        for step in 1..=len {
            let idx = if forward {
                (from_idx + step) % len
            } else {
                (from_idx + len - (step % len)) % len
            };

            let item = &self.items[idx];
            // Must not be in already-used IDs in current frame layout
            if !excluded_ids.contains(&item.path) && item.failure_reason.is_none() {
                return Some((idx, item.path.clone()));
            }
        }

        None
    }

    pub fn choose_random_candidate(
        &self,
        current_path: Option<&Path>,
        excluded_ids: &HashSet<PathBuf>,
    ) -> Option<(usize, PathBuf)> {
        use rand::seq::IteratorRandom;

        if self.items.is_empty() {
            return None;
        }

        let cur_clean = current_path.map(strip_unc_prefix);
        let is_current = |p: &Path| {
            if let Some(ref cp) = cur_clean {
                p == cp
            } else {
                false
            }
        };
        let is_in_excluded_ids = |p: &Path| {
            excluded_ids.contains(p)
        };

        let mut rng = rand::rng();

        // 1. First priority: valid images (!excluded, no failure, not in excluded_ids, and not current)
        let prio1 = self
            .items
            .iter()
            .enumerate()
            .filter(|(_, it)| {
                !it.is_excluded
                    && it.failure_reason.is_none()
                    && !is_in_excluded_ids(&it.path)
                    && !is_current(&it.path)
            })
            .choose(&mut rng);
        if let Some((idx, it)) = prio1 {
            return Some((idx, it.path.clone()));
        }

        // 2. Second priority: valid images including current_path (if current is valid and not in excluded_ids)
        let prio2 = self
            .items
            .iter()
            .enumerate()
            .filter(|(_, it)| {
                !it.is_excluded
                    && it.failure_reason.is_none()
                    && !is_in_excluded_ids(&it.path)
            })
            .choose(&mut rng);
        if let Some((idx, it)) = prio2 {
            return Some((idx, it.path.clone()));
        }

        // 3. Third priority (fallback): any images with no failure that are not in excluded_ids and not current
        let prio3 = self
            .items
            .iter()
            .enumerate()
            .filter(|(_, it)| {
                it.failure_reason.is_none()
                    && !is_in_excluded_ids(&it.path)
                    && !is_current(&it.path)
            })
            .choose(&mut rng);
        if let Some((idx, it)) = prio3 {
            return Some((idx, it.path.clone()));
        }

        // 4. Fallback to any non-failing not in excluded_ids
        self.items
            .iter()
            .enumerate()
            .filter(|(_, it)| it.failure_reason.is_none() && !is_in_excluded_ids(&it.path))
            .choose(&mut rng)
            .map(|(idx, it)| (idx, it.path.clone()))
    }

    pub fn select_random(&mut self) -> Option<&CatalogItem> {
        let cur_path = self.current_item().map(|it| it.path.clone());
        let empty_set = HashSet::new();
        if let Some((idx, _)) = self.choose_random_candidate(cur_path.as_deref(), &empty_set) {
            self.current_index = Some(idx);
            self.current_item()
        } else {
            None
        }
    }
}

pub fn strip_unc_prefix(path: &Path) -> PathBuf {
    let s = path.to_string_lossy();
    if let Some(stripped) = s.strip_prefix(r"\\?\") {
        PathBuf::from(stripped)
    } else {
        path.to_path_buf()
    }
}

pub fn collect_images_recursive(root: &Path) -> Vec<PathBuf> {
    let mut images = Vec::new();
    let norm_root = normalize_path(root);
    let mut dirs_to_visit = vec![norm_root.clone()];
    let mut visited_dirs = HashSet::new();
    visited_dirs.insert(norm_root);

    while let Some(current_dir) = dirs_to_visit.pop() {
        let Ok(entries) = fs::read_dir(&current_dir) else {
            continue;
        };

        for entry in entries.flatten() {
            let path = entry.path();
            let Ok(file_type) = entry.file_type() else {
                continue;
            };

            if file_type.is_dir() {
                // Skip hidden folders (e.g. .git) and recycle bin
                if let Some(name) = path.file_name().and_then(|n| n.to_str())
                    && (name.starts_with('.') || name.eq_ignore_ascii_case("$RECYCLE.BIN"))
                {
                    continue;
                }
                let clean_dir = strip_unc_prefix(&path);
                if visited_dirs.insert(clean_dir) {
                    dirs_to_visit.push(path);
                }
            } else if file_type.is_file() && is_supported_image(&path) {
                images.push(strip_unc_prefix(&path));
            }
        }
    }

    images
}

pub fn is_supported_image(path: &Path) -> bool {
    let Some(ext) = path.extension().and_then(|e| e.to_str()) else {
        return false;
    };
    matches!(
        ext.to_ascii_lowercase().as_str(),
        "jpg" | "jpeg" | "png" | "bmp" | "webp" | "avif"
    )
}

pub fn normalize_path(path: &Path) -> PathBuf {
    // Canonicalize or absolute path normalization
    if let Ok(canon) = fs::canonicalize(path) {
        // Strip Windows UNC prefix \\?\ if present for clean paths
        strip_unc_prefix(&canon)
    } else if path.is_absolute() {
        strip_unc_prefix(path)
    } else if let Ok(cur) = std::env::current_dir() {
        strip_unc_prefix(&cur.join(path))
    } else {
        strip_unc_prefix(path)
    }
}

pub fn natural_cmp(a: &Path, b: &Path) -> Ordering {
    let p_a = a.parent();
    let p_b = b.parent();

    // Fast path: if parent directory paths are identical, skip parent component traversals
    if p_a != p_b {
        match (p_a, p_b) {
            (Some(pa), Some(pb)) => {
                let mut it_a = pa.components();
                let mut it_b = pb.components();
                loop {
                    match (it_a.next(), it_b.next()) {
                        (Some(ca), Some(cb)) => {
                            if ca == cb {
                                continue;
                            }
                            let sa = ca.as_os_str().to_string_lossy();
                            let sb = cb.as_os_str().to_string_lossy();
                            let ord = natural_str_cmp(&sa, &sb);
                            if ord != Ordering::Equal {
                                return ord;
                            }
                        }
                        (Some(_), None) => return Ordering::Greater,
                        (None, Some(_)) => return Ordering::Less,
                        (None, None) => break,
                    }
                }
            }
            (Some(_), None) => return Ordering::Greater,
            (None, Some(_)) => return Ordering::Less,
            (None, None) => {}
        }
    }

    let name_a = a.file_name().and_then(|f| f.to_str()).unwrap_or("");
    let name_b = b.file_name().and_then(|f| f.to_str()).unwrap_or("");

    let cmp = natural_str_cmp(name_a, name_b);
    if cmp != Ordering::Equal {
        return cmp;
    }

    // Tie-break: codepoint order of normalized full path
    a.as_os_str().cmp(b.as_os_str())
}

pub fn natural_str_cmp(a: &str, b: &str) -> Ordering {
    let mut rem_a = a;
    let mut rem_b = b;

    while !rem_a.is_empty() && !rem_b.is_empty() {
        let first_a = rem_a.chars().next().unwrap();
        let first_b = rem_b.chars().next().unwrap();
        let a_is_digit = first_a.is_ascii_digit();
        let b_is_digit = first_b.is_ascii_digit();

        match (a_is_digit, b_is_digit) {
            (true, false) => return Ordering::Less,
            (false, true) => return Ordering::Greater,
            (true, true) => {
                // Extract contiguous ascii digits
                let len_a = rem_a.bytes().take_while(|b| b.is_ascii_digit()).count();
                let len_b = rem_b.bytes().take_while(|b| b.is_ascii_digit()).count();
                let slice_a = &rem_a[..len_a];
                let slice_b = &rem_b[..len_b];
                rem_a = &rem_a[len_a..];
                rem_b = &rem_b[len_b..];

                // Numeric comparison:
                let nz_a = slice_a.trim_start_matches('0');
                let nz_b = slice_b.trim_start_matches('0');

                let len_cmp = nz_a.len().cmp(&nz_b.len());
                if len_cmp != Ordering::Equal {
                    return len_cmp;
                }

                let val_cmp = nz_a.cmp(nz_b);
                if val_cmp != Ordering::Equal {
                    return val_cmp;
                }

                // If numeric values are equal, shorter original length comes first (e.g. 3 < 03)
                let raw_len_cmp = slice_a.len().cmp(&slice_b.len());
                if raw_len_cmp != Ordering::Equal {
                    return raw_len_cmp;
                }
            }
            (false, false) => {
                // Extract contiguous non-digits
                let end_a = rem_a
                    .bytes()
                    .position(|b| b.is_ascii_digit())
                    .unwrap_or(rem_a.len());
                let end_b = rem_b
                    .bytes()
                    .position(|b| b.is_ascii_digit())
                    .unwrap_or(rem_b.len());
                let slice_a = &rem_a[..end_a];
                let slice_b = &rem_b[..end_b];
                rem_a = &rem_a[end_a..];
                rem_b = &rem_b[end_b..];

                // Fast path: if both are pure ASCII, compare bytes case-insensitively without heap allocations
                if slice_a.is_ascii() && slice_b.is_ascii() {
                    let mut b_it_a = slice_a.bytes().map(|b| b.to_ascii_lowercase());
                    let mut b_it_b = slice_b.bytes().map(|b| b.to_ascii_lowercase());
                    loop {
                        match (b_it_a.next(), b_it_b.next()) {
                            (Some(ca), Some(cb)) => {
                                let ord = ca.cmp(&cb);
                                if ord != Ordering::Equal {
                                    return ord;
                                }
                            }
                            (Some(_), None) => return Ordering::Greater,
                            (None, Some(_)) => return Ordering::Less,
                            (None, None) => break,
                        }
                    }
                } else {
                    let mut it_a = slice_a.chars().flat_map(|c| c.to_lowercase());
                    let mut it_b = slice_b.chars().flat_map(|c| c.to_lowercase());

                    loop {
                        match (it_a.next(), it_b.next()) {
                            (Some(ca), Some(cb)) => {
                                let ord = ca.cmp(&cb);
                                if ord != Ordering::Equal {
                                    return ord;
                                }
                            }
                            (Some(_), None) => return Ordering::Greater,
                            (None, Some(_)) => return Ordering::Less,
                            (None, None) => break,
                        }
                    }
                }
            }
        }
    }

    rem_a.len().cmp(&rem_b.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_natural_sort() {
        let mut list = vec![
            "img10.jpg",
            "img2.jpg",
            "IMG1.jpg",
            "img20.png",
            "img03.jpg",
            "img3.jpg",
        ];
        list.sort_by(|a, b| natural_str_cmp(a, b));

        assert_eq!(
            list,
            vec![
                "IMG1.jpg",
                "img2.jpg",
                "img3.jpg",
                "img03.jpg",
                "img10.jpg",
                "img20.png"
            ]
        );
    }

    #[test]
    fn test_supported_extensions() {
        assert!(is_supported_image(Path::new("a.jpg")));
        assert!(is_supported_image(Path::new("a.JPEG")));
        assert!(is_supported_image(Path::new("a.png")));
        assert!(is_supported_image(Path::new("a.bmp")));
        assert!(is_supported_image(Path::new("a.webp")));
        assert!(is_supported_image(Path::new("a.avif")));
        assert!(!is_supported_image(Path::new("a.gif")));
        assert!(!is_supported_image(Path::new("a.txt")));
    }

    #[test]
    fn test_subfolder_recursive_and_natural_cmp() {
        let temp_dir = std::env::temp_dir().join(format!("kabaper_test_{}", std::process::id()));
        let _ = fs::remove_dir_all(&temp_dir);
        fs::create_dir_all(temp_dir.join("sub1/sub2")).unwrap();
        fs::create_dir_all(temp_dir.join("sub10")).unwrap();

        fs::write(temp_dir.join("root.jpg"), b"fake").unwrap();
        fs::write(temp_dir.join("sub1/img1.png"), b"fake").unwrap();
        fs::write(temp_dir.join("sub1/sub2/deep.bmp"), b"fake").unwrap();
        fs::write(temp_dir.join("sub10/img2.jpg"), b"fake").unwrap();
        fs::write(temp_dir.join("sub1/ignored.txt"), b"fake").unwrap();

        let mut catalog = Catalog::new(HashSet::new());
        assert!(catalog.open_folder(&temp_dir, None));
        assert_eq!(catalog.items.len(), 4);

        let filenames: Vec<String> = catalog.items.iter().map(|it| it.filename.clone()).collect();
        // root should come first, then sub1, then sub1/sub2, then sub10
        assert_eq!(
            filenames,
            vec![
                "root.jpg".to_string(),
                format!("sub1{}img1.png", std::path::MAIN_SEPARATOR),
                format!(
                    "sub1{}sub2{}deep.bmp",
                    std::path::MAIN_SEPARATOR,
                    std::path::MAIN_SEPARATOR
                ),
                format!("sub10{}img2.jpg", std::path::MAIN_SEPARATOR),
            ]
        );

        let _ = fs::remove_dir_all(&temp_dir);
    }

    #[test]
    fn test_catalog_choose_random_candidate() {
        let mut catalog = Catalog::new(HashSet::new());
        catalog.items = vec![
            CatalogItem {
                path: PathBuf::from("img1.jpg"),
                filename: "img1.jpg".into(),
                is_excluded: false,
                failure_reason: None,
            },
            CatalogItem {
                path: PathBuf::from("img2.jpg"),
                filename: "img2.jpg".into(),
                is_excluded: false,
                failure_reason: None,
            },
            CatalogItem {
                path: PathBuf::from("img3.jpg"),
                filename: "img3.jpg".into(),
                is_excluded: true, // excluded
                failure_reason: None,
            },
            CatalogItem {
                path: PathBuf::from("img4.jpg"),
                filename: "img4.jpg".into(),
                is_excluded: false,
                failure_reason: Some("corrupt".into()),
            },
        ];
        catalog.current_index = Some(0);

        // When current is img1, random candidate should pick img2 (only other valid, non-excluded, non-failed)
        let empty_set = HashSet::new();
        let candidate = catalog.choose_random_candidate(Some(Path::new("img1.jpg")), &empty_set);
        assert_eq!(candidate, Some((1, PathBuf::from("img2.jpg"))));

        // When img2 is also in excluded_ids (e.g. used by another frame), candidate falls back
        let mut used = HashSet::new();
        used.insert(PathBuf::from("img2.jpg"));
        let candidate2 = catalog.choose_random_candidate(Some(Path::new("img1.jpg")), &used);
        // Valid priority 2 allows current_path if not in used
        assert_eq!(candidate2, Some((0, PathBuf::from("img1.jpg"))));
    }
}
