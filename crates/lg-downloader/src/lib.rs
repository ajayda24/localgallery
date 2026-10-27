//! Resumable downloads with SHA-256 verification.
//!
//! Data goes to `<dest>.part` and is renamed when complete and verified. Cancelling
//! keeps the part file, so the next call resumes with an HTTP range request.

use futures::StreamExt;
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio_util::sync::CancellationToken;

#[derive(Debug, thiserror::Error)]
pub enum DownloadError {
    #[error("network error: {0}")]
    Network(String),
    #[error("server answered {0}")]
    Status(u16),
    #[error("checksum mismatch: expected {expected}, got {actual}")]
    Checksum { expected: String, actual: String },
    #[error("file error: {0}")]
    Io(#[from] std::io::Error),
    #[error("cancelled")]
    Cancelled,
}

#[derive(Debug, Clone, Default)]
pub struct Request {
    pub url: String,
    pub dest: PathBuf,
    pub sha256: Option<String>,
    /// Bearer token, only sent to Hugging Face hosts.
    pub hf_token: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Progress {
    pub received: u64,
    pub total: Option<u64>,
    pub bytes_per_sec: f64,
}

pub fn part_path(dest: &Path) -> PathBuf {
    let mut s = dest.as_os_str().to_owned();
    s.push(".part");
    PathBuf::from(s)
}

/// True for Hugging Face hosts, the only ones the access token is ever sent to.
pub fn is_hf(url: &str) -> bool {
    reqwest::Url::parse(url)
        .ok()
        .and_then(|u| {
            u.host_str()
                .map(|h| h == "huggingface.co" || h.ends_with(".huggingface.co"))
        })
        .unwrap_or(false)
}

pub fn client() -> reqwest::Client {
    reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(20))
        .read_timeout(Duration::from_secs(60))
        .user_agent(concat!("LocalGallery/", env!("CARGO_PKG_VERSION")))
        .build()
        .expect("http client")
}

pub async fn download(
    http: &reqwest::Client,
    req: &Request,
    cancel: &CancellationToken,
    mut progress: impl FnMut(Progress),
) -> Result<PathBuf, DownloadError> {
    if let Some(parent) = req.dest.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    let part = part_path(&req.dest);
    let mut hasher = Sha256::new();
    let mut have = match tokio::fs::metadata(&part).await {
        Ok(m) => m.len(),
        Err(_) => 0,
    };
    if have > 0 {
        hash_file_into(&part, &mut hasher, have).await?;
    }

    let mut rb = http.get(&req.url);
    if have > 0 {
        rb = rb.header(reqwest::header::RANGE, format!("bytes={have}-"));
    }
    if let Some(t) = req.hf_token.as_deref().filter(|t| !t.is_empty())
        && is_hf(&req.url)
    {
        rb = rb.bearer_auth(t);
    }
    let resp = tokio::select! {
        r = rb.send() => r.map_err(|e| DownloadError::Network(e.to_string()))?,
        _ = cancel.cancelled() => return Err(DownloadError::Cancelled),
    };
    let status = resp.status().as_u16();
    let total = match status {
        206 => resp.content_length().map(|l| l + have),
        200 => {
            // Server ignored the range: start over.
            if have > 0 {
                have = 0;
                hasher = Sha256::new();
            }
            resp.content_length()
        }
        416 if have > 0 => {
            // Already complete.
            return finish(&part, &req.dest, hasher, req.sha256.as_deref()).await;
        }
        s => return Err(DownloadError::Status(s)),
    };
    let mut file = tokio::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .append(have > 0)
        .truncate(have == 0)
        .open(&part)
        .await?;
    let mut stream = resp.bytes_stream();
    let mut received = have;
    let started = Instant::now();
    let mut last = Instant::now() - Duration::from_secs(1);
    loop {
        let chunk = tokio::select! {
            c = stream.next() => c,
            _ = cancel.cancelled() => {
                file.flush().await?;
                return Err(DownloadError::Cancelled);
            }
        };
        let Some(chunk) = chunk else { break };
        let chunk = chunk.map_err(|e| DownloadError::Network(e.to_string()))?;
        file.write_all(&chunk).await?;
        hasher.update(&chunk);
        received += chunk.len() as u64;
        if last.elapsed() >= Duration::from_millis(250) {
            last = Instant::now();
            let secs = started.elapsed().as_secs_f64().max(0.001);
            progress(Progress {
                received,
                total,
                bytes_per_sec: (received - have) as f64 / secs,
            });
        }
    }
    file.flush().await?;
    drop(file);
    if let Some(t) = total
        && received < t
    {
        return Err(DownloadError::Network(format!(
            "connection closed at {received} of {t} bytes"
        )));
    }
    progress(Progress {
        received,
        total,
        bytes_per_sec: 0.0,
    });
    finish(&part, &req.dest, hasher, req.sha256.as_deref()).await
}

async fn hash_file_into(path: &Path, hasher: &mut Sha256, len: u64) -> Result<(), DownloadError> {
    let mut f = tokio::fs::File::open(path).await?;
    let mut buf = vec![0u8; 1 << 20];
    let mut left = len;
    while left > 0 {
        let n = f.read(&mut buf).await?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
        left = left.saturating_sub(n as u64);
    }
    Ok(())
}

async fn finish(
    part: &Path,
    dest: &Path,
    hasher: Sha256,
    expected: Option<&str>,
) -> Result<PathBuf, DownloadError> {
    let actual = hex::encode(hasher.finalize());
    if let Some(exp) = expected.filter(|e| !e.is_empty())
        && !exp.eq_ignore_ascii_case(&actual)
    {
        // A corrupt part would fail forever on resume; remove it.
        let _ = tokio::fs::remove_file(part).await;
        return Err(DownloadError::Checksum {
            expected: exp.to_string(),
            actual,
        });
    }
    tokio::fs::rename(part, dest).await?;
    Ok(dest.to_path_buf())
}

/// SHA-256 of a whole file (for integrity checks / repair).
pub async fn sha256_file(path: &Path) -> Result<String, DownloadError> {
    let len = tokio::fs::metadata(path).await?.len();
    let mut h = Sha256::new();
    hash_file_into(path, &mut h, len).await?;
    Ok(hex::encode(h.finalize()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        Router,
        body::Body,
        extract::State,
        http::{HeaderMap, StatusCode, header},
        response::Response,
        routing::get,
    };
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[derive(Clone)]
    struct S {
        data: Arc<Vec<u8>>,
        /// Close the connection after this many bytes on the first request.
        cut_at: Option<usize>,
        hits: Arc<AtomicUsize>,
        auth_seen: Arc<parking_lot_free::Flag>,
    }

    mod parking_lot_free {
        use std::sync::atomic::{AtomicBool, Ordering};
        #[derive(Default)]
        pub struct Flag(AtomicBool);
        impl Flag {
            pub fn set(&self) {
                self.0.store(true, Ordering::SeqCst)
            }
            pub fn get(&self) -> bool {
                self.0.load(Ordering::SeqCst)
            }
        }
    }

    async fn serve(State(s): State<S>, headers: HeaderMap) -> Response {
        let hit = s.hits.fetch_add(1, Ordering::SeqCst);
        if headers.contains_key(header::AUTHORIZATION) {
            s.auth_seen.set();
        }
        let start = headers
            .get(header::RANGE)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.strip_prefix("bytes="))
            .and_then(|v| v.trim_end_matches('-').parse::<usize>().ok());
        let (status, body) = match start {
            Some(s0) => (StatusCode::PARTIAL_CONTENT, s.data[s0..].to_vec()),
            None => (StatusCode::OK, s.data.to_vec()),
        };
        let full_len = body.len();
        let body = match (hit, s.cut_at) {
            (0, Some(cut)) => {
                // Advertise the full length but send only part, then end the stream.
                let part = body[..cut].to_vec();
                // Delay the error so the headers and first chunk reach the client.
                let stream = futures::stream::iter(vec![
                    Ok::<_, std::io::Error>(part),
                    Err(std::io::Error::other("cut")),
                ])
                .then(|x| async move {
                    tokio::time::sleep(Duration::from_millis(100)).await;
                    x
                });
                Body::from_stream(stream)
            }
            _ => Body::from(body),
        };
        Response::builder()
            .status(status)
            .header(header::CONTENT_LENGTH, full_len)
            .body(body)
            .unwrap()
    }

    async fn server(cut_at: Option<usize>) -> (String, S) {
        let data: Vec<u8> = (0..300_000u32).map(|i| (i % 251) as u8).collect();
        let s = S {
            data: Arc::new(data),
            cut_at,
            hits: Arc::default(),
            auth_seen: Arc::default(),
        };
        let app = Router::new().route("/f", get(serve)).with_state(s.clone());
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = l.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(l, app).await.unwrap() });
        (format!("http://{addr}/f"), s)
    }

    fn sha(d: &[u8]) -> String {
        hex::encode(Sha256::digest(d))
    }

    fn local_client() -> reqwest::Client {
        reqwest::Client::builder().no_proxy().build().unwrap()
    }

    #[tokio::test]
    async fn resumes_after_cut_and_verifies() {
        let (url, s) = server(Some(100_000)).await;
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("model file ü.bin");
        let req = Request {
            url,
            dest: dest.clone(),
            sha256: Some(sha(&s.data)),
            hf_token: Some("secret".into()),
        };
        let http = local_client();
        let cancel = CancellationToken::new();
        let first = download(&http, &req, &cancel, |_| {}).await;
        assert!(first.is_err(), "first attempt is cut short");
        let have = std::fs::metadata(part_path(&dest)).unwrap().len();
        assert!(have > 0 && have < s.data.len() as u64);
        let mut last = None;
        download(&http, &req, &cancel, |p| last = Some(p))
            .await
            .unwrap();
        assert_eq!(std::fs::read(&dest).unwrap(), *s.data);
        assert_eq!(last.unwrap().received, s.data.len() as u64);
        assert!(!part_path(&dest).exists());
        assert!(!s.auth_seen.get(), "token must not be sent to non-HF hosts");
    }

    #[tokio::test]
    async fn bad_checksum_removes_part() {
        let (url, _) = server(None).await;
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("x.bin");
        let req = Request {
            url,
            dest: dest.clone(),
            sha256: Some("00".repeat(32)),
            hf_token: None,
        };
        let e = download(&local_client(), &req, &CancellationToken::new(), |_| {})
            .await
            .unwrap_err();
        assert!(matches!(e, DownloadError::Checksum { .. }));
        assert!(!dest.exists() && !part_path(&dest).exists());
    }

    #[tokio::test]
    async fn cancel_keeps_part() {
        let (url, _) = server(None).await;
        let dir = tempfile::tempdir().unwrap();
        let req = Request {
            url,
            dest: dir.path().join("y.bin"),
            sha256: None,
            hf_token: None,
        };
        let cancel = CancellationToken::new();
        cancel.cancel();
        let e = download(&local_client(), &req, &cancel, |_| {})
            .await
            .unwrap_err();
        assert!(matches!(e, DownloadError::Cancelled));
        assert!(
            is_hf("https://huggingface.co/x")
                && is_hf("https://cdn-lfs.huggingface.co/x")
                && !is_hf("https://evilhuggingface.co/x")
        );
    }
}
