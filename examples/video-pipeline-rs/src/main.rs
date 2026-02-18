mod pipeline;
mod tasks;

use pipeline::{DEFAULT_VIDEO_URL, DownloadRequest, Verdict, work_dir};
use sayiir_runtime::prelude::*;
use std::time::Instant;
use tasks::*;
use tracing::info;

#[tokio::main]
async fn main() -> Result<(), BoxError> {
    tracing_subscriber::fmt()
        .with_target(false)
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();

    let workflow = workflow! {
        name: "video-pipeline",
        codec: JsonCodec,
        steps: [
            download_video,
            validate_upload,
            (transcode_720p || transcode_1080p || transcode_4k || generate_thumbnails || moderate_content), // parallel tasks
            merge_results,
            route check_moderation -> Verdict {
                Approved => [upload_to_cdn, update_database, notify_user],
                Rejected => [cleanup_artifacts, notify_rejection],
            }
        ]
    }
    .expect("Failed to build workflow");

    let source_url = std::env::var("VIDEO_URL").unwrap_or_else(|_| DEFAULT_VIDEO_URL.into());

    info!(source_url = %source_url, "🎬 Starting video processing pipeline");

    let input = DownloadRequest {
        upload_id: "vid_a1b2c3".into(),
        user_id: "user_42".into(),
        source_url,
    };

    let output_dir = work_dir(&input.upload_id);

    let start = Instant::now();
    let status = workflow.run_once(input).await?;
    let elapsed = start.elapsed();
    info!(
        elapsed = format!("{:.1}s", elapsed.as_secs_f64()),
        output = %output_dir.display(),
        "🏁 Pipeline finished: {status:?}"
    );

    Ok(())
}
