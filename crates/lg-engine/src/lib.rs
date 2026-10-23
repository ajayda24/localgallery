//! Engine adapters. ComfyUI is the primary engine; others plug in behind [`Engine`].

pub mod comfy;
pub mod install;
pub mod supervisor;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

#[derive(Debug, Clone, thiserror::Error, PartialEq)]
pub enum EngineError {
    #[error("engine is not reachable: {0}")]
    Unreachable(String),
    #[error("engine rejected the workflow: {0}")]
    Rejected(String),
    #[error("out of GPU memory: {0}")]
    OutOfMemory(String),
    #[error("{0}")]
    Failed(String),
    #[error("cancelled")]
    Cancelled,
    #[error("engine HTTP error: {0}")]
    Http(String),
}

impl EngineError {
    pub fn unreachable(e: reqwest::Error) -> Self {
        EngineError::Unreachable(e.to_string())
    }
}

/// Streamed while a job runs.
#[derive(Debug, Clone, PartialEq)]
pub enum EngineEvent {
    Progress {
        step: u32,
        total: u32,
        node: Option<String>,
        overall: f64,
    },
    Node {
        node: String,
        overall: f64,
    },
    Preview {
        mime: String,
        bytes: Vec<u8>,
    },
}

/// A file produced by the engine.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct EngineOutput {
    pub node: String,
    pub filename: String,
    pub subfolder: String,
    /// "output" or "temp".
    pub kind: String,
    pub animated: bool,
}

#[async_trait]
pub trait Engine: Send + Sync {
    fn name(&self) -> &str;
    async fn healthy(&self) -> bool;
    async fn object_info(&self) -> Result<Value, EngineError>;
    async fn upload(&self, bytes: Vec<u8>, name: &str) -> Result<String, EngineError>;
    async fn run(
        &self,
        graph: &Value,
        events: mpsc::Sender<EngineEvent>,
        cancel: CancellationToken,
    ) -> Result<Vec<EngineOutput>, EngineError>;
    async fn fetch(&self, out: &EngineOutput) -> Result<Vec<u8>, EngineError>;
    async fn free(&self) -> Result<(), EngineError>;
}

pub struct ComfyEngine {
    pub client: comfy::ComfyClient,
}

#[async_trait]
impl Engine for ComfyEngine {
    fn name(&self) -> &str {
        "comfyui"
    }
    async fn healthy(&self) -> bool {
        self.client.system_stats().await.is_ok()
    }
    async fn object_info(&self) -> Result<Value, EngineError> {
        self.client.object_info().await
    }
    async fn upload(&self, bytes: Vec<u8>, name: &str) -> Result<String, EngineError> {
        self.client.upload_image(bytes, name).await
    }
    async fn run(
        &self,
        graph: &Value,
        events: mpsc::Sender<EngineEvent>,
        cancel: CancellationToken,
    ) -> Result<Vec<EngineOutput>, EngineError> {
        self.client.run(graph, &events, &cancel).await
    }
    async fn fetch(&self, out: &EngineOutput) -> Result<Vec<u8>, EngineError> {
        self.client.view(out).await
    }
    async fn free(&self) -> Result<(), EngineError> {
        self.client.free(true).await
    }
}
