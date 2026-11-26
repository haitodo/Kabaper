use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, mpsc};

use crate::catalog::{Catalog, is_supported_image};

pub(super) enum LibraryRequest {
    Open(PathBuf),
    Restore(PathBuf, Option<PathBuf>),
    Reload(PathBuf, Option<PathBuf>),
}

pub(super) struct LoadedLibrary {
    pub catalog: Catalog,
    pub add_to_recent: bool,
    pub message: Option<String>,
}

type LoadResult = Result<LoadedLibrary, String>;

impl LibraryRequest {
    pub fn load(self, excluded: HashSet<PathBuf>, active: &dyn Fn() -> bool) -> LoadResult {
        let add_to_recent = matches!(self, Self::Open(_));
        let (folder, selection, message) = match self {
            Self::Open(path) if path.is_file() => {
                if !is_supported_image(&path) {
                    return Err(format!("非対応のファイル形式です: {}", path.display()));
                }
                let folder = path.parent().unwrap_or(&path).to_owned();
                (folder, Some(path), None)
            }
            Self::Open(path) => (path, None, None),
            Self::Restore(folder, selection) => (folder, selection, None),
            Self::Reload(folder, selection) => {
                (folder, selection, Some("フォルダを再読込しました".into()))
            }
        };
        let mut catalog = Catalog::new(excluded);
        if !catalog.open_folder_while(&folder, selection.as_deref(), active) {
            return Err(format!("フォルダを開けませんでした: {}", folder.display()));
        }
        Ok(LoadedLibrary {
            catalog,
            add_to_recent,
            message,
        })
    }
}

// At most one scan runs and one newer selection waits, even on a slow network share.
#[derive(Default)]
pub(super) struct LibraryLoader {
    running: Option<(Arc<AtomicBool>, mpsc::Receiver<LoadResult>)>,
    pending: Option<(LibraryRequest, HashSet<PathBuf>)>,
}

impl LibraryLoader {
    pub fn request(&mut self, request: LibraryRequest, excluded: HashSet<PathBuf>) {
        self.pending = Some((request, excluded));
        if let Some((cancel, _)) = &self.running {
            cancel.store(true, Ordering::Relaxed);
        } else {
            self.start_pending();
        }
    }

    pub fn busy(&self) -> bool {
        self.running.is_some()
    }

    fn start_pending(&mut self) {
        let Some((request, excluded)) = self.pending.take() else {
            return;
        };
        let cancel = Arc::new(AtomicBool::new(false));
        let worker_cancel = cancel.clone();
        let (tx, rx) = mpsc::channel();
        self.running = Some((cancel, rx));
        rayon::spawn(move || {
            let result = request.load(excluded, &|| !worker_cancel.load(Ordering::Relaxed));
            let _ = tx.send(result);
        });
    }

    pub fn poll(&mut self) -> Option<LoadResult> {
        let (_, rx) = self.running.as_ref()?;
        let result = match rx.try_recv() {
            Ok(result) => result,
            Err(mpsc::TryRecvError::Empty) => return None,
            Err(mpsc::TryRecvError::Disconnected) => Err("フォルダの読込が中断されました".into()),
        };
        self.running = None;
        if self.pending.is_some() {
            self.start_pending();
            return None;
        }
        Some(result)
    }
}

impl Drop for LibraryLoader {
    fn drop(&mut self) {
        if let Some((cancel, _)) = &self.running {
            cancel.store(true, Ordering::Relaxed);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    #[test]
    fn only_the_latest_open_is_delivered() {
        let folder = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("logo");
        let mut loader = LibraryLoader::default();
        loader.request(LibraryRequest::Open(folder.join("missing")), HashSet::new());
        loader.request(
            LibraryRequest::Open(folder.join("icon.ico")),
            HashSet::new(),
        );
        loader.request(
            LibraryRequest::Open(folder.join("icon.png")),
            HashSet::new(),
        );
        let start = Instant::now();
        let loaded = loop {
            if let Some(result) = loader.poll() {
                break result.unwrap();
            }
            assert!(start.elapsed() < Duration::from_secs(10));
            std::thread::sleep(Duration::from_millis(1));
        };
        assert!(!loader.busy());
        assert_eq!(loaded.catalog.current_item().unwrap().filename, "icon.png");
    }
}
