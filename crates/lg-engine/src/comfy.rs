//! ComfyUI HTTP + WebSocket client.
//!
//! Endpoints verified against ComfyUI `server.py`: `POST /prompt` (with our own
//! `prompt_id` UUID and `client_id`), `GET /ws?clientId=`, `GET /history/{id}`,
//! `GET /view`, `POST /upload/image`, `POST /interrupt` (with `prompt_id`),
//! `POST /queue` (`delete`), `POST /free`, `GET /object_info`, `GET /system_stats`.

use crate::{EngineError, EngineEvent, EngineOutput};
use futures::{SinkExt, StreamExt};
use serde_json::{Value, json};
use std::time::Duration;
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::Message;
use tokio_util::sync::CancellationToken;

#[derive(Clone)]
pub struct ComfyClient {
    base: String,
    http: reqwest::Client,
}

/// A parsed WebSocket message.
#[derive(Debug, Clone, PartialEq)]
pub enum WsMsg {
    Status {
        queue_remaining: u64,
    },
    ExecutionStart {
        prompt_id: String,
    },
    Executing {
        prompt_id: Option<String>,
        node: Option<String>,
    },
    Progress {
        prompt_id: Option<String>,
        node: Option<String>,
        value: u32,
        max: u32,
    },
    Executed {
        prompt_id: Option<String>,
        node: String,
        output: Value,
    },
    Cached {
        prompt_id: String,
        nodes: Vec<String>,
    },
    Success {
        prompt_id: String,
    },
    Error {
        prompt_id: String,
        message: String,
        exception_type: String,
        node_type: Option<String>,
    },
    Interrupted {
        prompt_id: String,
    },
    /// Binary preview: mime type and image bytes.
    Preview {
        mime: &'static str,
        bytes: Vec<u8>,
    },
    Other,
}

pub fn parse_text(text: &str) -> WsMsg {
    let Ok(v) = serde_json::from_str::<Value>(text) else {
        return WsMsg::Other;
    };
    let d = &v["data"];
    let s = |k: &str| d.get(k).and_then(Value::as_str).map(str::to_string);
    let pid = || s("prompt_id").unwrap_or_default();
    match v["type"].as_str().unwrap_or_default() {
        "status" => WsMsg::Status {
            queue_remaining: d
                .pointer("/status/exec_info/queue_remaining")
                .and_then(Value::as_u64)
                .unwrap_or(0),
        },
        "execution_start" => WsMsg::ExecutionStart { prompt_id: pid() },
        "executing" => WsMsg::Executing {
            prompt_id: s("prompt_id"),
            node: s("node"),
        },
        "progress" => WsMsg::Progress {
            prompt_id: s("prompt_id"),
            node: s("node"),
            value: d["value"].as_u64().unwrap_or(0) as u32,
            max: d["max"].as_u64().unwrap_or(0) as u32,
        },
        "executed" => WsMsg::Executed {
            prompt_id: s("prompt_id"),
            node: s("node").unwrap_or_default(),
            output: d["output"].clone(),
        },
        "execution_cached" => WsMsg::Cached {
            prompt_id: pid(),
            nodes: d["nodes"]
                .as_array()
                .map(|a| {
                    a.iter()
                        .filter_map(|x| x.as_str().map(str::to_string))
                        .collect()
                })
                .unwrap_or_default(),
        },
        "execution_success" => WsMsg::Success { prompt_id: pid() },
        "execution_error" => WsMsg::Error {
            prompt_id: pid(),
            message: s("exception_message").unwrap_or_default(),
            exception_type: s("exception_type").unwrap_or_default(),
            node_type: s("node_type"),
        },
        "execution_interrupted" => WsMsg::Interrupted { prompt_id: pid() },
        _ => WsMsg::Other,
    }
}

/// Binary frames: 4-byte big-endian event type, then for previews a 4-byte image type
/// (1 = JPEG, 2 = PNG) and the image bytes. Type 4 carries a JSON metadata block first.
pub fn parse_binary(b: &[u8]) -> WsMsg {
    if b.len() < 8 {
        return WsMsg::Other;
    }
    let event = u32::from_be_bytes([b[0], b[1], b[2], b[3]]);
    match event {
        1 => {
            let fmt = u32::from_be_bytes([b[4], b[5], b[6], b[7]]);
            WsMsg::Preview {
                mime: if fmt == 2 { "image/png" } else { "image/jpeg" },
                bytes: b[8..].to_vec(),
            }
        }
        4 => {
            let len = u32::from_be_bytes([b[4], b[5], b[6], b[7]]) as usize;
            let Some(meta) = b.get(8..8 + len) else {
                return WsMsg::Other;
            };
            let mime = serde_json::from_slice::<Value>(meta)
                .ok()
                .and_then(|m| {
                    m["image_type"].as_str().map(|t| {
                        if t.contains("png") {
                            "image/png"
                        } else {
                            "image/jpeg"
                        }
                    })
                })
                .unwrap_or("image/jpeg");
            WsMsg::Preview {
                mime,
                bytes: b[8 + len..].to_vec(),
            }
        }
        _ => WsMsg::Other,
    }
}

/// Out-of-memory errors are retried with a lighter plan.
pub fn is_oom(exception_type: &str, message: &str) -> bool {
    let m = message.to_ascii_lowercase();
    exception_type.contains("OutOfMemory")
        || m.contains("out of memory")
        || m.contains("allocation on device")
        || m.contains("cuda error: out of memory")
}

impl ComfyClient {
    pub fn new(base: impl Into<String>) -> Self {
        let http = reqwest::Client::builder()
            .no_proxy()
            .connect_timeout(Duration::from_secs(5))
            .build()
            .expect("http client");
        Self {
            base: base.into().trim_end_matches('/').to_string(),
            http,
        }
    }

    pub fn base(&self) -> &str {
        &self.base
    }

    async fn get_json(&self, path: &str) -> Result<Value, EngineError> {
        let r = self
            .http
            .get(format!("{}{path}", self.base))
            .send()
            .await
            .map_err(EngineError::unreachable)?;
        if !r.status().is_success() {
            return Err(EngineError::Http(format!("GET {path}: {}", r.status())));
        }
        r.json().await.map_err(|e| EngineError::Http(e.to_string()))
    }

    async fn post_json(&self, path: &str, body: &Value) -> Result<reqwest::Response, EngineError> {
        self.http
            .post(format!("{}{path}", self.base))
            .json(body)
            .send()
            .await
            .map_err(EngineError::unreachable)
    }

    pub async fn system_stats(&self) -> Result<Value, EngineError> {
        self.get_json("/system_stats").await
    }

    pub async fn object_info(&self) -> Result<Value, EngineError> {
        self.get_json("/object_info").await
    }

    pub async fn history(&self, prompt_id: &str) -> Result<Option<Value>, EngineError> {
        let v = self.get_json(&format!("/history/{prompt_id}")).await?;
        Ok(v.get(prompt_id).cloned())
    }

    /// The prompt's history, polling for up to `wait` while it is not stored yet.
    async fn history_soon(
        &self,
        prompt_id: &str,
        wait: Duration,
    ) -> Result<Option<Value>, EngineError> {
        let deadline = tokio::time::Instant::now() + wait;
        loop {
            if let Some(h) = self.history(prompt_id).await? {
                return Ok(Some(h));
            }
            if tokio::time::Instant::now() >= deadline {
                return Ok(None);
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }

    /// Queue a graph. Validation failures come back as `EngineError::Rejected`.
    pub async fn queue_prompt(
        &self,
        graph: &Value,
        prompt_id: &str,
        client_id: &str,
    ) -> Result<(), EngineError> {
        let r = self
            .post_json(
                "/prompt",
                &json!({"prompt": graph, "prompt_id": prompt_id, "client_id": client_id}),
            )
            .await?;
        let status = r.status();
        let body: Value = r.json().await.unwrap_or(Value::Null);
        let node_errors = body["node_errors"]
            .as_object()
            .is_some_and(|o| !o.is_empty());
        if status.is_success() && !node_errors {
            return Ok(());
        }
        if status.is_success() {
            // ComfyUI queues whichever outputs validated and reports the rest; a partial
            // run would look like success, so withdraw it.
            let _ = self.delete_queued(prompt_id).await;
            let _ = self.interrupt(prompt_id).await;
        }
        Err(EngineError::Rejected(describe_rejection(&body)))
    }

    pub async fn interrupt(&self, prompt_id: &str) -> Result<(), EngineError> {
        self.post_json("/interrupt", &json!({"prompt_id": prompt_id}))
            .await?;
        Ok(())
    }

    pub async fn delete_queued(&self, prompt_id: &str) -> Result<(), EngineError> {
        self.post_json("/queue", &json!({"delete": [prompt_id]}))
            .await?;
        Ok(())
    }

    /// Ask ComfyUI to unload models and free memory.
    pub async fn free(&self, unload_models: bool) -> Result<(), EngineError> {
        self.post_json(
            "/free",
            &json!({"unload_models": unload_models, "free_memory": true}),
        )
        .await?;
        Ok(())
    }

    pub async fn view(&self, out: &EngineOutput) -> Result<Vec<u8>, EngineError> {
        let r = self
            .http
            .get(format!("{}/view", self.base))
            .query(&[
                ("filename", &out.filename),
                ("subfolder", &out.subfolder),
                ("type", &out.kind),
            ])
            .send()
            .await
            .map_err(EngineError::unreachable)?;
        if !r.status().is_success() {
            return Err(EngineError::Http(format!(
                "view {}: {}",
                out.filename,
                r.status()
            )));
        }
        Ok(r.bytes()
            .await
            .map_err(|e| EngineError::Http(e.to_string()))?
            .to_vec())
    }

    /// Upload into the input folder; returns the name nodes should reference.
    pub async fn upload_image(&self, bytes: Vec<u8>, name: &str) -> Result<String, EngineError> {
        let part = reqwest::multipart::Part::bytes(bytes).file_name(name.to_string());
        let form = reqwest::multipart::Form::new()
            .part("image", part)
            .text("type", "input")
            .text("overwrite", "true");
        let r = self
            .http
            .post(format!("{}/upload/image", self.base))
            .multipart(form)
            .send()
            .await
            .map_err(EngineError::unreachable)?;
        if !r.status().is_success() {
            return Err(EngineError::Http(format!("upload {name}: {}", r.status())));
        }
        let v: Value = r
            .json()
            .await
            .map_err(|e| EngineError::Http(e.to_string()))?;
        let file = v["name"].as_str().unwrap_or(name).to_string();
        Ok(match v["subfolder"].as_str() {
            Some(sub) if !sub.is_empty() => format!("{sub}/{file}"),
            _ => file,
        })
    }

    fn ws_url(&self, client_id: &str) -> String {
        let b = self
            .base
            .replacen("http://", "ws://", 1)
            .replacen("https://", "wss://", 1);
        format!("{b}/ws?clientId={client_id}")
    }

    /// Submit a graph and follow it to completion.
    pub async fn run(
        &self,
        graph: &Value,
        events: &mpsc::Sender<EngineEvent>,
        cancel: &CancellationToken,
    ) -> Result<Vec<EngineOutput>, EngineError> {
        let client_id = uuid::Uuid::new_v4().to_string();
        let prompt_id = uuid::Uuid::new_v4().to_string();
        let (mut ws, _) = tokio_tungstenite::connect_async(self.ws_url(&client_id))
            .await
            .map_err(|e| EngineError::Unreachable(format!("websocket: {e}")))?;
        self.queue_prompt(graph, &prompt_id, &client_id).await?;
        let total_nodes = graph.as_object().map(|o| o.len()).unwrap_or(1).max(1);
        let mut done_nodes = 0usize;
        let mut cancelled = false;
        let mut last_ping = tokio::time::Instant::now();
        loop {
            let msg = tokio::select! {
                _ = cancel.cancelled(), if !cancelled => {
                    cancelled = true;
                    let _ = self.delete_queued(&prompt_id).await;
                    let _ = self.interrupt(&prompt_id).await;
                    // ComfyUI reports the interruption; if the job never started, history tells us.
                    tokio::time::sleep(Duration::from_millis(300)).await;
                    if self.history(&prompt_id).await.ok().flatten().is_none() && !self.is_running(&prompt_id).await {
                        return Err(EngineError::Cancelled);
                    }
                    continue;
                }
                m = tokio::time::timeout(Duration::from_secs(20), ws.next()) => m,
            };
            let msg = match msg {
                Ok(Some(Ok(m))) => m,
                Ok(Some(Err(e))) => {
                    return Err(EngineError::Unreachable(format!("websocket: {e}")));
                }
                Ok(None) => {
                    return Err(EngineError::Unreachable(
                        "engine closed the connection".into(),
                    ));
                }
                Err(_) => {
                    // Quiet period (long node). Check the engine is still alive.
                    if last_ping.elapsed() > Duration::from_secs(15) {
                        self.system_stats().await?;
                        last_ping = tokio::time::Instant::now();
                    }
                    if let Some(h) = self.history(&prompt_id).await? {
                        return finish_from_history(&h, cancelled);
                    }
                    continue;
                }
            };
            let parsed = match &msg {
                Message::Text(t) => parse_text(t.as_str()),
                Message::Binary(b) => parse_binary(b),
                Message::Ping(p) => {
                    let _ = ws.send(Message::Pong(p.clone())).await;
                    continue;
                }
                _ => continue,
            };
            let mine = |id: &Option<String>| id.as_deref().is_none_or(|i| i == prompt_id);
            match parsed {
                WsMsg::Progress {
                    prompt_id: p,
                    node,
                    value,
                    max,
                } if mine(&p) => {
                    let _ = events.try_send(EngineEvent::Progress {
                        step: value,
                        total: max,
                        node,
                        overall: (done_nodes as f64 + value as f64 / max.max(1) as f64)
                            / total_nodes as f64,
                    });
                }
                WsMsg::Executing {
                    prompt_id: p,
                    node: Some(node),
                } if mine(&p) => {
                    done_nodes += 1;
                    let _ = events.try_send(EngineEvent::Node {
                        node,
                        overall: done_nodes as f64 / total_nodes as f64,
                    });
                }
                WsMsg::Cached {
                    prompt_id: p,
                    nodes,
                } if p == prompt_id => done_nodes += nodes.len(),
                WsMsg::Preview { mime, bytes } => {
                    let _ = events.try_send(EngineEvent::Preview {
                        mime: mime.to_string(),
                        bytes,
                    });
                }
                // ComfyUI sends `execution_success` before it stores the history, and the final
                // `executing: null` after, so a fast (e.g. fully cached) run can get here first.
                WsMsg::Success { prompt_id: p } if p == prompt_id => {
                    if let Some(h) = self
                        .history_soon(&prompt_id, Duration::from_millis(500))
                        .await?
                    {
                        return finish_from_history(&h, false);
                    }
                }
                WsMsg::Error {
                    prompt_id: p,
                    message,
                    exception_type,
                    node_type,
                } if p == prompt_id => {
                    if is_oom(&exception_type, &message) {
                        return Err(EngineError::OutOfMemory(message));
                    }
                    let at = node_type.map(|n| format!(" (in {n})")).unwrap_or_default();
                    return Err(EngineError::Failed(format!("{}{at}", message.trim())));
                }
                WsMsg::Interrupted { prompt_id: p } if p == prompt_id => {
                    return Err(EngineError::Cancelled);
                }
                WsMsg::Executing {
                    prompt_id: Some(p),
                    node: None,
                } if p == prompt_id => {
                    // Sent after the history is stored (and the only signal from older servers).
                    let h = self
                        .history_soon(&prompt_id, Duration::from_secs(5))
                        .await?
                        .ok_or_else(|| {
                            EngineError::Failed("engine finished but returned no history".into())
                        })?;
                    return finish_from_history(&h, cancelled);
                }
                _ => {}
            }
        }
    }

    async fn is_running(&self, prompt_id: &str) -> bool {
        let Ok(q) = self.get_json("/queue").await else {
            return false;
        };
        ["queue_running", "queue_pending"].iter().any(|k| {
            q[k].as_array()
                .into_iter()
                .flatten()
                .any(|item| item.get(1).and_then(Value::as_str) == Some(prompt_id))
        })
    }
}

fn finish_from_history(h: &Value, cancelled: bool) -> Result<Vec<EngineOutput>, EngineError> {
    let status = h
        .pointer("/status/status_str")
        .and_then(Value::as_str)
        .unwrap_or("success");
    if status == "error" {
        let msg = h
            .pointer("/status/messages")
            .and_then(Value::as_array)
            .and_then(|m| {
                m.iter().find_map(|e| {
                    (e[0] == "execution_error").then(|| {
                        e[1]["exception_message"]
                            .as_str()
                            .unwrap_or("error")
                            .to_string()
                    })
                })
            })
            .unwrap_or_else(|| "the engine reported an error".into());
        return Err(if is_oom("", &msg) {
            EngineError::OutOfMemory(msg)
        } else {
            EngineError::Failed(msg)
        });
    }
    if cancelled {
        return Err(EngineError::Cancelled);
    }
    Ok(collect_outputs(&h["outputs"]))
}

/// Files from history outputs. Prefers saved outputs; falls back to temp previews.
pub fn collect_outputs(outputs: &Value) -> Vec<EngineOutput> {
    let mut saved = Vec::new();
    let mut temp = Vec::new();
    for (node, out) in outputs.as_object().into_iter().flatten() {
        let animated = out["animated"]
            .as_array()
            .is_some_and(|a| a.iter().any(|x| x.as_bool() == Some(true)));
        for key in ["images", "gifs", "videos", "audio"] {
            for f in out[key].as_array().into_iter().flatten() {
                let Some(filename) = f["filename"].as_str() else {
                    continue;
                };
                let o = EngineOutput {
                    node: node.clone(),
                    filename: filename.to_string(),
                    subfolder: f["subfolder"].as_str().unwrap_or_default().to_string(),
                    kind: f["type"].as_str().unwrap_or("output").to_string(),
                    animated: animated || key != "images",
                };
                if o.kind == "output" {
                    saved.push(o)
                } else {
                    temp.push(o)
                }
            }
        }
    }
    if saved.is_empty() { temp } else { saved }
}

fn describe_rejection(body: &Value) -> String {
    let mut parts = Vec::new();
    if let Some(m) = body.pointer("/error/message").and_then(Value::as_str) {
        parts.push(m.to_string());
    }
    for (node, e) in body["node_errors"].as_object().into_iter().flatten() {
        let class = e["class_type"].as_str().unwrap_or("?");
        for err in e["errors"].as_array().into_iter().flatten() {
            let detail = err["details"]
                .as_str()
                .filter(|d| !d.is_empty())
                .or(err["message"].as_str())
                .unwrap_or("");
            parts.push(format!("node {node} ({class}): {detail}"));
        }
    }
    if parts.is_empty() {
        "the engine rejected the workflow".into()
    } else {
        parts.join("; ")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_messages() {
        assert_eq!(
            parse_text(
                r#"{"type":"progress","data":{"value":3,"max":20,"prompt_id":"p","node":"3"}}"#
            ),
            WsMsg::Progress {
                prompt_id: Some("p".into()),
                node: Some("3".into()),
                value: 3,
                max: 20
            }
        );
        assert!(matches!(
            parse_text(
                r#"{"type":"execution_error","data":{"prompt_id":"p","exception_type":"torch.OutOfMemoryError","exception_message":"CUDA out of memory"}}"#
            ),
            WsMsg::Error { .. }
        ));
        let mut b = vec![0, 0, 0, 1, 0, 0, 0, 2];
        b.extend_from_slice(b"png");
        assert_eq!(
            parse_binary(&b),
            WsMsg::Preview {
                mime: "image/png",
                bytes: b"png".to_vec()
            }
        );
    }

    #[test]
    fn outputs_prefer_saved() {
        let h = serde_json::json!({
            "9": {"images": [{"filename": "a.png", "subfolder": "lg", "type": "output"}]},
            "10": {"images": [{"filename": "p.png", "subfolder": "", "type": "temp"}]},
            "11": {"images": [{"filename": "v.mp4", "subfolder": "", "type": "output"}], "animated": [true]}
        });
        let o = collect_outputs(&h);
        assert_eq!(o.len(), 2);
        assert!(o.iter().any(|x| x.filename == "v.mp4" && x.animated));
    }

    #[test]
    fn rejection_text() {
        let body = serde_json::json!({"error": {"message": "Prompt outputs failed validation"},
            "node_errors": {"4": {"class_type": "CheckpointLoaderSimple", "errors": [{"message": "Value not in list", "details": "ckpt_name: 'x' not in []"}]}}});
        let s = describe_rejection(&body);
        assert!(s.contains("node 4 (CheckpointLoaderSimple): ckpt_name"));
        assert!(is_oom(
            "",
            "Allocation on device 0 would exceed allowed memory"
        ));
    }
}
