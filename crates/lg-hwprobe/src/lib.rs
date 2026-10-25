//! What the PC has, and how much of it a run used.
//!
//! NVIDIA GPUs are read through `nvidia-smi` (ships with the driver, no extra DLLs).
//! Other GPUs are filled in later from the engine's `/system_stats`.

use lg_types::{DiskInfo, GpuInfo, HardwareInfo};
use parking_lot::Mutex;
use std::path::Path;
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, BufReader};

const GB: f64 = 1024.0 * 1024.0 * 1024.0;

fn command(program: &str) -> tokio::process::Command {
    let mut c = tokio::process::Command::new(program);
    c.stdin(Stdio::null()).kill_on_drop(true);
    #[cfg(windows)]
    c.creation_flags(0x0800_0000);
    c
}

async fn output(program: &str, args: &[&str]) -> Option<String> {
    let out = tokio::time::timeout(Duration::from_secs(8), command(program).args(args).output())
        .await
        .ok()?
        .ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}

/// Probe the machine. `disk_path` is the app folder (where models and outputs live).
pub async fn probe(disk_path: &Path) -> HardwareInfo {
    let mut sys = sysinfo::System::new();
    sys.refresh_memory();
    sys.refresh_cpu_all();
    let cpu = sys
        .cpus()
        .first()
        .map(|c| c.brand().trim().to_string())
        .unwrap_or_default();
    let mut hw = HardwareInfo {
        os: format!(
            "{} {}",
            sysinfo::System::name().unwrap_or_default(),
            sysinfo::System::os_version().unwrap_or_default()
        )
        .trim()
        .to_string(),
        cpu,
        cpu_threads: sys.cpus().len(),
        ram_total_gb: sys.total_memory() as f64 / GB,
        ram_free_gb: sys.available_memory() as f64 / GB,
        gpus: nvidia_gpus().await,
        npu: npu().await,
        disk: disk(disk_path),
        laptop: laptop(),
    };
    hw.gpus.sort_by_key(|g| g.index);
    hw
}

const QUERY: &str = "index,name,memory.total,memory.free,driver_version,compute_cap";

async fn nvidia_gpus() -> Vec<GpuInfo> {
    let Some(csv) = output(
        "nvidia-smi",
        &[
            &format!("--query-gpu={QUERY}"),
            "--format=csv,noheader,nounits",
        ],
    )
    .await
    else {
        return vec![];
    };
    let cuda = output("nvidia-smi", &[])
        .await
        .and_then(|s| parse_cuda_version(&s));
    parse_gpus(&csv, cuda.as_deref())
}

/// Parse `nvidia-smi --query-gpu=index,name,memory.total,memory.free,driver_version,compute_cap`.
pub fn parse_gpus(csv: &str, cuda: Option<&str>) -> Vec<GpuInfo> {
    csv.lines()
        .filter_map(|line| {
            let f: Vec<&str> = line.split(',').map(str::trim).collect();
            if f.len() < 4 {
                return None;
            }
            let mib = |s: &str| s.parse::<f64>().ok().map(|m| m / 1024.0);
            Some(GpuInfo {
                index: f[0].parse().ok()?,
                name: f[1].to_string(),
                vendor: "nvidia".into(),
                vram_total_gb: mib(f[2])?,
                vram_free_gb: mib(f[3]).unwrap_or(0.0),
                driver: f
                    .get(4)
                    .filter(|s| !s.is_empty() && **s != "[N/A]")
                    .map(|s| s.to_string()),
                compute_capability: f.get(5).and_then(|s| {
                    let (a, b) = s.split_once('.')?;
                    Some((a.parse().ok()?, b.parse().ok()?))
                }),
                cuda_driver_version: cuda.map(str::to_string),
            })
        })
        .collect()
}

/// "CUDA Version: 13.0" from the plain `nvidia-smi` banner.
pub fn parse_cuda_version(banner: &str) -> Option<String> {
    let i = banner.find("CUDA Version:")?;
    banner[i + 13..]
        .split_whitespace()
        .next()
        .map(|v| v.trim_matches('|').to_string())
}

fn disk(path: &Path) -> Option<DiskInfo> {
    let disks = sysinfo::Disks::new_with_refreshed_list();
    let path = lg_types::canonical(path);
    // The disk whose mount point is the longest prefix of the app folder (drive letters are
    // compared without case on Windows).
    let norm = |p: &Path| {
        let s = p.to_string_lossy().into_owned();
        if cfg!(windows) { s.to_lowercase() } else { s }
    };
    let target = norm(&path);
    let d = disks
        .list()
        .iter()
        .filter(|d| target.starts_with(&norm(d.mount_point())))
        .max_by_key(|d| d.mount_point().as_os_str().len())?;
    Some(DiskInfo {
        path: d.mount_point().display().to_string(),
        free_gb: d.available_space() as f64 / GB,
        total_gb: d.total_space() as f64 / GB,
        kind: match d.kind() {
            sysinfo::DiskKind::SSD => "ssd".into(),
            sysinfo::DiskKind::HDD => "hdd".into(),
            _ => "unknown".into(),
        },
    })
}

#[cfg(windows)]
fn laptop() -> Option<bool> {
    use windows_sys::Win32::System::Power::{GetSystemPowerStatus, SYSTEM_POWER_STATUS};
    let mut s: SYSTEM_POWER_STATUS = unsafe { std::mem::zeroed() };
    if unsafe { GetSystemPowerStatus(&mut s) } == 0 {
        return None;
    }
    // 128 = no system battery.
    Some(s.BatteryFlag != 128 && s.BatteryFlag != 255)
}

#[cfg(not(windows))]
fn laptop() -> Option<bool> {
    let any = std::fs::read_dir("/sys/class/power_supply")
        .ok()?
        .flatten()
        .any(|e| e.file_name().to_string_lossy().starts_with("BAT"));
    Some(any)
}

/// NPUs show up on Windows as "ComputeAccelerator" devices (Intel AI Boost, AMD NPU, Qualcomm Hexagon).
async fn npu() -> String {
    #[cfg(windows)]
    {
        let script = "Get-PnpDevice -Class ComputeAccelerator -PresentOnly -ErrorAction SilentlyContinue | Select-Object -ExpandProperty FriendlyName";
        match output(
            "powershell",
            &["-NoProfile", "-NonInteractive", "-Command", script],
        )
        .await
        {
            Some(s) if !s.trim().is_empty() => {
                s.lines().next().unwrap_or_default().trim().to_string()
            }
            Some(_) => "none".into(),
            None => "unknown".into(),
        }
    }
    #[cfg(not(windows))]
    {
        if Path::new("/dev/accel").exists() {
            "present".into()
        } else {
            "none".into()
        }
    }
}

/// Peaks seen while a monitor ran.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Peaks {
    pub vram_used_gb: Option<f64>,
    pub ram_used_gb: f64,
    /// Windows "shared GPU memory": VRAM overflow into system RAM, which is very slow.
    pub shared_gpu_gb: Option<f64>,
    pub gpu_util: Option<f64>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Sample {
    pub vram_used_gb: Option<f64>,
    pub vram_total_gb: Option<f64>,
    pub ram_used_gb: f64,
    pub ram_total_gb: f64,
    pub shared_gpu_gb: Option<f64>,
    pub gpu_util: Option<f64>,
}

/// Samples resources twice a second until dropped or stopped.
pub struct Monitor {
    peaks: Arc<Mutex<Peaks>>,
    last: Arc<Mutex<Sample>>,
    task: tokio::task::JoinHandle<()>,
}

impl Monitor {
    pub fn start() -> Self {
        let peaks = Arc::new(Mutex::new(Peaks::default()));
        let last = Arc::new(Mutex::new(Sample::default()));
        let (p, l) = (peaks.clone(), last.clone());
        let task = tokio::spawn(async move {
            let mut sys = sysinfo::System::new();
            // One long-running nvidia-smi, rather than a process per sample.
            let mut smi = command("nvidia-smi")
                .args([
                    "--query-gpu=memory.used,memory.total,utilization.gpu",
                    "--format=csv,noheader,nounits",
                    "-lms",
                    "500",
                ])
                .stdout(Stdio::piped())
                .stderr(Stdio::null())
                .spawn()
                .ok();
            let mut lines = smi
                .as_mut()
                .and_then(|c| c.stdout.take())
                .map(|o| BufReader::new(o).lines());
            let mut tick = tokio::time::interval(Duration::from_millis(500));
            let mut n = 0u32;
            loop {
                tick.tick().await;
                sys.refresh_memory();
                let mut s = Sample {
                    ram_used_gb: (sys.total_memory() - sys.available_memory()) as f64 / GB,
                    ram_total_gb: sys.total_memory() as f64 / GB,
                    ..Default::default()
                };
                if let Some(ls) = lines.as_mut()
                    && let Ok(Ok(Some(line))) =
                        tokio::time::timeout(Duration::from_millis(50), ls.next_line()).await
                {
                    let f: Vec<f64> = line
                        .split(',')
                        .filter_map(|x| x.trim().parse().ok())
                        .collect();
                    if f.len() >= 2 {
                        s.vram_used_gb = Some(f[0] / 1024.0);
                        s.vram_total_gb = Some(f[1] / 1024.0);
                        s.gpu_util = f.get(2).copied();
                    }
                }
                // Shared memory is read through Windows perf counters, every 2 s.
                if n.is_multiple_of(4) {
                    s.shared_gpu_gb = shared_gpu_gb().await;
                } else {
                    s.shared_gpu_gb = l.lock().shared_gpu_gb;
                }
                n += 1;
                {
                    let mut pk = p.lock();
                    pk.ram_used_gb = pk.ram_used_gb.max(s.ram_used_gb);
                    pk.vram_used_gb = max_opt(pk.vram_used_gb, s.vram_used_gb);
                    pk.shared_gpu_gb = max_opt(pk.shared_gpu_gb, s.shared_gpu_gb);
                    pk.gpu_util = max_opt(pk.gpu_util, s.gpu_util);
                }
                *l.lock() = s;
            }
        });
        Self { peaks, last, task }
    }

    pub fn peaks(&self) -> Peaks {
        *self.peaks.lock()
    }

    pub fn reset_peaks(&self) {
        *self.peaks.lock() = Peaks::default();
    }

    pub fn sample(&self) -> Sample {
        *self.last.lock()
    }
}

impl Drop for Monitor {
    fn drop(&mut self) {
        self.task.abort();
    }
}

fn max_opt(a: Option<f64>, b: Option<f64>) -> Option<f64> {
    match (a, b) {
        (Some(x), Some(y)) => Some(x.max(y)),
        (x, None) => x,
        (None, y) => y,
    }
}

/// Total "Shared Usage" across GPU adapters, in GB (Windows only).
pub async fn shared_gpu_gb() -> Option<f64> {
    #[cfg(windows)]
    {
        let csv = output(
            "typeperf",
            &[r"\GPU Adapter Memory(*)\Shared Usage", "-sc", "1"],
        )
        .await?;
        parse_typeperf_sum(&csv).map(|b| b / GB)
    }
    #[cfg(not(windows))]
    {
        None
    }
}

/// Sum the values of the data row of a `typeperf -sc 1` CSV (first column is the time).
pub fn parse_typeperf_sum(csv: &str) -> Option<f64> {
    let row = csv.lines().filter(|l| l.starts_with('"')).nth(1)?;
    let vals: Vec<f64> = row
        .split("\",\"")
        .skip(1)
        .filter_map(|v| v.trim_matches('"').trim().parse().ok())
        .collect();
    (!vals.is_empty()).then(|| vals.iter().sum())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_nvidia_smi() {
        let csv = "0, NVIDIA GeForce RTX 5060, 8151, 7420, 580.88, 12.0\n";
        let g = parse_gpus(csv, Some("13.0"));
        assert_eq!(g.len(), 1);
        assert_eq!(g[0].compute_capability, Some((12, 0)));
        assert!((g[0].vram_total_gb - 7.96).abs() < 0.01);
        assert!(g[0].supports_fp8() && g[0].supports_nvfp4());
        let banner = "| NVIDIA-SMI 580.88   Driver Version: 580.88   CUDA Version: 13.0     |";
        assert_eq!(parse_cuda_version(banner).as_deref(), Some("13.0"));
        // Old drivers without compute_cap.
        let g = parse_gpus("0, GTX 1080, 8192, 8000, [N/A]\n", None);
        assert_eq!(g[0].compute_capability, None);
        assert!(!g[0].supports_fp8());
    }

    #[test]
    fn parses_typeperf() {
        let csv = "\r\n\"(PDH-CSV 4.0)\",\"\\\\PC\\GPU Adapter Memory(luid_0x0_phys_0)\\Shared Usage\",\"\\\\PC\\GPU Adapter Memory(luid_1_phys_0)\\Shared Usage\"\r\n\"10/23/2026 20:00:00.000\",\"1073741824.000000\",\"536870912.000000\"\r\nExiting, please wait...\r\n";
        assert_eq!(parse_typeperf_sum(csv), Some(1610612736.0));
    }

    #[tokio::test]
    async fn probes_this_machine() {
        let hw = probe(Path::new(".")).await;
        assert!(hw.ram_total_gb > 0.5);
        assert!(hw.cpu_threads >= 1);
        assert!(hw.disk.is_some());
        let m = Monitor::start();
        tokio::time::sleep(Duration::from_millis(1200)).await;
        assert!(m.peaks().ram_used_gb > 0.0);
    }
}
