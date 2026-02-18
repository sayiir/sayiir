use bytes::Bytes;
use rand::Rng;
use sayiir_runtime::prelude::*;
use std::path::Path;
use std::time::Instant;
use tokio::process::Command;
use tracing::{info, warn};

use crate::pipeline::*;

// ─── Fork result decoding ───────────────────────────────────────────────────
//
// Uses macro-generated `XxxTask::task_id()` constants so branch keys stay in
// sync with the `#[task(id = "...")]` attributes — a renamed task ID will
// produce a compile error here rather than a silent runtime mismatch.

impl TryFrom<NamedBranchResults> for ForkResults {
    type Error = BoxError;

    fn try_from(results: NamedBranchResults) -> Result<Self, Self::Error> {
        let map = results.into_map();

        fn decode<T: serde::de::DeserializeOwned>(
            map: &std::collections::HashMap<String, Bytes>,
            key: &str,
        ) -> Result<T, BoxError> {
            let bytes = map.get(key).ok_or(format!("branch '{key}' missing"))?;
            let inner: Bytes = JsonCodec.decode(bytes.clone())?;
            Ok(serde_json::from_slice(&inner)?)
        }

        Ok(Self {
            transcode_720p: decode(&map, Transcode720pTask::task_id())?,
            transcode_1080p: decode(&map, Transcode1080pTask::task_id())?,
            transcode_4k: decode(&map, Transcode4kTask::task_id())?,
            thumbnails: decode(&map, GenerateThumbnailsTask::task_id())?,
            moderation: decode(&map, ModerateContentTask::task_id())?,
        })
    }
}

// ─── Helpers ─────────────────────────────────────────────────────────────────

/// Run an ffmpeg/ffprobe command and return stdout. Logs stderr on failure.
async fn run_ffmpeg(program: &str, args: &[&str]) -> Result<String, BoxError> {
    let output = Command::new(program)
        .args(args)
        .output()
        .await
        .map_err(|e| format!("{program} not found — install ffmpeg: {e}"))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("{program} failed: {stderr}").into());
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

/// Query a single ffprobe value from a file.
async fn ffprobe_field(path: &str, entry: &str) -> Result<String, BoxError> {
    run_ffmpeg(
        "ffprobe",
        &[
            "-v",
            "error",
            "-select_streams",
            "v:0",
            "-show_entries",
            entry,
            "-of",
            "default=noprint_wrappers=1:nokey=1",
            path,
        ],
    )
    .await
}

/// Transcode the source video to a given resolution using ffmpeg.
async fn transcode(
    video: &VideoFile,
    height: u32,
    label: &str,
) -> Result<TranscodeResult, BoxError> {
    let dir = work_dir(&video.upload_id);
    let output = dir.join(format!("{label}.mp4"));

    let scale = format!("scale=-2:{height}");
    let start = Instant::now();

    info!(resolution = label, "⚙ transcoding with ffmpeg");
    let status = Command::new("ffmpeg")
        .args([
            "-y",
            "-i",
            &video.local_path,
            "-vf",
            &scale,
            "-c:v",
            "libx264",
            "-preset",
            "fast",
            "-crf",
            "23",
            "-c:a",
            "aac",
            "-b:a",
            "128k",
            "-movflags",
            "+faststart",
            "-loglevel",
            "warning",
            output.to_str().unwrap(),
        ])
        .status()
        .await
        .map_err(|e| format!("ffmpeg not found: {e}"))?;

    if !status.success() {
        return Err(format!("ffmpeg transcode to {label} failed").into());
    }

    let elapsed = start.elapsed().as_secs_f64();
    let file_size = tokio::fs::metadata(&output).await?.len();

    Ok(TranscodeResult {
        resolution: label.into(),
        path: output.to_string_lossy().into(),
        duration_secs: elapsed,
        file_size,
    })
}

// ─── Step 1: Download video ─────────────────────────────────────────────────

#[task(id = "download_video", retries = 2, timeout = "5m", backoff = "3s")]
pub async fn download_video(req: DownloadRequest) -> Result<VideoFile, BoxError> {
    use futures_util::StreamExt;
    use tokio::io::AsyncWriteExt;

    let dir = work_dir(&req.upload_id);
    tokio::fs::create_dir_all(&dir).await?;

    let dest = dir.join("source.mp4");

    info!(url = %req.source_url, "⬇ downloading video");
    let response = reqwest::get(&req.source_url)
        .await
        .map_err(|e| format!("download failed: {e}"))?
        .error_for_status()
        .map_err(|e| {
            format!(
                "download failed (HTTP {}): {e}",
                e.status().unwrap_or_default()
            )
        })?;

    let total = response.content_length().unwrap_or(0);
    let mut stream = response.bytes_stream();
    let mut file = tokio::fs::File::create(&dest).await?;
    let mut downloaded: u64 = 0;

    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|e| format!("download stream error: {e}"))?;
        file.write_all(&chunk).await?;
        downloaded += chunk.len() as u64;

        if total > 0 {
            let pct = (downloaded as f64 / total as f64) * 100.0;
            if downloaded == total || (pct as u64) % 25 == 0 {
                info!(progress = format!("{pct:.0}%"), "downloading");
            }
        }
    }
    file.flush().await?;

    let metadata = tokio::fs::metadata(&dest).await?;
    info!(
        size = format!("{:.1} MB", metadata.len() as f64 / 1_048_576.0),
        "✓ download complete"
    );

    let width: u32 = ffprobe_field(dest.to_str().unwrap(), "stream=width")
        .await?
        .parse()?;
    let height: u32 = ffprobe_field(dest.to_str().unwrap(), "stream=height")
        .await?
        .parse()?;
    let duration: f64 = ffprobe_field(dest.to_str().unwrap(), "format=duration")
        .await?
        .parse()?;
    let format = ffprobe_field(dest.to_str().unwrap(), "stream=codec_name").await?;

    Ok(VideoFile {
        upload_id: req.upload_id,
        user_id: req.user_id,
        local_path: dest.to_string_lossy().into(),
        size_bytes: metadata.len(),
        format,
        width,
        height,
        duration_secs: duration,
    })
}

// ─── Step 2: Validate upload ────────────────────────────────────────────────

#[task(id = "validate_upload", timeout = "10s")]
pub async fn validate_upload(video: VideoFile) -> Result<VideoFile, BoxError> {
    let max_size: u64 = 10 * 1024 * 1024 * 1024;

    if !Path::new(&video.local_path).exists() {
        return Err(format!("file not found: {}", video.local_path).into());
    }
    if video.size_bytes > max_size {
        return Err(format!(
            "file too large: {:.1} GB (max 10 GB)",
            video.size_bytes as f64 / 1_073_741_824.0
        )
        .into());
    }
    if video.width == 0 || video.height == 0 {
        return Err("no video stream detected".into());
    }

    info!(
        upload_id = %video.upload_id,
        format = %video.format,
        resolution = format!("{}x{}", video.width, video.height),
        duration = format!("{:.1}s", video.duration_secs),
        size = format!("{:.1} MB", video.size_bytes as f64 / 1_048_576.0),
        "✓ validated"
    );
    Ok(video)
}

// ─── Step 3: Parallel branches (transcode + thumbnails + moderation) ────────

#[task(id = "transcode_720p", retries = 2, timeout = "5m", backoff = "1s")]
pub async fn transcode_720p(video: VideoFile) -> Result<Bytes, BoxError> {
    let result = transcode(&video, 720, "720p").await.map_err(|e| {
        warn!(resolution = "720p", error = %e, "✗ transcode failed");
        e
    })?;
    info!(
        resolution = "720p",
        duration = format!("{:.1}s", result.duration_secs),
        size = format!("{:.1} MB", result.file_size as f64 / 1_048_576.0),
        "✓ transcode completed"
    );
    Ok(Bytes::from(serde_json::to_vec(&result)?))
}

#[task(id = "transcode_1080p", retries = 2, timeout = "15m", backoff = "2s")]
pub async fn transcode_1080p(video: VideoFile) -> Result<Bytes, BoxError> {
    let result = transcode(&video, 1080, "1080p").await.map_err(|e| {
        warn!(resolution = "1080p", error = %e, "✗ transcode failed");
        e
    })?;
    info!(
        resolution = "1080p",
        duration = format!("{:.1}s", result.duration_secs),
        size = format!("{:.1} MB", result.file_size as f64 / 1_048_576.0),
        "✓ transcode completed"
    );
    Ok(Bytes::from(serde_json::to_vec(&result)?))
}

#[task(
    id = "transcode_4k",
    retries = 3,
    timeout = "30m",
    backoff = "10s",
    backoff_multiplier = 2.0
)]
pub async fn transcode_4k(video: VideoFile) -> Result<Bytes, BoxError> {
    if video.height < 2160 && video.width < 3840 {
        warn!(
            source = format!("{}x{}", video.width, video.height),
            "4K transcode skipped — source resolution too low"
        );
        let result: Option<TranscodeResult> = None;
        return Ok(Bytes::from(serde_json::to_vec(&result)?));
    }

    match transcode(&video, 2160, "4k").await {
        Ok(result) => {
            info!(
                resolution = "4k",
                duration = format!("{:.1}s", result.duration_secs),
                size = format!("{:.1} MB", result.file_size as f64 / 1_048_576.0),
                "✓ transcode completed"
            );
            let wrapped: Option<TranscodeResult> = Some(result);
            Ok(Bytes::from(serde_json::to_vec(&wrapped)?))
        }
        Err(e) => {
            warn!(resolution = "4k", error = %e, "✗ transcode failed (best-effort, continuing)");
            let result: Option<TranscodeResult> = None;
            Ok(Bytes::from(serde_json::to_vec(&result)?))
        }
    }
}

#[task(
    id = "generate_thumbnails",
    retries = 2,
    timeout = "60s",
    backoff = "1s"
)]
pub async fn generate_thumbnails(video: VideoFile) -> Result<Bytes, BoxError> {
    let dir = work_dir(&video.upload_id);
    let pattern = dir.join("thumb_%02d.jpg");

    info!("⚙ extracting thumbnails with ffmpeg");

    let interval = video.duration_secs / 4.0;
    let fps_filter = format!("fps=1/{interval:.2},scale=320:-2");

    let status = Command::new("ffmpeg")
        .args([
            "-y",
            "-i",
            &video.local_path,
            "-vf",
            &fps_filter,
            "-frames:v",
            "3",
            "-q:v",
            "2",
            "-loglevel",
            "warning",
            pattern.to_str().unwrap(),
        ])
        .status()
        .await
        .map_err(|e| format!("ffmpeg not found: {e}"))?;

    if !status.success() {
        return Err("ffmpeg thumbnail extraction failed".into());
    }

    let mut paths = Vec::new();
    for i in 1..=3 {
        let thumb = dir.join(format!("thumb_{i:02}.jpg"));
        if thumb.exists() {
            paths.push(thumb.to_string_lossy().into());
        }
    }

    if paths.is_empty() {
        return Err("no thumbnails generated".into());
    }

    info!(count = paths.len(), "✓ thumbnails generated");
    let result = ThumbnailResult { paths };
    Ok(Bytes::from(serde_json::to_vec(&result)?))
}

#[task(id = "moderate_content", retries = 3, timeout = "30s", backoff = "2s")]
pub async fn moderate_content(_video: VideoFile) -> Result<Bytes, BoxError> {
    let (is_approved, confidence, delay_ms) = {
        let mut rng = rand::thread_rng();
        (
            rng.gen_range(0..100) < 90,
            rng.gen_range(0.85..0.99),
            rng.gen_range(200..800),
        )
    };

    tokio::time::sleep(tokio::time::Duration::from_millis(delay_ms)).await;

    let should_fail = { rand::thread_rng().gen_range(0..100) < 20 };
    if should_fail {
        warn!("✗ moderation API returned error");
        return Err("moderation API: simulated 503".into());
    }

    let verdict = if is_approved {
        Verdict::Approved
    } else {
        Verdict::Rejected
    };
    let result = ModerationResult {
        verdict: verdict.as_key().into(),
        confidence,
        flags: match verdict {
            Verdict::Rejected => vec!["policy_violation".into()],
            Verdict::Approved => vec![],
        },
    };
    info!(
        verdict = %result.verdict,
        confidence = format!("{:.2}", result.confidence),
        "✓ moderation completed"
    );
    Ok(Bytes::from(serde_json::to_vec(&result)?))
}

// ─── Step 4: Merge results (smart fan-in gate) ──────────────────────────────

#[task(id = "merge_results")]
pub async fn merge_results(results: NamedBranchResults) -> Result<String, BoxError> {
    let fork: ForkResults = results.try_into()?;

    if fork.transcode_4k.is_none() {
        warn!("4K transcode unavailable — continuing without it");
    }

    let mut transcodes = vec![fork.transcode_720p, fork.transcode_1080p];
    if let Some(t) = fork.transcode_4k {
        transcodes.push(t);
    }

    let upload_id = transcodes
        .first()
        .map(|t| {
            Path::new(&t.path)
                .parent()
                .and_then(|p| p.file_name())
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default()
        })
        .unwrap_or_default();

    let manifest = PipelineManifest {
        upload_id,
        transcodes,
        thumbnails: fork.thumbnails,
        moderation: fork.moderation,
    };

    info!(
        resolutions = manifest.transcodes.len(),
        thumbnails = manifest.thumbnails.paths.len(),
        moderation = %manifest.moderation.verdict,
        "✓ merge complete"
    );

    Ok(serde_json::to_string(&manifest)?)
}

// ─── Step 5: Route classifier ───────────────────────────────────────────────

#[task(id = "check_moderation")]
pub async fn check_moderation(manifest_json: String) -> Result<String, BoxError> {
    let manifest: PipelineManifest = serde_json::from_str(&manifest_json)?;
    let verdict = manifest.moderation.verdict.clone();
    info!(verdict = %verdict, "→ routing to '{verdict}' branch");
    Ok(verdict)
}

// ─── Step 6a: Approved branch ───────────────────────────────────────────────

#[task(id = "upload_to_cdn", retries = 2, timeout = "2m", backoff = "2s")]
pub async fn upload_to_cdn(manifest_json: String) -> Result<CdnManifest, BoxError> {
    let manifest: PipelineManifest = serde_json::from_str(&manifest_json)?;

    let mut video_urls = Vec::new();
    let mut thumbnail_urls = Vec::new();

    for tc in &manifest.transcodes {
        if !Path::new(&tc.path).exists() {
            return Err(format!("transcode output missing: {}", tc.path).into());
        }
        let url = format!(
            "https://cdn.example.com/{}/{}.mp4",
            manifest.upload_id, tc.resolution
        );
        info!(
            resolution = %tc.resolution,
            file_size = format!("{:.1} MB", tc.file_size as f64 / 1_048_576.0),
            "✓ uploaded to CDN"
        );
        video_urls.push(url);
    }

    for (i, path) in manifest.thumbnails.paths.iter().enumerate() {
        if !Path::new(path).exists() {
            warn!(thumbnail = i, "thumbnail file missing, skipping");
            continue;
        }
        let url = format!(
            "https://cdn.example.com/{}/thumb_{i}.jpg",
            manifest.upload_id
        );
        info!(thumbnail = i, "✓ thumbnail uploaded to CDN");
        thumbnail_urls.push(url);
    }

    let cdn_manifest = CdnManifest {
        upload_id: manifest.upload_id,
        video_urls,
        thumbnail_urls,
    };

    info!(
        videos = cdn_manifest.video_urls.len(),
        thumbnails = cdn_manifest.thumbnail_urls.len(),
        "✓ all assets uploaded to CDN"
    );
    Ok(cdn_manifest)
}

#[task(id = "update_database", retries = 2, timeout = "10s")]
pub async fn update_database(cdn_manifest: CdnManifest) -> Result<CdnManifest, BoxError> {
    tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;
    info!(
        upload_id = %cdn_manifest.upload_id,
        status = "published",
        "✓ video record updated"
    );
    Ok(cdn_manifest)
}

#[task(id = "notify_user", retries = 2, timeout = "10s")]
pub async fn notify_user(cdn_manifest: CdnManifest) -> Result<String, BoxError> {
    tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;
    info!(
        upload_id = %cdn_manifest.upload_id,
        urls = cdn_manifest.video_urls.len(),
        "✓ 'video ready' notification sent"
    );
    Ok(format!("Pipeline complete for {}", cdn_manifest.upload_id))
}

// ─── Step 6b: Rejected branch ───────────────────────────────────────────────

#[task(id = "cleanup_artifacts", retries = 2, timeout = "30s")]
pub async fn cleanup_artifacts(manifest_json: String) -> Result<String, BoxError> {
    let manifest: PipelineManifest = serde_json::from_str(&manifest_json)?;

    let mut deleted = 0u32;
    for tc in &manifest.transcodes {
        if tokio::fs::remove_file(&tc.path).await.is_ok() {
            deleted += 1;
        }
    }
    for path in &manifest.thumbnails.paths {
        if tokio::fs::remove_file(path).await.is_ok() {
            deleted += 1;
        }
    }

    info!(
        upload_id = %manifest.upload_id,
        artifacts_deleted = deleted,
        "✓ artifacts cleaned up"
    );
    Ok(manifest.upload_id)
}

#[task(id = "notify_rejection", retries = 2, timeout = "10s")]
pub async fn notify_rejection(upload_id: String) -> Result<String, BoxError> {
    tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;
    info!(upload_id = %upload_id, "✓ rejection notification sent");
    Ok(format!("Video {upload_id} rejected and cleaned up"))
}
