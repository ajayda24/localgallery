//! Real file sizes for model downloads. Manifests often leave `size_gb` at 0 (unknown);
//! the planner must not read that as "0 GB, fits anywhere". Sizes are asked from the server
//! with a HEAD request (Hugging Face reports them on the redirect as `x-linked-size`) and kept
//! in `data/cache/file-sizes.json`, so each URL is asked once.

use parking_lot::RwLock;
use serde::{Deserialize, Serialize};
use std::borrow::Cow;
use std::collections::{BTreeMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;

use lg_types::Manifest;

const GB: f64 = 1024.0 * 1024.0 * 1024.0;

/// The public Hugging Face endpoint manifests link to.
pub const HF: &str = "https://huggingface.co";

/// `url` as seen through `endpoint` (a mirror or a test mock stands in for huggingface.co).
pub fn via(endpoint: &str, url: &str) -> String {
    match url.strip_prefix(HF) {
        Some(rest) if endpoint != HF && rest.starts_with('/') => format!("{endpoint}{rest}"),
        _ => url.to_string(),
    }
}

#[derive(Default, Serialize, Deserialize)]
struct Cache {
    /// URL -> size in bytes.
    sizes: BTreeMap<String, u64>,
}

pub struct RemoteSizes {
    path: PathBuf,
    cache: RwLock<Cache>,
    /// URLs already tried this session (failures are retried on the next launch).
    tried: RwLock<HashSet<String>>,
    /// One refresh at a time.
    busy: tokio::sync::Mutex<()>,
}

impl RemoteSizes {
    pub fn load(path: PathBuf) -> Arc<Self> {
        let cache = std::fs::read(&path)
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default();
        Arc::new(Self {
            path,
            cache: RwLock::new(cache),
            tried: RwLock::new(HashSet::new()),
            busy: tokio::sync::Mutex::new(()),
        })
    }

    pub fn get_gb(&self, url: &str) -> Option<f64> {
        self.cache.read().sizes.get(url).map(|b| *b as f64 / GB)
    }

    /// Remember a size learned elsewhere (e.g. from a Hugging Face file listing).
    pub fn insert(&self, url: &str, bytes: u64) {
        if bytes == 0 {
            return;
        }
        let changed = self.cache.write().sizes.insert(url.to_string(), bytes) != Some(bytes);
        if changed {
            self.save();
        }
    }

    fn save(&self) {
        let text = serde_json::to_vec_pretty(&*self.cache.read()).unwrap_or_default();
        if let Some(dir) = self.path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let tmp = self.path.with_extension("json.tmp");
        if std::fs::write(&tmp, text).is_ok() {
            let _ = std::fs::rename(&tmp, &self.path);
        }
    }

    /// The manifest with every unknown variant size filled in from the cache.
    pub fn apply<'a>(&self, m: &'a Manifest) -> Cow<'a, Manifest> {
        let cache = self.cache.read();
        let known =
            |v: &lg_types::FileVariant| v.url.as_ref().and_then(|u| cache.sizes.get(u)).copied();
        let needs = m
            .files
            .iter()
            .flat_map(|f| &f.variants)
            .any(|v| v.size_gb <= 0.0 && known(v).is_some());
        if !needs {
            return Cow::Borrowed(m);
        }
        let mut m = m.clone();
        for v in m.files.iter_mut().flat_map(|f| f.variants.iter_mut()) {
            if v.size_gb <= 0.0
                && let Some(b) = known(v)
            {
                v.size_gb = b as f64 / GB;
            }
        }
        Cow::Owned(m)
    }

    /// Ask for the size of every listed URL not known yet. Returns how many were learned.
    pub async fn refresh(
        &self,
        urls: Vec<String>,
        hf_token: Option<String>,
        endpoint: &str,
    ) -> usize {
        let _one = self.busy.lock().await;
        let todo: Vec<String> = {
            let cache = self.cache.read();
            let mut tried = self.tried.write();
            urls.into_iter()
                .filter(|u| !cache.sizes.contains_key(u))
                .filter(|u| tried.insert(u.clone()))
                .collect()
        };
        if todo.is_empty() {
            return 0;
        }
        let client = match reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(std::time::Duration::from_secs(10))
            .timeout(std::time::Duration::from_secs(20))
            .user_agent(concat!("LocalGallery/", env!("CARGO_PKG_VERSION")))
            .build()
        {
            Ok(c) => c,
            Err(_) => return 0,
        };
        use futures::StreamExt;
        let found: Vec<(String, u64)> = futures::stream::iter(todo)
            .map(|url| {
                let client = client.clone();
                let token = hf_token.clone();
                let lookup = via(endpoint, &url);
                async move {
                    let size = head_size(&client, &lookup, token.as_deref()).await;
                    size.map(|s| (url, s))
                }
            })
            .buffer_unordered(8)
            .filter_map(|r| async move { r })
            .collect()
            .await;
        let n = found.len();
        if n > 0 {
            {
                let mut cache = self.cache.write();
                for (u, s) in found {
                    cache.sizes.insert(u, s);
                }
            }
            self.save();
        }
        n
    }
}

/// Size of the file behind `url`, following up to five redirects. The token goes only to
/// Hugging Face hosts.
async fn head_size(client: &reqwest::Client, url: &str, token: Option<&str>) -> Option<u64> {
    let mut url = reqwest::Url::parse(url).ok()?;
    for _ in 0..5 {
        let mut req = client.head(url.clone());
        if let Some(t) = token.filter(|_| lg_downloader::is_hf(url.as_str())) {
            req = req.bearer_auth(t);
        }
        let r = req.send().await.ok()?;
        let header = |name: &str| {
            r.headers()
                .get(name)
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.parse::<u64>().ok())
                .filter(|n| *n > 0)
        };
        if let Some(n) = header("x-linked-size") {
            return Some(n);
        }
        if r.status().is_redirection() {
            let next = r.headers().get("location")?.to_str().ok()?;
            url = url.join(next).ok()?;
            continue;
        }
        if r.status().is_success() {
            return header("content-length");
        }
        return None;
    }
    None
}

/// Every download URL the registry mentions whose size the manifest leaves unknown.
pub fn unknown_urls(manifests: impl Iterator<Item = Arc<Manifest>>) -> Vec<String> {
    let mut seen = HashSet::new();
    manifests
        .flat_map(|m| {
            m.files
                .iter()
                .flat_map(|f| f.variants.iter())
                .filter(|v| v.size_gb <= 0.0)
                .filter_map(|v| v.url.clone())
                .collect::<Vec<_>>()
        })
        .filter(|u| seen.insert(u.clone()))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::{HeaderMap, StatusCode, header};
    use axum::routing::any;

    async fn server() -> String {
        let app = axum::Router::new()
            // Hugging Face style: redirect that carries the size.
            .route(
                "/hf/{*rest}",
                any(|| async {
                    let mut h = HeaderMap::new();
                    h.insert(header::LOCATION, "/cdn/blob".parse().unwrap());
                    h.insert("x-linked-size", "7340032000".parse().unwrap());
                    (StatusCode::FOUND, h)
                }),
            )
            // Plain redirect to a file that reports its own length.
            .route(
                "/plain/{*rest}",
                any(|| async {
                    let mut h = HeaderMap::new();
                    h.insert(header::LOCATION, "/file".parse().unwrap());
                    (StatusCode::FOUND, h)
                }),
            )
            .route("/file", any(|| async { vec![0u8; 12345] }))
            .route("/missing", any(|| async { StatusCode::NOT_FOUND }));
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = l.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(l, app).await.unwrap() });
        format!("http://{addr}")
    }

    fn manifest(urls: &[&str]) -> Manifest {
        let variants: Vec<_> = urls
            .iter()
            .enumerate()
            .map(|(i, u)| serde_json::json!({"name": format!("f{i}.safetensors"), "precision": "bf16", "size_gb": 0, "url": u}))
            .collect();
        serde_json::from_value(serde_json::json!({
            "schema": 1, "id": "m", "display_name": "M", "family": "x", "media": "image",
            "files": [{"role": "diffusion", "folder": "diffusion_models", "variants": variants}],
            "ops": {}, "inputs": []
        }))
        .unwrap()
    }

    #[test]
    fn mirrors_stand_in_for_huggingface() {
        assert_eq!(
            via(HF, "https://huggingface.co/a/b"),
            "https://huggingface.co/a/b"
        );
        assert_eq!(
            via("http://127.0.0.1:9", "https://huggingface.co/a/b"),
            "http://127.0.0.1:9/a/b"
        );
        assert_eq!(
            via("http://127.0.0.1:9", "https://example.com/a"),
            "https://example.com/a"
        );
        assert_eq!(
            via("http://127.0.0.1:9", "https://huggingface.com.evil/a"),
            "https://huggingface.com.evil/a"
        );
    }

    #[tokio::test]
    async fn learns_sizes_and_fills_manifests() {
        let base = server().await;
        let dir = tempfile::tempdir().unwrap();
        let s = RemoteSizes::load(dir.path().join("cache/file-sizes.json"));
        let urls = [
            format!("{base}/hf/org/repo/resolve/main/a.safetensors"),
            format!("{base}/plain/b.safetensors"),
            format!("{base}/missing"),
        ];
        let m = Arc::new(manifest(
            &urls.iter().map(String::as_str).collect::<Vec<_>>(),
        ));
        assert_eq!(unknown_urls(std::iter::once(m.clone())).len(), 3);
        assert_eq!(s.refresh(urls.to_vec(), Some("secret".into()), HF).await, 2);
        // Tried once per session: no second round of requests.
        assert_eq!(s.refresh(urls.to_vec(), None, HF).await, 0);
        let filled = s.apply(&m);
        let v = &filled.files[0].variants;
        assert!((v[0].size_gb - 7340032000.0 / GB).abs() < 1e-9);
        assert!((v[1].size_gb - 12345.0 / GB).abs() < 1e-12);
        assert_eq!(v[2].size_gb, 0.0);
        // Persisted for the next launch.
        let again = RemoteSizes::load(dir.path().join("cache/file-sizes.json"));
        assert!(again.get_gb(&urls[0]).is_some());
    }
}
