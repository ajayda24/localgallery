//! Model files on disk (the app's `models/` plus extra folders), and downloads.

use lg_modelscan::ModelInfo;
use lg_types::CoreEvent;
use parking_lot::{Mutex, RwLock};
use serde::Serialize;
use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::SystemTime;
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;

#[derive(Debug, Clone, Serialize)]
pub struct LibraryFile {
    pub folder: String,
    /// Name as ComfyUI lists it (relative to the folder, forward slashes).
    pub name: String,
    pub root: String,
    pub size_bytes: u64,
    pub info: Option<ModelInfo>,
}

#[derive(Default)]
pub struct Library {
    roots: RwLock<Vec<PathBuf>>,
    files: RwLock<Vec<LibraryFile>>,
    /// (path, size, mtime) -> header info, so rescans are cheap.
    cache: Mutex<HashMap<(PathBuf, u64, SystemTime), Option<ModelInfo>>>,
}

impl Library {
    pub fn set_roots(&self, roots: Vec<PathBuf>) {
        *self.roots.write() = roots;
    }

    /// Rescan every root. Headers are read only for new or changed files.
    pub fn scan(&self) -> usize {
        let roots = self.roots.read().clone();
        let mut out = Vec::new();
        for root in &roots {
            // Extra folders may be a ComfyUI folder with models/ inside.
            let base = if root.join("models").is_dir() {
                root.join("models")
            } else {
                root.clone()
            };
            for (folder, rel) in lg_modelscan::list_files(&base) {
                let path = base.join(&folder).join(&rel);
                let Ok(meta) = std::fs::metadata(&path) else {
                    continue;
                };
                let key = (
                    path.clone(),
                    meta.len(),
                    meta.modified().unwrap_or(SystemTime::UNIX_EPOCH),
                );
                let info = {
                    let mut cache = self.cache.lock();
                    cache
                        .entry(key)
                        .or_insert_with(|| lg_modelscan::inspect(&path).ok())
                        .clone()
                };
                // ComfyUI treats unet/ as diffusion_models and clip/ as text_encoders.
                let folder = match folder.as_str() {
                    "unet" => "diffusion_models".to_string(),
                    "clip" => "text_encoders".to_string(),
                    f => f.to_string(),
                };
                out.push(LibraryFile {
                    folder,
                    name: rel.to_string_lossy().replace('\\', "/"),
                    root: base.display().to_string(),
                    size_bytes: meta.len(),
                    info,
                });
            }
        }
        out.sort_by(|a, b| (&a.folder, &a.name).cmp(&(&b.folder, &b.name)));
        let n = out.len();
        *self.files.write() = out;
        n
    }

    pub fn files(&self) -> Vec<LibraryFile> {
        self.files.read().clone()
    }

    /// Size on disk of each present file, by name (any folder).
    pub fn on_disk(&self) -> BTreeMap<String, u64> {
        self.files
            .read()
            .iter()
            .map(|f| (f.name.clone(), f.size_bytes))
            .collect()
    }

    pub fn has(&self, folder: &str, name: &str) -> bool {
        self.files
            .read()
            .iter()
            .any(|f| f.folder == folder && f.name == name)
    }

    pub fn names_in(&self, folder: &str) -> Vec<String> {
        self.files
            .read()
            .iter()
            .filter(|f| f.folder == folder)
            .map(|f| f.name.clone())
            .collect()
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct Download {
    pub id: String,
    pub name: String,
    pub url: String,
    pub dest: String,
    pub received: u64,
    pub total: Option<u64>,
    pub state: String,
    pub error: Option<String>,
}

/// Downloads run one at a time (they share the connection) and survive pause/resume.
pub struct Downloads {
    items: RwLock<Vec<Download>>,
    tokens: Mutex<HashMap<String, CancellationToken>>,
    events: broadcast::Sender<CoreEvent>,
    gate: tokio::sync::Semaphore,
    http: reqwest::Client,
}

impl Downloads {
    pub fn new(events: broadcast::Sender<CoreEvent>) -> Arc<Self> {
        Arc::new(Self {
            items: RwLock::new(vec![]),
            tokens: Mutex::new(HashMap::new()),
            events,
            gate: tokio::sync::Semaphore::new(1),
            http: lg_downloader::client(),
        })
    }

    pub fn list(&self) -> Vec<Download> {
        self.items.read().clone()
    }

    fn update(&self, id: &str, f: impl FnOnce(&mut Download)) {
        let snapshot = {
            let mut items = self.items.write();
            let Some(d) = items.iter_mut().find(|d| d.id == id) else {
                return;
            };
            f(d);
            d.clone()
        };
        let _ = self.events.send(CoreEvent::Download {
            id: snapshot.id,
            name: snapshot.name,
            received: snapshot.received,
            total: snapshot.total,
            state: snapshot.state,
            error: snapshot.error,
        });
    }

    /// Queue a download unless the same destination is already queued. Returns its id.
    pub fn start(
        self: &Arc<Self>,
        url: String,
        dest: PathBuf,
        sha256: Option<String>,
        hf_token: Option<String>,
        on_done: impl FnOnce(bool) + Send + 'static,
    ) -> String {
        let dest_s = dest.display().to_string();
        if let Some(d) = self
            .items
            .read()
            .iter()
            .find(|d| d.dest == dest_s && matches!(d.state.as_str(), "queued" | "running"))
        {
            return d.id.clone();
        }
        let id = uuid::Uuid::new_v4().simple().to_string()[..12].to_string();
        let name = dest
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        self.items.write().push(Download {
            id: id.clone(),
            name,
            url: url.clone(),
            dest: dest_s,
            received: 0,
            total: None,
            state: "queued".into(),
            error: None,
        });
        let token = CancellationToken::new();
        self.tokens.lock().insert(id.clone(), token.clone());
        let me = self.clone();
        let id2 = id.clone();
        tokio::spawn(async move {
            let _permit = me.gate.acquire().await;
            if token.is_cancelled() {
                me.update(&id2, |d| d.state = "paused".into());
                on_done(false);
                return;
            }
            me.update(&id2, |d| d.state = "running".into());
            let req = lg_downloader::Request {
                url,
                dest,
                sha256,
                hf_token,
            };
            let mut last = std::time::Instant::now();
            let r = lg_downloader::download(&me.http, &req, &token, |p| {
                if last.elapsed().as_millis() > 400 {
                    last = std::time::Instant::now();
                    me.update(&id2, |d| {
                        d.received = p.received;
                        d.total = p.total;
                    });
                }
            })
            .await;
            match r {
                Ok(_) => {
                    me.update(&id2, |d| {
                        d.state = "done".into();
                        d.total = d.total.or(Some(d.received));
                        d.received = d.total.unwrap_or(d.received);
                    });
                    on_done(true);
                }
                Err(lg_downloader::DownloadError::Cancelled) => {
                    me.update(&id2, |d| d.state = "paused".into());
                    on_done(false);
                }
                Err(e) => {
                    me.update(&id2, |d| {
                        d.state = "failed".into();
                        d.error = Some(e.to_string());
                    });
                    on_done(false);
                }
            }
        });
        id
    }

    /// Pause keeps the partial file; starting again resumes it.
    pub fn pause(&self, id: &str) -> bool {
        match self.tokens.lock().get(id) {
            Some(t) => {
                t.cancel();
                true
            }
            None => false,
        }
    }

    pub fn remove(&self, id: &str) {
        self.pause(id);
        let dest = self
            .items
            .read()
            .iter()
            .find(|d| d.id == id)
            .map(|d| d.dest.clone());
        self.items.write().retain(|d| d.id != id);
        if let Some(d) = dest {
            let _ = std::fs::remove_file(lg_downloader::part_path(Path::new(&d)));
        }
    }
}
