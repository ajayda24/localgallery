//! Events pushed from the core to the UI over the WebSocket.

use crate::job::{Job, Output};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum CoreEvent {
    JobUpdated {
        job: Box<Job>,
    },
    Progress {
        job_id: String,
        step: u32,
        total: u32,
        /// 0..1 over the whole job (all nodes), best effort.
        overall: f64,
        node: Option<String>,
        eta_seconds: Option<f64>,
    },
    /// Live preview frame; `data` is a base64 JPEG/PNG data URL.
    Preview {
        job_id: String,
        data: String,
    },
    OutputAdded {
        output: Box<Output>,
    },
    Log {
        source: String,
        line: String,
    },
    EngineStatus {
        status: EngineState,
        detail: String,
    },
    Download {
        id: String,
        name: String,
        received: u64,
        total: Option<u64>,
        state: String,
        error: Option<String>,
    },
    ManifestsChanged,
    Hardware {
        vram_used_gb: f64,
        vram_total_gb: f64,
        ram_used_gb: f64,
        ram_total_gb: f64,
        shared_gpu_gb: Option<f64>,
        gpu_util: Option<f64>,
    },
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum EngineState {
    NotInstalled,
    Installing,
    Stopped,
    Starting,
    Ready,
    Busy,
    Error,
}
