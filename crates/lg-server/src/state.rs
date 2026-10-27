//! Shared application state and startup.

use crate::engine::{Engine, Mode};
use crate::library::{Downloads, Library};
use crate::paths::Paths;
use crate::settings::Settings;
use lg_manifests::{Registry, Source};
use lg_store::Store;
use lg_types::{CoreEvent, HardwareInfo, JobSpec, Manifest, Origin};
use parking_lot::{Mutex, RwLock};
use serde_json::Value;
use std::collections::BTreeMap;
use std::sync::Arc;
use tokio::sync::{Notify, broadcast};
use tokio_util::sync::CancellationToken;

pub struct App {
    pub paths: Paths,
    pub store: Arc<Store>,
    pub registry: Arc<Registry>,
    pub engine: Arc<Engine>,
    pub library: Arc<Library>,
    pub downloads: Arc<Downloads>,
    /// File sizes learned from the download servers (manifests often leave them unknown).
    pub sizes: Arc<crate::sizes::RemoteSizes>,
    /// Hugging Face endpoint, or `None` when online lookups are off.
    pub hf_endpoint: Option<String>,
    pub hub: crate::hub::Hub,
    pub events: broadcast::Sender<CoreEvent>,
    pub hw: RwLock<HardwareInfo>,
    pub settings: RwLock<Settings>,
    pub monitor: lg_hwprobe::Monitor,
    pub wake: Notify,
    pub running: Mutex<Option<(String, CancellationToken)>>,
    pub last_model: Mutex<Option<String>>,
    pub last_graph: Mutex<Option<Value>>,
    engine_gen_seen: std::sync::atomic::AtomicU64,
    /// Per-launch secret the UI must present.
    pub token: String,
    _watcher: Mutex<Option<notify::RecommendedWatcher>>,
}

pub struct Options {
    pub root: std::path::PathBuf,
    pub mode: Mode,
    /// Where built-in manifests live (default: `<root>/manifests`).
    pub manifests: Option<std::path::PathBuf>,
    /// Where the built UI lives (default: `<root>/ui`).
    pub ui: Option<std::path::PathBuf>,
    pub token: Option<String>,
    /// Hugging Face endpoint for size lookups and the model browser. `None` = huggingface.co,
    /// `Some("off")` = never go online for these (tests), any other URL = a mirror or mock.
    pub hf_endpoint: Option<String>,
}

impl App {
    pub async fn start(opts: Options) -> anyhow::Result<Arc<Self>> {
        let mut paths = Paths::new(&opts.root);
        if let Some(m) = opts.manifests {
            paths.manifests = m;
        }
        if let Some(u) = opts.ui {
            paths.ui = u;
        }
        paths.create_all()?;
        let store = Arc::new(Store::open(&paths.db())?);
        let recovered = store.recover()?;
        if recovered > 0 {
            tracing::info!("requeued {recovered} job(s) left running by an earlier crash");
        }
        let settings = Settings::load(&store);
        let (events, _) = broadcast::channel(1024);
        let registry = Arc::new(Registry::new(vec![
            Source {
                dir: paths.manifests.join("models"),
                root: paths.manifests.clone(),
                origin: Origin::Builtin,
            },
            Source {
                dir: paths.tools(),
                root: paths.tools(),
                origin: Origin::Tool,
            },
            Source {
                dir: paths.user_manifests(),
                root: paths.user_manifests(),
                origin: Origin::User,
            },
        ]));
        let engine = Arc::new(Engine::new(opts.mode, paths.clone(), events.clone()));
        engine.set_extra_dirs(settings.extra_model_dirs.clone());
        let library = Arc::new(Library::default());
        let mut roots = vec![paths.models.clone()];
        roots.extend(
            settings
                .extra_model_dirs
                .iter()
                .map(std::path::PathBuf::from),
        );
        library.set_roots(roots);
        library.scan();
        let hw = lg_hwprobe::probe(&paths.root).await;
        let hf_endpoint = match opts.hf_endpoint.as_deref() {
            Some("off") => None,
            Some(u) => Some(u.trim_end_matches('/').to_string()),
            None => Some(crate::sizes::HF.to_string()),
        };
        let token = opts.token.unwrap_or_else(|| {
            use rand::Rng;
            let mut b = [0u8; 24];
            rand::rng().fill(&mut b);
            b.iter().map(|x| format!("{x:02x}")).collect()
        });
        let app = Arc::new(Self {
            downloads: Downloads::new(events.clone()),
            sizes: crate::sizes::RemoteSizes::load(
                paths.data.join("cache").join("file-sizes.json"),
            ),
            hf_endpoint,
            hub: crate::hub::Hub::new(),
            paths,
            store,
            registry,
            engine,
            library,
            events,
            hw: RwLock::new(hw),
            settings: RwLock::new(settings),
            monitor: lg_hwprobe::Monitor::start(),
            wake: Notify::new(),
            running: Mutex::new(None),
            last_model: Mutex::new(None),
            last_graph: Mutex::new(None),
            engine_gen_seen: std::sync::atomic::AtomicU64::new(0),
            token,
            _watcher: Mutex::new(None),
        });
        // Manifest folders hot-reload.
        let weak = Arc::downgrade(&app);
        let watcher = lg_manifests::watch(app.registry.source_dirs(), move || {
            if let Some(app) = weak.upgrade() {
                app.registry.reload();
                let _ = app.events.send(CoreEvent::ManifestsChanged);
            }
        });
        *app._watcher.lock() = watcher;
        tokio::spawn(crate::jobs::worker(app.clone()));
        tokio::spawn(hardware_ticker(app.clone()));
        tokio::spawn(size_refresher(app.clone()));
        let a = app.clone();
        tokio::spawn(async move {
            let npu = lg_hwprobe::probe_npu().await;
            a.hw.write().npu = npu;
        });
        Ok(app)
    }

    /// After the engine starts: share its node definitions and turn its blueprints into tools.
    pub fn on_engine_ready(&self) {
        let Some(info) = self.engine.object_info() else {
            return;
        };
        self.registry.set_object_info(info.clone());
        if let Some(dir) = self
            .engine
            .comfy_dir()
            .map(|d| d.join("blueprints"))
            .filter(|d| d.is_dir())
        {
            let (tools, failed) = lg_manifests::blueprint_tools(&dir, &info);
            for (name, e) in &failed {
                tracing::debug!("blueprint {name}: {e}");
            }
            tracing::info!(
                "{} official blueprint tools ({} unavailable)",
                tools.len(),
                failed.len()
            );
            self.registry.set_generated(tools);
            self.registry.set_object_info(info);
        }
        let _ = self.events.send(CoreEvent::ManifestsChanged);
    }

    /// Start (or reuse) the engine, refreshing node definitions and tools after a restart.
    pub async fn ensure_engine(
        &self,
        flags: &[String],
    ) -> Result<lg_engine::comfy::ComfyClient, String> {
        use std::sync::atomic::Ordering;
        let c = self.engine.ensure(flags).await?;
        let generation = self.engine.generation.load(Ordering::SeqCst);
        if self.engine_gen_seen.swap(generation, Ordering::SeqCst) != generation {
            self.on_engine_ready();
            // GPUs nvidia-smi cannot see (AMD, Intel) come from the engine.
            if self.hw.read().gpus.is_empty()
                && let Ok(stats) = c.system_stats().await
            {
                let gpus: Vec<lg_types::GpuInfo> = stats["devices"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .enumerate()
                    .filter(|(_, d)| d["type"].as_str() != Some("cpu"))
                    .map(|(i, d)| lg_types::GpuInfo {
                        index: i as u32,
                        name: d["name"].as_str().unwrap_or("GPU").to_string(),
                        vendor: d["type"].as_str().unwrap_or("unknown").to_string(),
                        vram_total_gb: d["vram_total"].as_f64().unwrap_or(0.0) / 1073741824.0,
                        vram_free_gb: d["vram_free"].as_f64().unwrap_or(0.0) / 1073741824.0,
                        ..Default::default()
                    })
                    .collect();
                self.hw.write().gpus = gpus;
            }
        }
        Ok(c)
    }

    pub fn running_job(&self) -> Option<String> {
        self.running.lock().as_ref().map(|(id, _)| id.clone())
    }

    /// Cancel a job: queued jobs are marked cancelled, the running one is interrupted.
    pub fn cancel(&self, id: &str) -> bool {
        if let Some((rid, t)) = self.running.lock().as_ref()
            && rid == id
        {
            t.cancel();
            return true;
        }
        match self.store.job(id) {
            Ok(Some(j)) if j.status == lg_types::JobStatus::Queued => {
                let _ = self
                    .store
                    .set_status(id, lg_types::JobStatus::Cancelled, None);
                if let Ok(Some(job)) = self.store.job(id) {
                    let _ = self
                        .events
                        .send(CoreEvent::JobUpdated { job: Box::new(job) });
                }
                true
            }
            _ => false,
        }
    }
}

/// Plan a job with the current hardware, settings and history.
pub fn plan_for(
    app: &App,
    m: &Manifest,
    spec: &JobSpec,
    values: &BTreeMap<String, Value>,
    escalate: u8,
) -> lg_types::Plan {
    let hw = app.hw.read().clone();
    let settings = app.settings.read().planner.clone();
    let history = app.store.benchmarks(Some(&m.id), 50).unwrap_or_default();
    let on_disk = app.library.on_disk();
    let force_fit = match spec.memory.plan.as_deref() {
        Some("resident") => Some(lg_types::Fit::Resident),
        Some("offload") => Some(lg_types::Fit::Offload),
        Some("stream") => Some(lg_types::Fit::Stream),
        _ => None,
    };
    let force_precision = spec
        .memory
        .precision
        .as_deref()
        .and_then(|p| serde_json::from_value(Value::String(p.into())).ok());
    let m = &*app.sizes.apply(m);
    lg_planner::plan(&lg_planner::Input {
        hw: &hw,
        manifest: m,
        op: &spec.op,
        values,
        on_disk: &on_disk,
        history: &history,
        settings: &settings,
        force_fit,
        force_precision,
        force_vae_tiled: spec.memory.vae_tiled,
        escalate,
    })
}

/// Learn unknown download sizes at startup and whenever the model list changes (new
/// manifests, official workflows after an engine start). Offline is fine: they stay unknown.
async fn size_refresher(app: Arc<App>) {
    let Some(endpoint) = app.hf_endpoint.clone() else {
        return;
    };
    let mut rx = app.events.subscribe();
    loop {
        let urls = crate::sizes::unknown_urls(app.registry.list().into_iter().map(|e| e.manifest));
        let token = Some(app.settings.read().hf_token.clone()).filter(|t| !t.is_empty());
        if app.sizes.refresh(urls, token, &endpoint).await > 0 {
            let _ = app.events.send(CoreEvent::ManifestsChanged);
        }
        // Wait for the next change, then let a burst of changes settle.
        loop {
            match rx.recv().await {
                Ok(CoreEvent::ManifestsChanged) => break,
                Ok(_) => continue,
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => break,
                Err(_) => return,
            }
        }
        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
        while rx.try_recv().is_ok() {}
    }
}

/// Live GPU/RAM gauges for the UI, every two seconds.
async fn hardware_ticker(app: Arc<App>) {
    let mut tick = tokio::time::interval(std::time::Duration::from_secs(2));
    loop {
        tick.tick().await;
        if app.events.receiver_count() == 0 {
            continue;
        }
        let s = app.monitor.sample();
        let hw = app.hw.read();
        let _ = app.events.send(CoreEvent::Hardware {
            vram_used_gb: s.vram_used_gb.unwrap_or(0.0),
            vram_total_gb: s
                .vram_total_gb
                .or(hw.primary_gpu().map(|g| g.vram_total_gb))
                .unwrap_or(0.0),
            ram_used_gb: s.ram_used_gb,
            ram_total_gb: s.ram_total_gb.max(hw.ram_total_gb),
            shared_gpu_gb: s.shared_gpu_gb,
            gpu_util: s.gpu_util,
        });
    }
}
