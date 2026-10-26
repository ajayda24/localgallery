//! Starts, watches and stops the ComfyUI process.
//!
//! On Windows the child is placed in a Job Object with KILL_ON_JOB_CLOSE, so it dies
//! with the app even on a crash, and it is started without a console window.

use crate::EngineError;
use crate::comfy::ComfyClient;
use parking_lot::Mutex;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, Command};

#[derive(Debug, Clone)]
pub struct LaunchConfig {
    pub python: PathBuf,
    /// ComfyUI checkout (contains main.py).
    pub comfy_dir: PathBuf,
    pub base_dir: PathBuf,
    pub output_dir: PathBuf,
    pub input_dir: PathBuf,
    pub temp_dir: PathBuf,
    pub user_dir: PathBuf,
    pub extra_model_paths: Option<PathBuf>,
    pub port: u16,
    /// Planner flags (memory, precision, attention...).
    pub flags: Vec<String>,
    pub env: Vec<(String, String)>,
    pub log_file: PathBuf,
}

impl LaunchConfig {
    pub fn args(&self) -> Vec<String> {
        let p = |p: &PathBuf| p.to_string_lossy().into_owned();
        let mut a = vec![
            "-s".into(), // ignore user site-packages: the venv is the only source of packages
            p(&self.comfy_dir.join("main.py")),
            "--listen".into(),
            "127.0.0.1".into(),
            "--port".into(),
            self.port.to_string(),
            "--base-directory".into(),
            p(&self.base_dir),
            "--output-directory".into(),
            p(&self.output_dir),
            "--input-directory".into(),
            p(&self.input_dir),
            "--temp-directory".into(),
            p(&self.temp_dir),
            "--user-directory".into(),
            p(&self.user_dir),
            "--disable-auto-launch".into(),
            "--disable-product-tips".into(),
            "--offline".into(),
            "--preview-method".into(),
            "auto".into(),
            "--log-stdout".into(),
        ];
        if let Some(extra) = &self.extra_model_paths {
            a.push("--extra-model-paths-config".into());
            a.push(p(extra));
        }
        a.extend(self.flags.iter().cloned());
        a
    }
}

pub type LogSink = Arc<dyn Fn(String) + Send + Sync>;

pub struct Supervisor {
    child: Mutex<Option<Child>>,
    #[cfg(windows)]
    job: Mutex<Option<win::JobHandle>>,
    config: Mutex<Option<LaunchConfig>>,
}

impl Default for Supervisor {
    fn default() -> Self {
        Self::new()
    }
}

impl Supervisor {
    pub fn new() -> Self {
        Self {
            child: Mutex::new(None),
            #[cfg(windows)]
            job: Mutex::new(None),
            config: Mutex::new(None),
        }
    }

    pub fn running_config(&self) -> Option<LaunchConfig> {
        self.config.lock().clone()
    }

    pub fn is_running(&self) -> bool {
        let mut c = self.child.lock();
        match c.as_mut() {
            Some(ch) => matches!(ch.try_wait(), Ok(None)),
            None => false,
        }
    }

    /// Spawn ComfyUI and wait until its HTTP server answers.
    pub async fn start(
        &self,
        cfg: LaunchConfig,
        log: LogSink,
        ready_timeout: Duration,
    ) -> Result<ComfyClient, EngineError> {
        self.stop().await;
        if let Some(parent) = cfg.log_file.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        // ComfyUI exits at startup if <base>/custom_nodes is missing.
        let base = [
            cfg.base_dir.join("custom_nodes"),
            cfg.base_dir.join("models"),
        ];
        for d in base.iter().chain([
            &cfg.output_dir,
            &cfg.input_dir,
            &cfg.temp_dir,
            &cfg.user_dir,
        ]) {
            let _ = std::fs::create_dir_all(d);
        }
        let mut cmd = Command::new(&cfg.python);
        cmd.args(cfg.args())
            .current_dir(&cfg.comfy_dir)
            .env("PYTHONUNBUFFERED", "1")
            .env("PYTHONNOUSERSITE", "1")
            .env("PYTHONIOENCODING", "utf-8")
            .env("COMFYUI_DISABLE_PRODUCT_TIPS", "1")
            .envs(cfg.env.iter().map(|(k, v)| (k.as_str(), v.as_str())))
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        #[cfg(windows)]
        {
            const CREATE_NO_WINDOW: u32 = 0x0800_0000;
            cmd.creation_flags(CREATE_NO_WINDOW);
        }
        let mut child = cmd.spawn().map_err(|e| {
            EngineError::Failed(format!(
                "cannot start engine ({}): {e}",
                cfg.python.display()
            ))
        })?;
        #[cfg(windows)]
        {
            if let Some(h) = child.raw_handle() {
                match win::JobHandle::kill_on_close(h) {
                    Ok(job) => *self.job.lock() = Some(job),
                    Err(e) => tracing::warn!("could not attach engine to job object: {e}"),
                }
            }
        }
        let file = tokio::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&cfg.log_file)
            .await
            .ok()
            .map(|f| Arc::new(tokio::sync::Mutex::new(f)));
        for stream in [
            child
                .stdout
                .take()
                .map(|s| Box::new(s) as Box<dyn tokio::io::AsyncRead + Send + Unpin>),
            child.stderr.take().map(|s| Box::new(s) as _),
        ]
        .into_iter()
        .flatten()
        {
            let log = log.clone();
            let file = file.clone();
            tokio::spawn(async move {
                let mut lines = BufReader::new(stream).lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    if let Some(f) = &file {
                        let mut f = f.lock().await;
                        let _ = f.write_all(line.as_bytes()).await;
                        let _ = f.write_all(b"\n").await;
                    }
                    log(line);
                }
            });
        }
        *self.child.lock() = Some(child);
        *self.config.lock() = Some(cfg.clone());

        let client = ComfyClient::new(format!("http://127.0.0.1:{}", cfg.port));
        let deadline = tokio::time::Instant::now() + ready_timeout;
        loop {
            if client.system_stats().await.is_ok() {
                return Ok(client);
            }
            if let Some(code) = self.exit_code() {
                *self.child.lock() = None;
                return Err(EngineError::Failed(format!(
                    "engine exited during startup (code {code}); see the engine log"
                )));
            }
            if tokio::time::Instant::now() > deadline {
                self.stop().await;
                return Err(EngineError::Failed(
                    "engine did not become ready in time".into(),
                ));
            }
            tokio::time::sleep(Duration::from_millis(400)).await;
        }
    }

    fn exit_code(&self) -> Option<i32> {
        let mut c = self.child.lock();
        match c.as_mut().map(|ch| ch.try_wait()) {
            Some(Ok(Some(status))) => Some(status.code().unwrap_or(-1)),
            _ => None,
        }
    }

    pub async fn stop(&self) {
        let child = self.child.lock().take();
        if let Some(mut child) = child {
            let _ = child.start_kill();
            let _ = tokio::time::timeout(Duration::from_secs(10), child.wait()).await;
        }
        *self.config.lock() = None;
        #[cfg(windows)]
        {
            self.job.lock().take();
        }
    }
}

/// Pick a free loopback port.
pub fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .and_then(|l| l.local_addr())
        .map(|a| a.port())
        .unwrap_or(8188)
}

#[cfg(windows)]
mod win {
    use std::ffi::c_void;
    use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};
    use windows_sys::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
        JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation,
        SetInformationJobObject,
    };

    pub struct JobHandle(HANDLE);
    // The handle is only used to keep the job alive and to close it.
    unsafe impl Send for JobHandle {}

    impl JobHandle {
        pub fn kill_on_close(process: *mut c_void) -> Result<Self, String> {
            unsafe {
                let job = CreateJobObjectW(std::ptr::null(), std::ptr::null());
                if job.is_null() {
                    return Err("CreateJobObjectW failed".into());
                }
                let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
                info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
                let ok = SetInformationJobObject(
                    job,
                    JobObjectExtendedLimitInformation,
                    &info as *const _ as *const c_void,
                    std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
                );
                if ok == 0 {
                    CloseHandle(job);
                    return Err("SetInformationJobObject failed".into());
                }
                if AssignProcessToJobObject(job, process as HANDLE) == 0 {
                    CloseHandle(job);
                    return Err("AssignProcessToJobObject failed".into());
                }
                Ok(Self(job))
            }
        }
    }

    impl Drop for JobHandle {
        fn drop(&mut self) {
            unsafe {
                CloseHandle(self.0);
            }
        }
    }
}
