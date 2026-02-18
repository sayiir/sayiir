# Video Processing Pipeline (Rust)

Fault-tolerant video processing pipeline that downloads a video, transcodes it to
multiple resolutions with **real ffmpeg**, extracts thumbnails, runs content moderation,
and conditionally routes to CDN upload or artifact cleanup.

Matches the [Video Pipeline tutorial](https://docs.sayiir.dev/tutorials/background-jobs-rust/).

## Sayiir features demonstrated

| Feature | How it's used |
|---|---|
| **Fan-out parallelism** | 5-way fork: 3 transcodes + thumbnails + moderation run concurrently |
| **Fan-in with validation** | `merge_results` validates required transcodes, 4K is optional |
| **Step-level retries** | Only the failed branch retries — completed transcodes are preserved |
| **Per-task backoff** | Different retry policies per failure profile (4K: 10s exponential, moderation: 2s) |
| **Conditional routing** | `Approved` branch uploads to CDN; `Rejected` branch cleans up artifacts |
| **Best-effort tasks** | 4K transcode failure doesn't block the pipeline — returns `None`, merge handles it |
| **Typed routing keys** | `#[derive(BranchKey)]` enum ensures exhaustive route matching at compile time |
| **Typed fan-in** | `ForkResults` with `TryFrom<NamedBranchResults>` using macro-generated `task_id()` constants |

## Pipeline

```
download_video             [retries=2, timeout=5m]
validate_upload
    ├── transcode_720p     [retries=2, timeout=5m]   ← ffmpeg
    ├── transcode_1080p    [retries=2, timeout=15m]  ← ffmpeg
    ├── transcode_4k       [retries=3, timeout=30m, best-effort]
    ├── generate_thumbnails [retries=2, timeout=60s] ← ffmpeg
    └── moderate_content   [retries=3, timeout=30s]  ← simulated flaky API
merge_results  ← validates required transcodes, 4K optional
    route check_moderation -> Verdict {
        Approved => [upload_to_cdn, update_database, notify_user],
        Rejected => [cleanup_artifacts, notify_rejection],
    }
```

**14 tasks** composed using a single `workflow!` macro call:

```rust
workflow! {
    name: "video-pipeline",
    codec: JsonCodec,
    steps: [
        download_video,
        validate_upload,
        (transcode_720p || transcode_1080p || transcode_4k || generate_thumbnails || moderate_content),
        merge_results,
        route check_moderation -> Verdict {
            Approved => [upload_to_cdn, update_database, notify_user],
            Rejected => [cleanup_artifacts, notify_rejection],
        }
    ]
}
```

## Prerequisites

- Rust (1.83+)
- `ffmpeg` and `ffprobe` installed and on `PATH`

## Run

```bash
cargo run
```

By default, downloads a 30 MB [Big Buck Bunny trailer](https://download.blender.org/peach/trailer/trailer_1080p.mov) from the Blender Foundation. Provide your own video:

```bash
VIDEO_URL=https://example.com/my-video.mp4 cargo run
```

## Sample output

```
INFO  🎬 Starting video processing pipeline
INFO  ⬇ downloading video url=https://download.blender.org/peach/trailer/trailer_1080p.mov
INFO  ✓ download complete size="29.4 MB"
INFO  ✓ validated resolution=1920x1080 duration=33.0s size="29.4 MB"
INFO  ⚙ transcoding with ffmpeg resolution=720p
INFO  ⚙ transcoding with ffmpeg resolution=1080p
INFO  ⚙ extracting thumbnails with ffmpeg
WARN  4K transcode skipped — source resolution too low
WARN  ✗ moderation API returned error
INFO  Retrying task task_id=moderate_content delay_ms=2000
INFO  ✓ thumbnails generated count=3
INFO  ✓ transcode completed resolution=720p duration=4.2s size="4.7 MB"
INFO  ✓ moderation completed verdict=approved confidence=0.95
INFO  ✓ transcode completed resolution=1080p duration=6.3s size="9.4 MB"
INFO  ✓ merge complete resolutions=2 thumbnails=3 moderation=approved
INFO  → routing to 'approved' branch verdict=approved
INFO  ✓ uploaded to CDN resolution=720p
INFO  ✓ uploaded to CDN resolution=1080p
INFO  ✓ all assets uploaded to CDN videos=2 thumbnails=3
INFO  ✓ video record updated status="published"
INFO  ✓ 'video ready' notification sent urls=2
INFO  🏁 Pipeline finished: Completed elapsed="7.1s" output=data/vid_a1b2c3
```

## Output files

All artifacts are written to `data/` (gitignored):

```
data/vid_a1b2c3/
├── source.mp4        ← downloaded original (30 MB)
├── 720p.mp4          ← transcoded by ffmpeg (4.7 MB)
├── 1080p.mp4         ← transcoded by ffmpeg (9.4 MB)
├── thumb_01.jpg      ← extracted thumbnail
├── thumb_02.jpg
└── thumb_03.jpg
```

## Project structure

```
src/
├── main.rs       ← workflow composition + entry point
├── pipeline.rs   ← data types, Verdict enum, ForkResults
└── tasks.rs      ← all 14 task implementations + ffmpeg helpers
```

## How it handles failures

| Scenario | What happens |
|---|---|
| 4K transcode fails | Returns `Option::None` — merge continues with 720p + 1080p |
| Source resolution < 4K | 4K task skips gracefully instead of upscaling |
| Moderation API 503 | Retries 3 times with 2s exponential backoff |
| CDN upload fails | Retries 2 times with 2s backoff |
| Moderation rejects video | Routes to `Rejected` branch — deletes transcoded files and thumbnails |
| Deploy mid-pipeline | Durable execution resumes from the last completed step |
