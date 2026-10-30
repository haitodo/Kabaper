use std::cmp::Ordering;
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

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
}

impl Catalog {
    pub fn new(excluded_paths: HashSet<PathBuf>) -> Self {
        Self {
            folder: None,
            items: Vec::new(),
            current_index: None,
            excluded_paths,
            failed_decodes: HashMap::new(),
        }
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

        // Sort with natural sort order
        found_paths.sort_by(|a, b| natural_cmp(a, b));

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
            return;
        }

        // Selection logic:
        // 1. If target_to_select is specified, try to find it.
        // 2. If deleted, select next image in natural order (or first if was at end).
        // 3. Otherwise select first (index 0).
        if let Some(target) = target_to_select {
            let target_norm = normalize_path(target);
            if let Some(idx) = self.items.iter().position(|it| it.path == target_norm) {
                self.current_index = Some(idx);
                return;
            } else {
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
        let norm = normalize_path(path);
        if let Some(idx) = self.items.iter().position(|it| it.path == norm) {
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

        for item in &mut self.items {
            if item.path == norm {
                item.is_excluded = is_now_excluded;
            }
        }

        is_now_excluded
    }

    pub fn mark_failed(&mut self, path: &Path, reason: String) {
        let norm = normalize_path(path);
        self.failed_decodes.insert(norm.clone(), reason.clone());
        for item in &mut self.items {
            if item.path == norm {
                item.failure_reason = Some(reason.clone());
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
}

pub fn collect_images_recursive(root: &Path) -> Vec<PathBuf> {
    let mut images = Vec::new();
    let mut dirs_to_visit = vec![root.to_path_buf()];
    let mut visited_dirs = HashSet::new();

    let norm_root = normalize_path(root);
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
                let norm = normalize_path(&path);
                if visited_dirs.insert(norm) {
                    dirs_to_visit.push(path);
                }
            } else if file_type.is_file() && is_supported_image(&path) {
                images.push(normalize_path(&path));
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
        "jpg" | "jpeg" | "png" | "bmp"
    )
}

pub fn normalize_path(path: &Path) -> PathBuf {
    // Canonicalize or absolute path normalization
    if let Ok(canon) = fs::canonicalize(path) {
        // Strip Windows UNC prefix \\?\ if present for clean paths
        let s = canon.to_string_lossy();
        if let Some(stripped) = s.strip_prefix(r"\\?\") {
            PathBuf::from(stripped)
        } else {
            canon
        }
    } else {
        if path.is_absolute() {
            path.to_path_buf()
        } else if let Ok(cur) = std::env::current_dir() {
            cur.join(path)
        } else {
            path.to_path_buf()
        }
    }
}

pub fn natural_cmp(a: &Path, b: &Path) -> Ordering {
    let parent_a: Vec<_> = a
        .parent()
        .map(|p| p.components().collect())
        .unwrap_or_default();
    let parent_b: Vec<_> = b
        .parent()
        .map(|p| p.components().collect())
        .unwrap_or_default();

    for (ca, cb) in parent_a.iter().zip(parent_b.iter()) {
        let sa = ca.as_os_str().to_string_lossy();
        let sb = cb.as_os_str().to_string_lossy();
        let ord = natural_str_cmp(&sa, &sb);
        if ord != Ordering::Equal {
            return ord;
        }
    }

    let depth_ord = parent_a.len().cmp(&parent_b.len());
    if depth_ord != Ordering::Equal {
        return depth_ord;
    }

    let name_a = a.file_name().and_then(|f| f.to_str()).unwrap_or("");
    let name_b = b.file_name().and_then(|f| f.to_str()).unwrap_or("");

    let cmp = natural_str_cmp(name_a, name_b);
    if cmp != Ordering::Equal {
        return cmp;
    }

    // Tie-break: codepoint order of normalized full path
    a.to_string_lossy().cmp(&b.to_string_lossy())
}

enum Chunk<'a> {
    Digits(&'a str),
    NonDigits(&'a str),
}

fn chunk_str(s: &str) -> Vec<Chunk<'_>> {
    let mut chunks = Vec::new();
    let mut chars = s.char_indices().peekable();

    while let Some(&(start, c)) = chars.peek() {
        let is_digit = c.is_ascii_digit();
        let mut end = start + c.len_utf8();
        chars.next();

        while let Some(&(next_start, next_c)) = chars.peek() {
            if next_c.is_ascii_digit() == is_digit {
                end = next_start + next_c.len_utf8();
                chars.next();
            } else {
                break;
            }
        }

        let slice = &s[start..end];
        if is_digit {
            chunks.push(Chunk::Digits(slice));
        } else {
            chunks.push(Chunk::NonDigits(slice));
        }
    }

    chunks
}

pub fn natural_str_cmp(a: &str, b: &str) -> Ordering {
    let chunks_a = chunk_str(a);
    let chunks_b = chunk_str(b);

    let mut i = 0;
    while i < chunks_a.len() && i < chunks_b.len() {
        match (&chunks_a[i], &chunks_b[i]) {
            (Chunk::Digits(da), Chunk::Digits(db)) => {
                // Parse numeric value or compare by value
                let num_a = da.parse::<u128>().ok();
                let num_b = db.parse::<u128>().ok();
                match (num_a, num_b) {
                    (Some(na), Some(nb)) => {
                        let ord = na.cmp(&nb);
                        if ord != Ordering::Equal {
                            return ord;
                        }
                        // If values are equal (e.g. 01 vs 1), shorter length first
                        let len_ord = da.len().cmp(&db.len());
                        if len_ord != Ordering::Equal {
                            return len_ord;
                        }
                    }
                    _ => {
                        // Fallback string compare
                        let ord = da.cmp(db);
                        if ord != Ordering::Equal {
                            return ord;
                        }
                    }
                }
            }
            (Chunk::NonDigits(na), Chunk::NonDigits(nb)) => {
                let ord = na.to_lowercase().cmp(&nb.to_lowercase());
                if ord != Ordering::Equal {
                    return ord;
                }
            }
            (Chunk::Digits(_), Chunk::NonDigits(_)) => {
                return Ordering::Less;
            }
            (Chunk::NonDigits(_), Chunk::Digits(_)) => {
                return Ordering::Greater;
            }
        }
        i += 1;
    }

    chunks_a.len().cmp(&chunks_b.len())
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
        assert!(!is_supported_image(Path::new("a.webp")));
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
}
