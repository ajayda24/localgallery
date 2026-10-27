//! Managed, portable ComfyUI install.
//!
//! Everything lands inside the app folder:
//! `engines/tools/uv`, `engines/python` (uv-managed CPython), `engines/full/ComfyUI`,
//! `engines/full/venv`, with caches under `data/cache`. Steps are data so they can be
//! shown to the user before anything is downloaded, and tested on any OS.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::{Path, PathBuf};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio_util::sync::CancellationToken;

#[derive(Debug, Clone, Deserialize)]
pub struct Lock {
    pub comfyui: LockComfy,
    pub python: String,
    pub uv: LockUv,
    pub torch: std::collections::BTreeMap<String, TorchProfile>,
    #[serde(default)]
    pub never_install: Vec<String>,
    /// Pinned ComfyUI extensions installed with the engine (e.g. ComfyUI-GGUF).
    #[serde(default)]
    pub custom_nodes: Vec<LockNode>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct LockNode {
    pub name: String,
    pub commit: String,
    /// Zip URL with `{commit}`.
    pub archive: String,
    /// Extra Python packages, installed under the torch constraints.
    #[serde(default)]
    pub requirements: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct LockComfy {
    pub repo: String,
    pub commit: String,
    pub archive: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct LockUv {
    pub version: String,
    pub windows_zip: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct TorchProfile {
    pub index: String,
    pub packages: Vec<String>,
}

/// Paths of a portable install.
#[derive(Debug, Clone)]
pub struct Layout {
    pub engines: PathBuf,
    pub data: PathBuf,
    /// Share a package cache with another install (default: `data/cache/uv`).
    pub uv_cache: Option<PathBuf>,
}

impl Layout {
    pub fn new(app_root: &Path) -> Self {
        Self {
            engines: app_root.join("engines"),
            data: app_root.join("data"),
            uv_cache: None,
        }
    }
    pub fn full(&self) -> PathBuf {
        self.engines.join("full")
    }
    pub fn comfy_dir(&self) -> PathBuf {
        self.full().join("ComfyUI")
    }
    pub fn venv(&self) -> PathBuf {
        self.full().join("venv")
    }
    pub fn venv_python(&self) -> PathBuf {
        if cfg!(windows) {
            self.venv().join("Scripts").join("python.exe")
        } else {
            self.venv().join("bin").join("python")
        }
    }
    /// ComfyUI runs with `--base-directory full/base`, so extensions live here.
    pub fn custom_nodes(&self) -> PathBuf {
        self.full().join("base").join("custom_nodes")
    }
    fn addon_marker(&self, name: &str) -> PathBuf {
        self.custom_nodes().join(name).join(".localgallery.json")
    }
    pub fn uv_dir(&self) -> PathBuf {
        self.engines.join("tools").join("uv")
    }
    pub fn constraints(&self) -> PathBuf {
        self.full().join("constraints.txt")
    }
    pub fn installed_marker(&self) -> PathBuf {
        self.full().join("installed.json")
    }
    pub fn is_installed(&self) -> bool {
        self.installed_marker().exists()
            && self.venv_python().exists()
            && self.comfy_dir().join("main.py").exists()
    }
    /// Environment for every engine child process: caches and temp files stay in the app folder.
    pub fn child_env(&self) -> Vec<(String, String)> {
        let c = self.data.join("cache");
        let s = |p: PathBuf| p.to_string_lossy().into_owned();
        vec![
            (
                "UV_CACHE_DIR".into(),
                s(self.uv_cache.clone().unwrap_or_else(|| c.join("uv"))),
            ),
            (
                "UV_PYTHON_INSTALL_DIR".into(),
                s(self.engines.join("python")),
            ),
            ("UV_PYTHON_PREFERENCE".into(), "only-managed".into()),
            ("UV_NO_CONFIG".into(), "1".into()),
            ("HF_HOME".into(), s(c.join("huggingface"))),
            ("TORCH_HOME".into(), s(c.join("torch"))),
            ("TRITON_CACHE_DIR".into(), s(c.join("triton"))),
            ("CUDA_CACHE_PATH".into(), s(c.join("cuda"))),
            ("PIP_CACHE_DIR".into(), s(c.join("pip"))),
            ("XDG_CACHE_HOME".into(), s(c.clone())),
            ("TEMP".into(), s(self.data.join("tmp"))),
            ("TMP".into(), s(self.data.join("tmp"))),
            ("TMPDIR".into(), s(self.data.join("tmp"))),
        ]
    }
}

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Step {
    Download {
        label: String,
        url: String,
        dest: PathBuf,
    },
    /// Extract a zip, dropping the archive's single top-level folder when `strip_top`.
    Extract {
        label: String,
        archive: PathBuf,
        dest: PathBuf,
        strip_top: bool,
    },
    /// Delete a folder if it exists (before reinstalling an extension).
    RemoveDir {
        label: String,
        path: PathBuf,
    },
    /// Copy an existing ComfyUI checkout (offline installs and tests).
    CopyDir {
        label: String,
        from: PathBuf,
        to: PathBuf,
    },
    Run {
        label: String,
        program: PathBuf,
        args: Vec<String>,
    },
    /// Record installed torch versions as pip constraints so nothing can replace them.
    FreezeTorch {
        label: String,
    },
    WriteFile {
        label: String,
        path: PathBuf,
        contents: String,
    },
    SelfTest {
        label: String,
    },
}

impl Step {
    pub fn label(&self) -> &str {
        match self {
            Step::Download { label, .. }
            | Step::Extract { label, .. }
            | Step::CopyDir { label, .. }
            | Step::RemoveDir { label, .. }
            | Step::Run { label, .. }
            | Step::FreezeTorch { label }
            | Step::WriteFile { label, .. }
            | Step::SelfTest { label } => label,
        }
    }
}

#[derive(Debug, Clone)]
pub struct InstallOptions {
    /// Key into `lock.torch` ("cuda" or "cpu").
    pub torch_profile: String,
    /// Use this uv binary instead of downloading one.
    pub uv: Option<PathBuf>,
    /// Copy ComfyUI from here instead of downloading the pinned archive.
    pub comfy_source: Option<PathBuf>,
    /// Override the torch package index (e.g. PyPI for CPU-only test machines).
    pub torch_index: Option<String>,
    /// Copy extensions from these folders (by name) instead of downloading them.
    pub addon_sources: std::collections::BTreeMap<String, PathBuf>,
}

/// Extensions from the lock that are not installed at the pinned commit.
pub fn missing_addons<'a>(lock: &'a Lock, layout: &Layout) -> Vec<&'a LockNode> {
    lock.custom_nodes
        .iter()
        .filter(|n| {
            let marker = std::fs::read_to_string(layout.addon_marker(&n.name)).unwrap_or_default();
            !marker.contains(&n.commit)
        })
        .collect()
}

/// Steps that install the given extensions into an engine whose venv exists.
fn addon_steps(
    nodes: &[&LockNode],
    layout: &Layout,
    uv: &Path,
    opts: &InstallOptions,
) -> Vec<Step> {
    let dl = layout.data.join("cache").join("downloads");
    let py = layout.venv_python().to_string_lossy().into_owned();
    let mut steps = Vec::new();
    for n in nodes {
        let dest = layout.custom_nodes().join(&n.name);
        steps.push(Step::RemoveDir {
            label: format!("Prepare {}", n.name),
            path: dest.clone(),
        });
        match opts.addon_sources.get(&n.name) {
            Some(src) => steps.push(Step::CopyDir {
                label: format!("Copy {}", n.name),
                from: src.clone(),
                to: dest.clone(),
            }),
            None => {
                let zip = dl.join(format!(
                    "{}-{}.zip",
                    n.name,
                    &n.commit[..12.min(n.commit.len())]
                ));
                steps.push(Step::Download {
                    label: format!("Download {}", n.name),
                    url: n.archive.replace("{commit}", &n.commit),
                    dest: zip.clone(),
                });
                steps.push(Step::Extract {
                    label: format!("Unpack {}", n.name),
                    archive: zip,
                    dest: dest.clone(),
                    strip_top: true,
                });
            }
        }
        if !n.requirements.is_empty() {
            let mut args: Vec<String> = vec![
                "pip".into(),
                "install".into(),
                "--python".into(),
                py.clone(),
            ];
            args.extend(n.requirements.iter().cloned());
            // Relative on purpose (see the ComfyUI requirements step): torch stays pinned.
            args.extend(["-c".into(), "constraints.txt".into()]);
            steps.push(Step::Run {
                label: format!("Install {} requirements", n.name),
                program: uv.to_path_buf(),
                args,
            });
        }
        steps.push(Step::WriteFile {
            label: format!("Finish {}", n.name),
            path: layout.addon_marker(&n.name),
            contents: serde_json::json!({"name": n.name, "commit": n.commit}).to_string(),
        });
    }
    steps
}

/// Bring an existing engine's extensions up to the lock (e.g. after an app update adds one).
pub fn addons_plan(
    lock: &Lock,
    layout: &Layout,
    opts: &InstallOptions,
) -> Result<Vec<Step>, String> {
    let missing = missing_addons(lock, layout);
    if missing.is_empty() {
        return Ok(Vec::new());
    }
    let uv = match &opts.uv {
        Some(p) => p.clone(),
        None if cfg!(windows) => layout.uv_dir().join("uv.exe"),
        None => return Err("on this OS, pass the path to an installed uv".into()),
    };
    if !uv.exists() {
        return Err(format!("uv not found at {}", uv.display()));
    }
    Ok(addon_steps(&missing, layout, &uv, opts))
}

pub fn plan(lock: &Lock, layout: &Layout, opts: &InstallOptions) -> Result<Vec<Step>, String> {
    let mut steps = Vec::new();
    let dl = layout.data.join("cache").join("downloads");
    let uv = match &opts.uv {
        Some(p) => p.clone(),
        None => {
            if !cfg!(windows) {
                return Err("on this OS, pass the path to an installed uv".into());
            }
            let zip = dl.join(format!("uv-{}.zip", lock.uv.version));
            steps.push(Step::Download {
                label: format!("Download uv {}", lock.uv.version),
                url: lock.uv.windows_zip.replace("{version}", &lock.uv.version),
                dest: zip.clone(),
            });
            steps.push(Step::Extract {
                label: "Unpack uv".into(),
                archive: zip,
                dest: layout.uv_dir(),
                strip_top: false,
            });
            layout.uv_dir().join("uv.exe")
        }
    };
    match &opts.comfy_source {
        Some(src) => steps.push(Step::CopyDir {
            label: "Copy ComfyUI".into(),
            from: src.clone(),
            to: layout.comfy_dir(),
        }),
        None => {
            let zip = dl.join(format!(
                "ComfyUI-{}.zip",
                &lock.comfyui.commit[..12.min(lock.comfyui.commit.len())]
            ));
            steps.push(Step::Download {
                label: format!(
                    "Download ComfyUI ({})",
                    &lock.comfyui.commit[..8.min(lock.comfyui.commit.len())]
                ),
                url: lock
                    .comfyui
                    .archive
                    .replace("{commit}", &lock.comfyui.commit),
                dest: zip.clone(),
            });
            steps.push(Step::Extract {
                label: "Unpack ComfyUI".into(),
                archive: zip,
                dest: layout.comfy_dir(),
                strip_top: true,
            });
        }
    }
    let venv = layout.venv().to_string_lossy().into_owned();
    let py = layout.venv_python().to_string_lossy().into_owned();
    steps.push(Step::Run {
        label: format!("Install Python {}", lock.python),
        program: uv.clone(),
        args: vec!["python".into(), "install".into(), lock.python.clone()],
    });
    steps.push(Step::Run {
        label: "Create the engine environment".into(),
        program: uv.clone(),
        args: vec![
            "venv".into(),
            venv,
            "--python".into(),
            lock.python.clone(),
            "--clear".into(),
        ],
    });
    let profile = lock
        .torch
        .get(&opts.torch_profile)
        .ok_or_else(|| format!("unknown torch profile {:?}", opts.torch_profile))?;
    let mut torch_args = vec![
        "pip".into(),
        "install".into(),
        "--python".into(),
        py.clone(),
    ];
    torch_args.extend(profile.packages.iter().cloned());
    let index = opts
        .torch_index
        .clone()
        .unwrap_or_else(|| profile.index.clone());
    torch_args.extend(["--index-url".into(), index]);
    steps.push(Step::Run {
        label: "Install PyTorch (large download)".into(),
        program: uv.clone(),
        args: torch_args,
    });
    steps.push(Step::FreezeTorch {
        label: "Pin PyTorch".into(),
    });
    let reqs = layout
        .comfy_dir()
        .join("requirements.txt")
        .to_string_lossy()
        .into_owned();
    steps.push(Step::Run {
        label: "Install ComfyUI requirements".into(),
        program: uv.clone(),
        args: vec![
            "pip".into(),
            "install".into(),
            "--python".into(),
            py,
            "-r".into(),
            reqs,
            // Relative on purpose: uv cuts `-c` paths at the first space, so this
            // runs with the working directory set to engines/full (see `command`).
            "-c".into(),
            "constraints.txt".into(),
        ],
    });
    let all: Vec<&LockNode> = lock.custom_nodes.iter().collect();
    steps.extend(addon_steps(&all, layout, &uv, opts));
    steps.push(Step::SelfTest {
        label: "Check the GPU works".into(),
    });
    steps.push(Step::WriteFile {
        label: "Finish".into(),
        path: layout.installed_marker(),
        contents: serde_json::json!({"comfyui_commit": lock.comfyui.commit, "python": lock.python, "torch_profile": opts.torch_profile}).to_string(),
    });
    Ok(steps)
}

#[derive(Debug, Clone, Serialize)]
pub struct InstallProgress {
    pub step: usize,
    pub steps: usize,
    pub label: String,
    pub line: Option<String>,
    pub received: Option<u64>,
    pub total: Option<u64>,
}

/// Result of the post-install GPU check.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct SelfTest {
    pub torch: String,
    pub cuda: Option<String>,
    pub cuda_available: bool,
    pub device: Option<String>,
    pub capability: Option<(u32, u32)>,
    pub arch_list: Vec<String>,
    pub matmul_ok: bool,
    pub error: Option<String>,
}

const SELF_TEST_PY: &str = r#"
import json
r = {"torch": "", "cuda": None, "cuda_available": False, "device": None, "capability": None, "arch_list": [], "matmul_ok": False, "error": None}
try:
    import torch
    r["torch"] = torch.__version__
    r["cuda"] = torch.version.cuda
    r["cuda_available"] = torch.cuda.is_available()
    if r["cuda_available"]:
        r["device"] = torch.cuda.get_device_name(0)
        r["capability"] = list(torch.cuda.get_device_capability(0))
        r["arch_list"] = torch.cuda.get_arch_list()
        a = torch.randn(256, 256, device="cuda")
        r["matmul_ok"] = bool(torch.isfinite(a @ a).all().item())
    else:
        a = torch.randn(64, 64)
        r["matmul_ok"] = bool(torch.isfinite(a @ a).all().item())
except Exception as e:
    r["error"] = repr(e)
print("LG_SELFTEST " + json.dumps(r))
"#;

pub fn parse_self_test(output: &str) -> Option<SelfTest> {
    output
        .lines()
        .find_map(|l| l.strip_prefix("LG_SELFTEST "))
        .and_then(|j| serde_json::from_str(j).ok())
}

impl SelfTest {
    /// A problem worth stopping for, in plain words.
    pub fn problem(&self, want_cuda: bool) -> Option<String> {
        if let Some(e) = &self.error {
            return Some(format!("PyTorch failed to load: {e}"));
        }
        if want_cuda && !self.cuda_available {
            return Some(
                "PyTorch cannot see the NVIDIA GPU. Update the NVIDIA driver and try Repair."
                    .into(),
            );
        }
        if let Some((maj, min)) = self.capability {
            let arch = format!("sm_{maj}{min}");
            if !self.arch_list.is_empty()
                && !self
                    .arch_list
                    .iter()
                    .any(|a| a == &arch || a.starts_with(&format!("compute_{maj}")))
            {
                return Some(format!(
                    "This PyTorch build has no kernels for your GPU ({arch}). A newer CUDA build is needed."
                ));
            }
        }
        if !self.matmul_ok {
            return Some("A test calculation on the GPU failed.".into());
        }
        None
    }
}

pub async fn run(
    steps: &[Step],
    layout: &Layout,
    want_cuda: bool,
    cancel: &CancellationToken,
    progress: &(dyn Fn(InstallProgress) + Send + Sync),
) -> Result<Option<SelfTest>, String> {
    let n = steps.len();
    let mut test = None;
    let http = lg_downloader::client();
    for d in [
        layout.data.join("tmp"),
        layout.data.join("cache"),
        layout.full(),
    ] {
        let _ = tokio::fs::create_dir_all(d).await;
    }
    for (i, step) in steps.iter().enumerate() {
        if cancel.is_cancelled() {
            return Err("cancelled".into());
        }
        let report = |line: Option<String>, received: Option<u64>, total: Option<u64>| {
            progress(InstallProgress {
                step: i,
                steps: n,
                label: step.label().to_string(),
                line,
                received,
                total,
            })
        };
        report(None, None, None);
        match step {
            Step::Download { url, dest, .. } => {
                if !dest.exists() {
                    let req = lg_downloader::Request {
                        url: url.clone(),
                        dest: dest.clone(),
                        sha256: None,
                        hf_token: None,
                    };
                    lg_downloader::download(&http, &req, cancel, |p| {
                        report(None, Some(p.received), p.total)
                    })
                    .await
                    .map_err(|e| format!("{}: {e}", step.label()))?;
                }
            }
            Step::Extract {
                archive,
                dest,
                strip_top,
                ..
            } => {
                let (a, d, s) = (archive.clone(), dest.clone(), *strip_top);
                tokio::task::spawn_blocking(move || extract_zip(&a, &d, s))
                    .await
                    .map_err(|e| e.to_string())??;
            }
            Step::RemoveDir { path, .. } => {
                if path.exists() {
                    tokio::fs::remove_dir_all(path)
                        .await
                        .map_err(|e| format!("{}: {e}", path.display()))?;
                }
            }
            Step::CopyDir { from, to, .. } => {
                let (f, t) = (from.clone(), to.clone());
                tokio::task::spawn_blocking(move || copy_dir(&f, &t))
                    .await
                    .map_err(|e| e.to_string())??;
            }
            Step::Run { program, args, .. } => {
                run_logged(program, args, layout, cancel, &|l| {
                    report(Some(l), None, None)
                })
                .await?;
            }
            Step::FreezeTorch { .. } => {
                let out = capture(
                    &layout.venv_python(),
                    &["-c".into(), "import importlib.metadata as m\nfor p in ('torch','torchvision','torchaudio'):\n    try: print(f'{p}=={m.version(p)}')\n    except Exception: pass".into()],
                    layout,
                )
                .await?;
                let mut text: String = out
                    .lines()
                    .filter(|l| l.contains("=="))
                    .map(|l| format!("{l}\n"))
                    .collect();
                if text.is_empty() {
                    return Err("PyTorch did not install".into());
                }
                // Packages that are known to downgrade PyTorch and break new GPUs.
                text.push_str("# never install: xformers\n");
                tokio::fs::write(layout.constraints(), text)
                    .await
                    .map_err(|e| e.to_string())?;
            }
            Step::WriteFile { path, contents, .. } => {
                tokio::fs::write(path, contents)
                    .await
                    .map_err(|e| e.to_string())?;
            }
            Step::SelfTest { .. } => {
                let out = capture(
                    &layout.venv_python(),
                    &["-c".into(), SELF_TEST_PY.into()],
                    layout,
                )
                .await?;
                let t = parse_self_test(&out)
                    .ok_or_else(|| format!("self-test produced no result: {out}"))?;
                if let Some(p) = t.problem(want_cuda) {
                    return Err(p);
                }
                test = Some(t);
            }
        }
    }
    Ok(test)
}

fn command(program: &Path, args: &[String], layout: &Layout) -> tokio::process::Command {
    let mut c = tokio::process::Command::new(program);
    c.args(args)
        .current_dir(layout.full())
        .envs(layout.child_env())
        .env("PYTHONNOUSERSITE", "1")
        .env_remove("VIRTUAL_ENV")
        .env_remove("PYTHONPATH")
        .stdin(std::process::Stdio::null())
        .kill_on_drop(true);
    #[cfg(windows)]
    c.creation_flags(0x0800_0000);
    c
}

async fn capture(program: &Path, args: &[String], layout: &Layout) -> Result<String, String> {
    let out = command(program, args, layout)
        .output()
        .await
        .map_err(|e| format!("{}: {e}", program.display()))?;
    let mut s = String::from_utf8_lossy(&out.stdout).into_owned();
    s.push_str(&String::from_utf8_lossy(&out.stderr));
    if !out.status.success() {
        return Err(s);
    }
    Ok(s)
}

async fn run_logged(
    program: &Path,
    args: &[String],
    layout: &Layout,
    cancel: &CancellationToken,
    line: &(dyn Fn(String) + Send + Sync),
) -> Result<(), String> {
    let mut child = command(program, args, layout)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| format!("cannot run {}: {e}", program.display()))?;
    let out = child.stdout.take().unwrap();
    let err = child.stderr.take().unwrap();
    let mut a = BufReader::new(out).lines();
    let mut b = BufReader::new(err).lines();
    let mut tail = Vec::new();
    let (mut a_done, mut b_done) = (false, false);
    while !(a_done && b_done) {
        let l = tokio::select! {
            l = a.next_line(), if !a_done => l,
            l = b.next_line(), if !b_done => l,
            _ = cancel.cancelled() => { let _ = child.kill().await; return Err("cancelled".into()); }
        };
        match l {
            Ok(Some(l)) => {
                tail.push(l.clone());
                if tail.len() > 20 {
                    tail.remove(0);
                }
                line(l);
            }
            _ => {
                if !a_done {
                    a_done = true
                } else {
                    b_done = true
                }
            }
        }
    }
    let status = child.wait().await.map_err(|e| e.to_string())?;
    if !status.success() {
        return Err(format!(
            "{} failed:\n{}",
            program.file_name().unwrap_or_default().to_string_lossy(),
            tail.join("\n")
        ));
    }
    Ok(())
}

pub fn extract_zip(archive: &Path, dest: &Path, strip_top: bool) -> Result<(), String> {
    let f = std::fs::File::open(archive).map_err(|e| e.to_string())?;
    let mut z = zip::ZipArchive::new(f).map_err(|e| e.to_string())?;
    std::fs::create_dir_all(dest).map_err(|e| e.to_string())?;
    for i in 0..z.len() {
        let mut entry = z.by_index(i).map_err(|e| e.to_string())?;
        // enclosed_name rejects absolute paths and `..` (zip-slip).
        let Some(rel) = entry.enclosed_name() else {
            continue;
        };
        let rel: PathBuf = if strip_top {
            rel.components().skip(1).collect()
        } else {
            rel
        };
        if rel.as_os_str().is_empty() {
            continue;
        }
        let out = dest.join(rel);
        if entry.is_dir() {
            std::fs::create_dir_all(&out).map_err(|e| e.to_string())?;
        } else {
            if let Some(p) = out.parent() {
                std::fs::create_dir_all(p).map_err(|e| e.to_string())?;
            }
            let mut w = std::fs::File::create(&out).map_err(|e| e.to_string())?;
            std::io::copy(&mut entry, &mut w).map_err(|e| e.to_string())?;
        }
    }
    Ok(())
}

pub fn copy_dir(from: &Path, to: &Path) -> Result<(), String> {
    std::fs::create_dir_all(to).map_err(|e| e.to_string())?;
    for e in std::fs::read_dir(from).map_err(|e| format!("{}: {e}", from.display()))? {
        let e = e.map_err(|e| e.to_string())?;
        let name = e.file_name();
        if name == ".git" || name == "__pycache__" {
            continue;
        }
        let ty = e.file_type().map_err(|e| e.to_string())?;
        let target = to.join(&name);
        if ty.is_dir() {
            copy_dir(&e.path(), &target)?;
        } else {
            std::fs::copy(e.path(), &target).map_err(|e| e.to_string())?;
        }
    }
    Ok(())
}

/// Read `engines.lock.json`.
pub fn read_lock(path: &Path) -> Result<Lock, String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let v: Value = serde_json::from_str(&text).map_err(|e| e.to_string())?;
    serde_json::from_value(v).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn lock() -> Lock {
        read_lock(&Path::new(env!("CARGO_MANIFEST_DIR")).join("../../engines.lock.json")).unwrap()
    }

    #[test]
    fn plan_installs_into_app_folder() {
        let layout = Layout::new(Path::new("/apps/Local Gallery"));
        let steps = plan(
            &lock(),
            &layout,
            &InstallOptions {
                torch_profile: "cuda".into(),
                uv: Some("/usr/bin/uv".into()),
                comfy_source: None,
                torch_index: None,
                addon_sources: Default::default(),
            },
        )
        .unwrap();
        let text = serde_json::to_string(&steps).unwrap();
        assert!(text.contains("cu130"));
        assert!(text.contains("9fe7ef764d04"));
        assert!(!text.contains("xformers"));
        // ComfyUI's and each extension's requirements install under the torch constraints.
        let runs: Vec<&Vec<String>> = steps
            .iter()
            .filter_map(|s| match s {
                Step::Run { label, args, .. } if label.contains("requirements") => Some(args),
                _ => None,
            })
            .collect();
        assert_eq!(runs.len(), 2, "{steps:?}");
        assert!(runs.iter().all(|a| a.iter().any(|x| x == "-c")));
        assert!(
            runs[1].iter().any(|x| x.starts_with("gguf")),
            "{:?}",
            runs[1]
        );
        // GGUF support: pinned archive, unpacked into the engine's custom_nodes.
        assert!(
            text.contains("ComfyUI-GGUF/archive/6ea2651e7df66d7585f6ffee804b20e92fb38b8a.zip"),
            "{text}"
        );
        assert!(
            text.contains("base/custom_nodes/ComfyUI-GGUF")
                || text.contains("base\\\\custom_nodes\\\\ComfyUI-GGUF"),
            "{text}"
        );
        for (_, v) in layout.child_env() {
            assert!(
                v.starts_with("/apps/Local Gallery") || !v.contains('/'),
                "{v} escapes the app folder"
            );
        }
    }

    #[test]
    fn self_test_diagnoses_missing_kernels() {
        let t = parse_self_test(r#"LG_SELFTEST {"torch":"2.9.0+cu126","cuda":"12.6","cuda_available":true,"device":"RTX 5060","capability":[12,0],"arch_list":["sm_80","sm_90"],"matmul_ok":false,"error":null}"#).unwrap();
        assert!(t.problem(true).unwrap().contains("sm_120"));
        let ok = SelfTest {
            torch: "2.14".into(),
            cuda_available: true,
            capability: Some((12, 0)),
            arch_list: vec!["sm_120".into()],
            matmul_ok: true,
            ..Default::default()
        };
        assert_eq!(ok.problem(true), None);
    }

    #[test]
    fn extract_strips_top_and_blocks_zip_slip() {
        let dir = tempfile::tempdir().unwrap();
        let zpath = dir.path().join("a.zip");
        {
            let mut w = zip::ZipWriter::new(std::fs::File::create(&zpath).unwrap());
            let o: zip::write::SimpleFileOptions = Default::default();
            w.start_file("ComfyUI-abc/main.py", o).unwrap();
            w.write_all(b"print(1)").unwrap();
            w.start_file("../evil.txt", o).unwrap();
            w.write_all(b"x").unwrap();
            w.finish().unwrap();
        }
        let dest = dir.path().join("out dir");
        extract_zip(&zpath, &dest, true).unwrap();
        assert_eq!(
            std::fs::read_to_string(dest.join("main.py")).unwrap(),
            "print(1)"
        );
        assert!(!dir.path().join("evil.txt").exists());
    }
}
