//! Shared data types for LocalGallery.

pub mod event;
pub mod hw;
pub mod job;
pub mod manifest;

pub use event::*;
pub use hw::*;
pub use job::*;
pub use manifest::*;

/// Current time as unix milliseconds.
pub fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

/// `std::fs::canonicalize` without Windows' verbatim prefix: `\\?\D:\x` becomes `D:\x` and
/// `\\?\UNC\host\share` becomes `\\host\share`. Verbatim paths never match mount points, and
/// child processes such as Python get them in a form they do not always handle. Falls back to
/// the path as given when it does not exist.
pub fn canonical(path: &std::path::Path) -> std::path::PathBuf {
    let p = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    strip_verbatim(&p.to_string_lossy()).map_or(p, std::path::PathBuf::from)
}

fn strip_verbatim(s: &str) -> Option<String> {
    if let Some(rest) = s.strip_prefix(r"\\?\UNC\") {
        return Some(format!(r"\\{rest}"));
    }
    let rest = s.strip_prefix(r"\\?\")?;
    let b = rest.as_bytes();
    (b.len() >= 2 && b[0].is_ascii_alphabetic() && b[1] == b':').then(|| rest.to_string())
}

#[cfg(test)]
mod tests {
    use super::strip_verbatim;

    #[test]
    fn strips_windows_verbatim_prefixes() {
        assert_eq!(
            strip_verbatim(r"\\?\D:\a\Local Gallery").as_deref(),
            Some(r"D:\a\Local Gallery")
        );
        assert_eq!(
            strip_verbatim(r"\\?\UNC\nas\share\lg").as_deref(),
            Some(r"\\nas\share\lg")
        );
        assert_eq!(strip_verbatim(r"\\?\Volume{abc}\x"), None);
        assert_eq!(strip_verbatim("/home/user/lg"), None);
    }
}
