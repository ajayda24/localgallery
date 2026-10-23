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
