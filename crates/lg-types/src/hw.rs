//! Hardware description and memory plan types.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct HardwareInfo {
    pub os: String,
    pub cpu: String,
    pub cpu_threads: usize,
    pub ram_total_gb: f64,
    pub ram_free_gb: f64,
    pub gpus: Vec<GpuInfo>,
    /// "none" | "unknown" | vendor name.
    pub npu: String,
    pub disk: Option<DiskInfo>,
    pub laptop: Option<bool>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct GpuInfo {
    pub index: u32,
    pub name: String,
    pub vendor: String,
    pub vram_total_gb: f64,
    pub vram_free_gb: f64,
    /// CUDA compute capability, e.g. (12, 0) for consumer Blackwell.
    pub compute_capability: Option<(u32, u32)>,
    pub driver: Option<String>,
    /// Highest CUDA version the driver supports, e.g. "13.0".
    pub cuda_driver_version: Option<String>,
}

impl GpuInfo {
    /// fp8 tensor-core math: Ada (8.9) and newer.
    pub fn supports_fp8(&self) -> bool {
        matches!(self.compute_capability, Some((maj, min)) if maj > 8 || (maj == 8 && min >= 9))
    }

    /// NVFP4 tensor-core math: Blackwell (10.x datacentre, 12.x consumer).
    pub fn supports_nvfp4(&self) -> bool {
        matches!(self.compute_capability, Some((maj, _)) if maj >= 10)
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct DiskInfo {
    pub path: String,
    pub free_gb: f64,
    pub total_gb: f64,
    /// "nvme" | "ssd" | "hdd" | "unknown".
    pub kind: String,
}

impl HardwareInfo {
    pub fn primary_gpu(&self) -> Option<&GpuInfo> {
        self.gpus
            .iter()
            .max_by(|a, b| a.vram_total_gb.total_cmp(&b.vram_total_gb))
    }
}

/// Where the diffusion weights live while sampling.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "lowercase")]
pub enum Fit {
    /// Everything stays in VRAM.
    Resident,
    /// Part of the weights sit in RAM and stream to the GPU.
    Offload,
    /// Weights stream from disk; slow.
    Stream,
    /// Will not run on this machine.
    TooBig,
}

impl Fit {
    pub fn as_str(self) -> &'static str {
        match self {
            Fit::Resident => "resident",
            Fit::Offload => "offload",
            Fit::Stream => "stream",
            Fit::TooBig => "too_big",
        }
    }

    pub fn badge(self) -> &'static str {
        match self {
            Fit::Resident => "green",
            Fit::Offload => "yellow",
            Fit::Stream | Fit::TooBig => "red",
        }
    }
}

/// Output of the memory planner.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Plan {
    pub fit: Fit,
    /// Chosen weight variant per file role.
    pub variants: Vec<(String, String)>,
    pub precision: Option<String>,
    /// ComfyUI launch flags. A change here restarts the engine.
    pub launch_flags: Vec<String>,
    pub vae_tiled: bool,
    /// Input values the planner recommends (e.g. `weight_dtype: fp8_e4m3fn`); only applied
    /// where the user left the default.
    #[serde(default)]
    pub value_overrides: std::collections::BTreeMap<String, serde_json::Value>,
    pub vram_budget_gb: f64,
    /// Total weights that must be held in RAM or VRAM.
    #[serde(default)]
    pub weights_gb: f64,
    /// Files that still have to be downloaded, with sizes.
    #[serde(default)]
    pub downloads: Vec<(String, f64)>,
    pub estimated_vram_gb: f64,
    pub estimated_seconds: Option<f64>,
    /// Plain-language reasons, shown to the user.
    pub explain: Vec<String>,
    pub warnings: Vec<String>,
}
