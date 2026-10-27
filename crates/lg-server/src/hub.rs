//! Browse Hugging Face model repos (Comfy-Org by default) from the app: list repos, list a
//! repo's model files with real sizes and the ComfyUI folder each belongs in. Downloads then go
//! through the normal download queue. The access token is only ever sent to Hugging Face.

use parking_lot::Mutex;
use serde::Serialize;
use serde_json::Value;
use std::collections::HashMap;
use std::time::{Duration, Instant};

const CACHE_FOR: Duration = Duration::from_secs(600);

pub struct Hub {
    http: reqwest::Client,
    cache: Mutex<HashMap<String, (Instant, Value)>>,
}

#[derive(Debug)]
pub enum HubError {
    /// 401/403: gated or private repo.
    Denied(String),
    NotFound,
    Other(String),
}

impl std::fmt::Display for HubError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            HubError::Denied(m) => write!(
                f,
                "Hugging Face refused access ({m}). For gated models, accept the license on huggingface.co and add your access token in Settings."
            ),
            HubError::NotFound => write!(f, "Not found on Hugging Face."),
            HubError::Other(m) => write!(f, "Could not reach Hugging Face: {m}"),
        }
    }
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Repo {
    pub id: String,
    pub name: String,
    pub updated: Option<String>,
    pub downloads: Option<u64>,
    pub likes: Option<u64>,
    pub gated: bool,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct RepoFile {
    /// Path inside the repo.
    pub path: String,
    /// File name it is saved as.
    pub name: String,
    /// ComfyUI model folder, when the repo layout says (`split_files/<folder>/...`).
    pub folder: Option<String>,
    pub size_bytes: u64,
}

/// Only plain `owner/name` repo ids; nothing that could escape the API path.
pub fn valid_repo(id: &str) -> bool {
    let ok = |s: &str| {
        !s.is_empty()
            && s.len() <= 96
            && s != "."
            && s != ".."
            && s.chars()
                .all(|c| c.is_ascii_alphanumeric() || "-_.".contains(c))
    };
    matches!(id.split_once('/'), Some((a, b)) if ok(a) && ok(b))
}

/// The model folder a repo path belongs in: the deepest path component that is a ComfyUI
/// model folder (`split_files/diffusion_models/x.safetensors` -> `diffusion_models`).
/// Old folder names map to the current ones.
pub fn folder_for(path: &str) -> Option<String> {
    let parts: Vec<&str> = path.split('/').collect();
    parts[..parts.len().saturating_sub(1)]
        .iter()
        .rev()
        .find_map(|p| {
            let p = p.to_ascii_lowercase();
            let p = match p.as_str() {
                "unet" => "diffusion_models".to_string(),
                "clip" => "text_encoders".to_string(),
                _ => p,
            };
            lg_modelscan::FOLDERS.contains(&p.as_str()).then_some(p)
        })
}

pub fn is_model_file(path: &str) -> bool {
    let ext = path.rsplit('.').next().unwrap_or("").to_ascii_lowercase();
    ["safetensors", "sft", "gguf", "ckpt", "pt", "pth", "bin"].contains(&ext.as_str())
        && !path.rsplit('/').next().unwrap_or("").starts_with('.')
}

impl Default for Hub {
    fn default() -> Self {
        Self::new()
    }
}

impl Hub {
    pub fn new() -> Self {
        Self {
            http: reqwest::Client::builder()
                .connect_timeout(Duration::from_secs(10))
                .timeout(Duration::from_secs(30))
                .user_agent(concat!("LocalGallery/", env!("CARGO_PKG_VERSION")))
                .build()
                .unwrap_or_default(),
            cache: Mutex::new(HashMap::new()),
        }
    }

    /// GET a JSON document (following `Link: rel="next"` pages for lists), cached for ten minutes.
    async fn get(&self, url: &str, token: Option<&str>) -> Result<Value, HubError> {
        if let Some((at, v)) = self.cache.lock().get(url)
            && at.elapsed() < CACHE_FOR
        {
            return Ok(v.clone());
        }
        let mut next = Some(url.to_string());
        let mut items: Vec<Value> = Vec::new();
        let mut single: Option<Value> = None;
        for _ in 0..20 {
            let Some(u) = next.take() else { break };
            let mut req = self.http.get(&u);
            if let Some(t) = token.filter(|_| lg_downloader::is_hf(&u)) {
                req = req.bearer_auth(t);
            }
            let r = req
                .send()
                .await
                .map_err(|e| HubError::Other(e.to_string()))?;
            match r.status().as_u16() {
                401 | 403 => return Err(HubError::Denied(r.status().to_string())),
                404 => return Err(HubError::NotFound),
                s if !(200..300).contains(&s) => {
                    return Err(HubError::Other(format!("HTTP {s}")));
                }
                _ => {}
            }
            next = r
                .headers()
                .get("link")
                .and_then(|l| l.to_str().ok())
                .and_then(next_link)
                .map(|n| {
                    reqwest::Url::parse(&u)
                        .and_then(|b| b.join(&n))
                        .map(|x| x.to_string())
                        .unwrap_or(n)
                });
            let v: Value = r.json().await.map_err(|e| HubError::Other(e.to_string()))?;
            match v {
                Value::Array(a) => items.extend(a),
                other => {
                    single = Some(other);
                    break;
                }
            }
        }
        let v = single.unwrap_or(Value::Array(items));
        self.cache
            .lock()
            .insert(url.to_string(), (Instant::now(), v.clone()));
        Ok(v)
    }

    pub async fn repos(
        &self,
        endpoint: &str,
        author: &str,
        search: &str,
        token: Option<&str>,
    ) -> Result<Vec<Repo>, HubError> {
        let mut url = reqwest::Url::parse(&format!("{endpoint}/api/models"))
            .map_err(|e| HubError::Other(e.to_string()))?;
        {
            let mut q = url.query_pairs_mut();
            if !author.is_empty() {
                q.append_pair("author", author);
            }
            if !search.is_empty() {
                q.append_pair("search", search);
            }
            q.append_pair("sort", "lastModified")
                .append_pair("direction", "-1")
                .append_pair("limit", "500");
            for f in ["downloads", "likes", "lastModified", "gated"] {
                q.append_pair("expand[]", f);
            }
        }
        let v = self.get(url.as_str(), token).await?;
        Ok(v.as_array()
            .into_iter()
            .flatten()
            .filter_map(|r| {
                let id = r.get("id").or(r.get("modelId"))?.as_str()?.to_string();
                Some(Repo {
                    name: id
                        .split_once('/')
                        .map(|(_, n)| n)
                        .unwrap_or(&id)
                        .to_string(),
                    updated: r
                        .get("lastModified")
                        .and_then(Value::as_str)
                        .map(str::to_string),
                    downloads: r.get("downloads").and_then(Value::as_u64),
                    likes: r.get("likes").and_then(Value::as_u64),
                    gated: r
                        .get("gated")
                        .is_some_and(|g| g.as_bool() != Some(false) && !g.is_null()),
                    id,
                })
            })
            .collect())
    }

    /// Model files in a repo, plus how many other files were left out.
    pub async fn files(
        &self,
        endpoint: &str,
        repo: &str,
        token: Option<&str>,
    ) -> Result<(Vec<RepoFile>, usize), HubError> {
        if !valid_repo(repo) {
            return Err(HubError::NotFound);
        }
        let v = self
            .get(
                &format!("{endpoint}/api/models/{repo}/tree/main?recursive=true"),
                token,
            )
            .await?;
        let mut files = Vec::new();
        let mut other = 0;
        for e in v.as_array().into_iter().flatten() {
            if e.get("type").and_then(Value::as_str) != Some("file") {
                continue;
            }
            let Some(path) = e.get("path").and_then(Value::as_str) else {
                continue;
            };
            if !is_model_file(path) {
                other += 1;
                continue;
            }
            let size = e
                .pointer("/lfs/size")
                .and_then(Value::as_u64)
                .or_else(|| e.get("size").and_then(Value::as_u64))
                .unwrap_or(0);
            files.push(RepoFile {
                path: path.to_string(),
                name: path.rsplit('/').next().unwrap_or(path).to_string(),
                folder: folder_for(path),
                size_bytes: size,
            });
        }
        files.sort_by(|a, b| {
            (a.folder.is_none(), &a.folder, &a.path).cmp(&(b.folder.is_none(), &b.folder, &b.path))
        });
        Ok((files, other))
    }
}

/// `<https://...?cursor=x>; rel="next"` -> the URL.
fn next_link(header: &str) -> Option<String> {
    header.split(',').find_map(|part| {
        let (url, rel) = part.split_once(';')?;
        rel.contains("rel=\"next\"").then(|| {
            url.trim()
                .trim_start_matches('<')
                .trim_end_matches('>')
                .to_string()
        })
    })
}

/// Download URL of a repo file (always the canonical huggingface.co form; see `sizes::via`).
pub fn resolve_url(repo: &str, path: &str) -> String {
    let path: String = path
        .split('/')
        .map(|p| {
            p.bytes()
                .map(|b| match b {
                    b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                        (b as char).to_string()
                    }
                    _ => format!("%{b:02X}"),
                })
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("/");
    format!("{}/{repo}/resolve/main/{path}", crate::sizes::HF)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repo_ids() {
        assert!(valid_repo("Comfy-Org/MOSS-Image"));
        assert!(valid_repo("city96/FLUX.1-dev-gguf"));
        for bad in [
            "Comfy-Org",
            "a/b/c",
            "../x",
            "a/..",
            "a b/c",
            "a/b?x=1",
            "/b",
            "a/",
        ] {
            assert!(!valid_repo(bad), "{bad}");
        }
    }

    #[test]
    fn folders_from_repo_layout() {
        assert_eq!(
            folder_for("split_files/diffusion_models/z_image_turbo_bf16.safetensors").as_deref(),
            Some("diffusion_models")
        );
        assert_eq!(
            folder_for("split_files/text_encoders/qwen_3_4b.safetensors").as_deref(),
            Some("text_encoders")
        );
        assert_eq!(
            folder_for("split_files/vae/ae.safetensors").as_deref(),
            Some("vae")
        );
        assert_eq!(
            folder_for("unet/flux1-dev-Q4_K_S.gguf").as_deref(),
            Some("diffusion_models")
        );
        assert_eq!(
            folder_for("clip/t5xxl.gguf").as_deref(),
            Some("text_encoders")
        );
        assert_eq!(
            folder_for("loras/style/x.safetensors").as_deref(),
            Some("loras")
        );
        assert_eq!(folder_for("flux1-dev-Q8_0.gguf"), None);
        assert_eq!(
            folder_for("vae"),
            None,
            "a file called like a folder is not in it"
        );
        assert!(
            is_model_file("a/b.GGUF")
                && !is_model_file("README.md")
                && !is_model_file("x/.hidden.safetensors")
        );
    }

    #[test]
    fn urls_and_links() {
        assert_eq!(
            resolve_url(
                "Comfy-Org/z_image_turbo",
                "split_files/vae/ae v2.safetensors"
            ),
            "https://huggingface.co/Comfy-Org/z_image_turbo/resolve/main/split_files/vae/ae%20v2.safetensors"
        );
        assert_eq!(
            next_link("<https://huggingface.co/api/models?cursor=abc>; rel=\"next\"").as_deref(),
            Some("https://huggingface.co/api/models?cursor=abc")
        );
        assert_eq!(next_link("<https://x>; rel=\"prev\""), None);
    }
}
