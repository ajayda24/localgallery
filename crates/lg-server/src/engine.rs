//! Owns the ComfyUI engine: install, start/restart with the planner's flags, health,
//! node definitions, and turning its blueprints into tools.

use crate::paths::Paths;
use lg_engine::comfy::ComfyClient;
use lg_engine::install::{InstallOptions, Layout};
use lg_engine::supervisor::{LaunchConfig, Supervisor, free_port};
use lg_types::{CoreEvent, EngineState};
use parking_lot::RwLock;
use serde_json::Value;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;

#[derive(Debug, Clone)]
pub enum Mode {
    /// The app's own portable ComfyUI in `engines/full`.
    Managed,
    /// A ComfyUI the user already runs (e.g. their own portable install).
    External(String),
    /// The in-process fake engine (development, tests, machines without a GPU).
    Fake {
        /// Node definition files, merged in order. None = the built-in minimal set.
        object_info: Vec<PathBuf>,
        step_ms: u64,
    },
}

pub struct Engine {
    pub mode: Mode,
    paths: Paths,
    events: broadcast::Sender<CoreEvent>,
    state: RwLock<(EngineState, String)>,
    client: RwLock<Option<ComfyClient>>,
    object_info: RwLock<Option<Arc<Value>>>,
    supervisor: Supervisor,
    flags: RwLock<Vec<String>>,
    start_lock: tokio::sync::Mutex<()>,
    install_cancel: parking_lot::Mutex<Option<CancellationToken>>,
    extra_dirs: RwLock<Vec<String>>,
    /// Extensions were checked (and added if missing) this session.
    addons_checked: std::sync::atomic::AtomicBool,
    /// Bumped on every successful (re)start.
    pub generation: std::sync::atomic::AtomicU64,
}

impl Engine {
    pub fn new(mode: Mode, paths: Paths, events: broadcast::Sender<CoreEvent>) -> Self {
        let installed = Layout::new(&paths.root).is_installed();
        let initial = match (&mode, installed) {
            (Mode::Managed, false) => (
                EngineState::NotInstalled,
                "The image engine is not installed yet.".into(),
            ),
            _ => (EngineState::Stopped, String::new()),
        };
        Self {
            mode,
            paths,
            events,
            state: RwLock::new(initial),
            client: RwLock::new(None),
            object_info: RwLock::new(None),
            supervisor: Supervisor::new(),
            flags: RwLock::new(vec![]),
            start_lock: tokio::sync::Mutex::new(()),
            install_cancel: parking_lot::Mutex::new(None),
            extra_dirs: RwLock::new(vec![]),
            addons_checked: std::sync::atomic::AtomicBool::new(false),
            generation: std::sync::atomic::AtomicU64::new(0),
        }
    }

    pub fn status(&self) -> (EngineState, String) {
        self.state.read().clone()
    }

    fn set_state(&self, s: EngineState, detail: impl Into<String>) {
        let detail = detail.into();
        *self.state.write() = (s, detail.clone());
        let _ = self
            .events
            .send(CoreEvent::EngineStatus { status: s, detail });
    }

    pub fn object_info(&self) -> Option<Arc<Value>> {
        self.object_info.read().clone()
    }

    pub fn client(&self) -> Option<ComfyClient> {
        self.client.read().clone()
    }

    pub fn set_extra_dirs(&self, dirs: Vec<String>) {
        *self.extra_dirs.write() = dirs;
    }

    pub fn running_flags(&self) -> Vec<String> {
        self.flags.read().clone()
    }

    pub fn layout(&self) -> Layout {
        Layout::new(&self.paths.root)
    }

    /// The ComfyUI checkout, when the app manages it (for blueprints).
    pub fn comfy_dir(&self) -> Option<PathBuf> {
        match self.mode {
            Mode::Managed => Some(self.layout().comfy_dir()),
            _ => std::env::var_os("LG_COMFY_DIR").map(PathBuf::from),
        }
    }

    /// Make sure the engine runs with `flags`; restarts it only when they changed.
    pub async fn ensure(&self, flags: &[String]) -> Result<ComfyClient, String> {
        let _g = self.start_lock.lock().await;
        if let Some(c) = self.client()
            && (matches!(self.mode, Mode::External(_) | Mode::Fake { .. })
                || !lg_planner::restart_needed(&self.running_flags(), flags))
            && c.system_stats().await.is_ok()
        {
            return Ok(c);
        }
        self.set_state(EngineState::Starting, "Starting the image engine...");
        let result = match &self.mode {
            Mode::Fake {
                object_info,
                step_ms,
            } => self.start_fake(object_info.clone(), *step_ms).await,
            Mode::External(url) => {
                let c = ComfyClient::new(url.clone());
                c.system_stats()
                    .await
                    .map(|_| c)
                    .map_err(|e| format!("cannot reach ComfyUI at {url}: {e}"))
            }
            Mode::Managed => self.start_managed(flags).await,
        };
        match result {
            Ok(c) => {
                *self.client.write() = Some(c.clone());
                *self.flags.write() = flags.to_vec();
                match c.object_info().await {
                    Ok(info) => *self.object_info.write() = Some(Arc::new(info)),
                    Err(e) => tracing::warn!("object_info: {e}"),
                }
                self.generation
                    .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                self.set_state(EngineState::Ready, "Ready");
                Ok(c)
            }
            Err(e) => {
                *self.client.write() = None;
                self.set_state(
                    if matches!(self.mode, Mode::Managed) && !self.layout().is_installed() {
                        EngineState::NotInstalled
                    } else {
                        EngineState::Error
                    },
                    e.clone(),
                );
                Err(e)
            }
        }
    }

    async fn start_fake(
        &self,
        object_info: Vec<PathBuf>,
        step_ms: u64,
    ) -> Result<ComfyClient, String> {
        let mut cfg = lg_fakecomfy::Config::new(self.paths.engine_data().join("fake"));
        cfg.step_ms = step_ms;
        if !object_info.is_empty() {
            let mut all = serde_json::Map::new();
            for p in &object_info {
                let text =
                    std::fs::read_to_string(p).map_err(|e| format!("{}: {e}", p.display()))?;
                let nodes: serde_json::Map<String, Value> =
                    serde_json::from_str(&text).map_err(|e| format!("{}: {e}", p.display()))?;
                // Keys starting with "_" are notes in hand-written fixtures, not nodes.
                all.extend(nodes.into_iter().filter(|(k, _)| !k.starts_with('_')));
            }
            cfg.object_info = Value::Object(all);
        }
        let (addr, _) = lg_fakecomfy::serve(cfg, ([127, 0, 0, 1], 0).into())
            .await
            .map_err(|e| e.to_string())?;
        Ok(ComfyClient::new(format!("http://{addr}")))
    }

    async fn start_managed(&self, flags: &[String]) -> Result<ComfyClient, String> {
        let layout = self.layout();
        if !layout.is_installed() {
            return Err(
                "The image engine is not installed yet. Open Settings > Engine to install it."
                    .into(),
            );
        }
        self.ensure_addons(&layout).await;
        let ed = self.paths.engine_data();
        let extra = self.write_extra_model_paths()?;
        let mut all_flags = vec![
            "--models-directory".to_string(),
            self.paths.models.to_string_lossy().into_owned(),
        ];
        all_flags.extend(flags.iter().cloned());
        let cfg = LaunchConfig {
            python: layout.venv_python(),
            comfy_dir: layout.comfy_dir(),
            base_dir: layout.full().join("base"),
            output_dir: ed.join("output"),
            input_dir: ed.join("input"),
            temp_dir: self.paths.data.join("tmp").join("engine"),
            user_dir: ed.join("user"),
            extra_model_paths: extra,
            port: free_port(),
            flags: all_flags,
            env: layout.child_env(),
            log_file: self.paths.logs().join("engine.log"),
        };
        let events = self.events.clone();
        let sink: lg_engine::supervisor::LogSink = Arc::new(move |line: String| {
            let _ = events.send(CoreEvent::Log {
                source: "engine".into(),
                line: strip_ansi(&line),
            });
        });
        self.supervisor
            .start(cfg, sink, Duration::from_secs(300))
            .await
            .map_err(|e| e.to_string())
    }

    /// `extra_model_paths.yaml` for model folders outside the app.
    fn write_extra_model_paths(&self) -> Result<Option<PathBuf>, String> {
        let dirs = self.extra_dirs.read().clone();
        if dirs.is_empty() {
            return Ok(None);
        }
        let mut yaml = String::new();
        for (i, d) in dirs.iter().enumerate() {
            let p = PathBuf::from(d);
            // Accept either a ComfyUI folder (with models/) or a models folder itself.
            let base = if p.join("models").is_dir() {
                p.join("models")
            } else {
                p
            };
            let q = base.to_string_lossy().replace('\'', "''");
            yaml.push_str(&format!("extra_{i}:\n  base_path: '{q}'\n"));
            for (key, folders) in [
                ("checkpoints", "checkpoints"),
                ("diffusion_models", "diffusion_models\n    unet"),
                ("text_encoders", "text_encoders\n    clip"),
                ("vae", "vae"),
                ("loras", "loras"),
                ("controlnet", "controlnet"),
                ("upscale_models", "upscale_models"),
                ("clip_vision", "clip_vision"),
                ("model_patches", "model_patches"),
                ("latent_upscale_models", "latent_upscale_models"),
                ("embeddings", "embeddings"),
            ] {
                yaml.push_str(&format!("  {key}: |\n    {folders}\n"));
            }
        }
        let path = self.paths.engine_data().join("extra_model_paths.yaml");
        std::fs::create_dir_all(self.paths.engine_data()).map_err(|e| e.to_string())?;
        std::fs::write(&path, yaml).map_err(|e| e.to_string())?;
        Ok(Some(path))
    }

    pub async fn stop(&self) {
        self.supervisor.stop().await;
        *self.client.write() = None;
        if !matches!(self.state.read().0, EngineState::NotInstalled) {
            self.set_state(EngineState::Stopped, "Stopped");
        }
    }

    fn read_lock(&self) -> Result<lg_engine::install::Lock, String> {
        let lock_path = self.paths.root.join("engines.lock.json");
        let lock_path = if lock_path.exists() {
            lock_path
        } else {
            self.paths.manifests.join("../engines.lock.json")
        };
        lg_engine::install::read_lock(&lock_path)
    }

    /// Engines installed by an older app version may lack an extension the lock now lists
    /// (e.g. GGUF support). Add it before starting; on failure (offline...) start without it
    /// and say so in the log. Checked once per session.
    async fn ensure_addons(&self, layout: &Layout) {
        if self
            .addons_checked
            .swap(true, std::sync::atomic::Ordering::SeqCst)
        {
            return;
        }
        let log = |line: String| {
            let _ = self.events.send(CoreEvent::Log {
                source: "install".into(),
                line,
            });
        };
        let Ok(lock) = self.read_lock() else { return };
        let opts = InstallOptions {
            torch_profile: String::new(),
            uv: None,
            comfy_source: None,
            torch_index: None,
            addon_sources: Default::default(),
        };
        let steps = match lg_engine::install::addons_plan(&lock, layout, &opts) {
            Ok(s) if s.is_empty() => return,
            Ok(s) => s,
            Err(e) => {
                log(format!("Engine extensions not updated: {e}"));
                return;
            }
        };
        self.set_state(EngineState::Installing, "Adding GGUF support...");
        let events = self.events.clone();
        let progress = move |p: lg_engine::install::InstallProgress| {
            if let Some(line) = &p.line {
                let _ = events.send(CoreEvent::Log {
                    source: "install".into(),
                    line: strip_ansi(line),
                });
            }
        };
        if let Err(e) =
            lg_engine::install::run(&steps, layout, false, &CancellationToken::new(), &progress)
                .await
        {
            log(format!(
                "Could not add engine extensions ({e}). The engine starts without them; GGUF model files will not load until this works."
            ));
        }
        self.set_state(EngineState::Starting, "Starting...");
    }

    /// Install the managed engine in the background, reporting progress as events.
    pub fn install(self: &Arc<Self>, opts: InstallOptions) -> Result<(), String> {
        if !matches!(self.mode, Mode::Managed) {
            return Err("this app uses an external engine".into());
        }
        let lock = self.read_lock()?;
        let layout = self.layout();
        let steps = lg_engine::install::plan(&lock, &layout, &opts)?;
        let cancel = CancellationToken::new();
        *self.install_cancel.lock() = Some(cancel.clone());
        let me = self.clone();
        let want_cuda = opts.torch_profile == "cuda";
        self.set_state(EngineState::Installing, "Preparing...");
        tokio::spawn(async move {
            let events = me.events.clone();
            let progress = move |p: lg_engine::install::InstallProgress| {
                let detail = match (p.received, p.total) {
                    (Some(r), Some(t)) if t > 0 => format!(
                        "{} ({}/{}) {:.0}%",
                        p.label,
                        p.step + 1,
                        p.steps,
                        r as f64 * 100.0 / t as f64
                    ),
                    _ => format!("{} ({}/{})", p.label, p.step + 1, p.steps),
                };
                if let Some(line) = &p.line {
                    let _ = events.send(CoreEvent::Log {
                        source: "install".into(),
                        line: strip_ansi(line),
                    });
                } else {
                    let _ = events.send(CoreEvent::EngineStatus {
                        status: EngineState::Installing,
                        detail,
                    });
                }
            };
            match lg_engine::install::run(&steps, &layout, want_cuda, &cancel, &progress).await {
                Ok(test) => {
                    let detail = test
                        .map(|t| {
                            format!(
                                "Installed (PyTorch {}, {})",
                                t.torch,
                                t.device.unwrap_or_else(|| "CPU".into())
                            )
                        })
                        .unwrap_or_else(|| "Installed".into());
                    me.set_state(EngineState::Stopped, detail);
                }
                Err(e) => me.set_state(EngineState::NotInstalled, format!("Install failed: {e}")),
            }
        });
        Ok(())
    }

    pub fn cancel_install(&self) {
        if let Some(c) = self.install_cancel.lock().take() {
            c.cancel();
        }
    }

    /// Mark the engine broken so the next job restarts it.
    pub fn invalidate(&self) {
        *self.client.write() = None;
        *self.flags.write() = vec![];
    }
}

/// Remove terminal colour codes from engine output.
pub fn strip_ansi(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' {
            if chars.peek() == Some(&'[') {
                chars.next();
                for d in chars.by_ref() {
                    if d.is_ascii_alphabetic() {
                        break;
                    }
                }
            }
            continue;
        }
        out.push(c);
    }
    out
}

#[cfg(test)]
mod tests {
    #[test]
    fn strips_colours() {
        assert_eq!(
            super::strip_ansi("\u{1b}[32m[INFO]\u{1b}[0m got prompt"),
            "[INFO] got prompt"
        );
    }
}
