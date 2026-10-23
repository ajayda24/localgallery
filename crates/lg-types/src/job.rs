//! Engine-neutral job description (the IR) and job records.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;

pub const JOB_SPEC_VERSION: u32 = 1;

/// What the user asked for. Values are keyed by manifest input id, so the spec
/// stays the same shape for every model.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct JobSpec {
    #[serde(default = "spec_version")]
    pub version: u32,
    /// Manifest id (a model or a published tool).
    pub model: String,
    /// Operation name from the manifest (txt2img, img2img, inpaint, img2vid, ...).
    pub op: String,
    #[serde(default)]
    pub values: BTreeMap<String, Value>,
    /// How many times to run with consecutive seeds.
    #[serde(default = "one")]
    pub batch: u32,
    #[serde(default)]
    pub memory: MemoryPrefs,
    /// Raw overrides applied to the final graph: `"node_id.input" -> value`.
    #[serde(default)]
    pub overrides: BTreeMap<String, Value>,
}

fn spec_version() -> u32 {
    JOB_SPEC_VERSION
}
fn one() -> u32 {
    1
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct MemoryPrefs {
    /// "auto" or a forced fit: resident | offload | stream.
    #[serde(default)]
    pub plan: Option<String>,
    /// Force a weight precision variant (e.g. "fp8").
    #[serde(default)]
    pub precision: Option<String>,
    /// Force tiled VAE decode on or off.
    #[serde(default)]
    pub vae_tiled: Option<bool>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum JobStatus {
    Queued,
    Running,
    Done,
    Failed,
    Cancelled,
}

impl JobStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            JobStatus::Queued => "queued",
            JobStatus::Running => "running",
            JobStatus::Done => "done",
            JobStatus::Failed => "failed",
            JobStatus::Cancelled => "cancelled",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "queued" => JobStatus::Queued,
            "running" => JobStatus::Running,
            "done" => JobStatus::Done,
            "failed" => JobStatus::Failed,
            "cancelled" => JobStatus::Cancelled,
            _ => return None,
        })
    }

    pub fn is_final(self) -> bool {
        matches!(
            self,
            JobStatus::Done | JobStatus::Failed | JobStatus::Cancelled
        )
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Job {
    pub id: String,
    pub spec: JobSpec,
    pub status: JobStatus,
    /// Lower runs first.
    pub position: i64,
    pub created_at: i64,
    pub started_at: Option<i64>,
    pub finished_at: Option<i64>,
    pub error: Option<String>,
    pub outputs: Vec<String>,
    pub metrics: Option<RunMetrics>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct RunMetrics {
    pub seconds: f64,
    pub peak_vram_gb: Option<f64>,
    pub peak_ram_gb: Option<f64>,
    pub shared_gpu_gb: Option<f64>,
    pub plan: String,
    pub precision: Option<String>,
}

/// One generated file in the gallery.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Output {
    pub id: String,
    pub job_id: String,
    /// Path relative to the data dir.
    pub path: String,
    pub thumb: Option<String>,
    pub media: String,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub model: String,
    pub op: String,
    pub prompt: String,
    pub seed: Option<i64>,
    pub created_at: i64,
    pub favorite: bool,
}
