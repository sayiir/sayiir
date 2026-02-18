use sayiir_runtime::prelude::*;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Public domain 1080p video from the Blender Foundation (Big Buck Bunny trailer, ~30 MB).
/// Replace with any direct-download MP4/MOV URL to process your own video.
pub const DEFAULT_VIDEO_URL: &str = "https://download.blender.org/peach/trailer/trailer_1080p.mov";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DownloadRequest {
    pub upload_id: String,
    pub user_id: String,
    pub source_url: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VideoFile {
    pub upload_id: String,
    pub user_id: String,
    pub local_path: String,
    pub size_bytes: u64,
    pub format: String,
    pub width: u32,
    pub height: u32,
    pub duration_secs: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TranscodeResult {
    pub resolution: String,
    pub path: String,
    pub duration_secs: f64,
    pub file_size: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ThumbnailResult {
    pub paths: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModerationResult {
    pub verdict: String,
    pub confidence: f64,
    pub flags: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PipelineManifest {
    pub upload_id: String,
    pub transcodes: Vec<TranscodeResult>,
    pub thumbnails: ThumbnailResult,
    pub moderation: ModerationResult,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CdnManifest {
    pub upload_id: String,
    pub video_urls: Vec<String>,
    pub thumbnail_urls: Vec<String>,
}

/// Typed routing key for the moderation verdict. Derives `BranchKey` so the
/// `workflow!` macro checks exhaustiveness at build time — adding a new variant
/// without a matching route arm is a compile error.
#[derive(BranchKey)]
pub enum Verdict {
    Approved,
    Rejected,
}

// ─── Fork results (typed fan-in) ─────────────────────────────────────────────

/// Typed view of the parallel branch outputs. Isolates the stringly-typed
/// `NamedBranchResults` decoding so `merge_results` stays clean.
///
/// The `TryFrom<NamedBranchResults>` impl lives in `tasks.rs` where the
/// macro-generated task structs (and their `task_id()` constants) are available.
pub struct ForkResults {
    pub transcode_720p: TranscodeResult,
    pub transcode_1080p: TranscodeResult,
    pub transcode_4k: Option<TranscodeResult>,
    pub thumbnails: ThumbnailResult,
    pub moderation: ModerationResult,
}

/// Build the working directory for a given upload.
/// Outputs go to `data/<upload_id>/` next to `src/` for easy inspection.
pub fn work_dir(upload_id: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("data")
        .join(upload_id)
}
