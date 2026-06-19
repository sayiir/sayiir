//! Sayiir performance benchmark driver.
//!
//! See `docs/plans/2026-05-17-performance-benchmark-design.md` for the
//! full design. This binary is the public face of the bench suite: a
//! third party can `git clone && docker compose up && cargo run`
//! against it without reading any Rust to reproduce our numbers.
//!
//! Scenarios:
//!
//! * `linear`         — universal N-step throughput sweep.
//! * `fanout`         — 1 parent → K parallel children → join.
//! * `signal-driven`  — park-on-signal, signal arrives, resume.
//! * `sleeping-giants` — long-timer durable parking + wake storm.
//!
//! Meta-commands:
//!
//! * `sweep`   — invoke a scenario N times varying one axis, write a
//!   Markdown summary table next to the per-point JSONs.
//! * `compare` — diff a fresh run against a committed baseline; exit
//!   non-zero on regression beyond a configurable threshold.

#![deny(clippy::pedantic)]
#![allow(
    clippy::module_name_repetitions,
    clippy::missing_errors_doc,
    clippy::missing_panics_doc,
    clippy::doc_markdown,
    // Throughput/latency math is bench math: nanosecond → f64 → ms
    // chains are expected, and `usize → u32` for connection pool
    // sizing is hand-validated. Reverting to checked-conversion noise
    // would obscure the actual measurement logic; bench code is the
    // wrong place to chase mantissa precision.
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_sign_loss,
    clippy::cast_lossless,
    clippy::similar_names,
    clippy::too_many_lines,
    clippy::struct_excessive_bools,
    clippy::format_push_string,
    clippy::map_unwrap_or,
    clippy::items_after_statements,
    clippy::needless_pass_by_value,
    clippy::case_sensitive_file_extension_comparisons,
    clippy::clone_on_copy,
)]

mod compare;
mod driver;
mod metrics;
mod report;
mod sweep;
mod workloads;

use std::time::Duration;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};

const DEFAULT_POSTGRES_URL: &str = "postgresql://postgres:postgres@127.0.0.1:5432/sayiir_bench";
const DEFAULT_PROMETHEUS_URL: &str = "http://127.0.0.1:9090";

#[derive(Parser, Debug)]
#[command(name = "sayiir-bench", version, about)]
struct Cli {
    /// Postgres connection URL.
    #[arg(long, env = "SAYIIR_BENCH_POSTGRES_URL", default_value = DEFAULT_POSTGRES_URL)]
    postgres_url: String,

    /// Prometheus base URL used for the end-of-run snapshot.
    #[arg(long, env = "SAYIIR_BENCH_PROMETHEUS_URL", default_value = DEFAULT_PROMETHEUS_URL)]
    prometheus_url: String,

    /// Bind address for the driver's Prometheus metrics scrape endpoint.
    #[arg(
        long,
        env = "SAYIIR_BENCH_METRICS_ADDR",
        default_value = "0.0.0.0:9464"
    )]
    metrics_addr: String,

    /// Directory where JSON result files are written.
    #[arg(long, default_value = "results")]
    results_dir: String,

    /// Skip truncating Sayiir tables before the run. Default: truncate.
    #[arg(long)]
    no_reset_db: bool,

    #[command(subcommand)]
    scenario: Scenario,
}

#[derive(Subcommand, Debug)]
enum Scenario {
    /// Linear N-step throughput burst — the universal benchmark.
    ///
    /// `--steps N` defaults to 4 so this is a drop-in replacement for
    /// the prior `throughput` scenario; the clap `alias` keeps the old
    /// invocation working too.
    #[command(alias = "throughput")]
    Linear(LinearArgs),
    /// Fan-out / scatter-gather: 1 parent → K parallel branches → join.
    Fanout(FanoutArgs),
    /// Signal-driven park-and-resume.
    #[command(name = "signal-driven", alias = "signal")]
    SignalDriven(SignalDrivenArgs),
    /// Long-timer durable parking + wake storm.
    #[command(name = "sleeping-giants", alias = "sleeping")]
    SleepingGiants(SleepingGiantsArgs),

    /// Run a scenario N times sweeping one parameter.
    Sweep(SweepArgs),
    /// Compare a fresh run against a committed baseline.
    Compare(CompareArgs),
}

#[derive(Parser, Debug)]
pub struct LinearArgs {
    /// Total number of workflows to submit in the measurement phase.
    #[arg(long, default_value_t = 100_000)]
    pub workflows: usize,

    /// Workflows to submit and drain before the measurement phase
    /// starts. Set to 0 to disable warmup entirely (small smoke runs).
    /// Default scales with the measured load: at least 200, capped at
    /// 5% of `workflows`.
    #[arg(long, default_value_t = 200)]
    pub warmup_workflows: usize,

    /// Number of tasks per workflow. Linear shape: 1 pickup + (steps-2)
    /// middle + 1 final_emit. `--steps 1` collapses everything into a
    /// single task. Restate uses {1,3,9}; DBOS uses 4.
    #[arg(long, default_value_t = 4)]
    pub steps: usize,

    /// Number of concurrent submission tasks.
    #[arg(long, default_value_t = 256)]
    pub concurrency: usize,

    /// Optional submission rate cap (workflows/sec). Default: unbounded.
    #[arg(long)]
    pub target_rate: Option<u64>,

    /// Number of in-process Sayiir worker actors.
    #[arg(long, default_value_t = 8)]
    pub workers: usize,

    /// Worker poll interval (milliseconds).
    #[arg(long, default_value_t = 5)]
    pub poll_ms: u64,

    /// Tasks fetched per worker poll (batch size).
    #[arg(long, default_value_t = 64)]
    pub batch_size: usize,
}

#[derive(Parser, Debug)]
pub struct FanoutArgs {
    #[arg(long, default_value_t = 10_000)]
    pub workflows: usize,
    #[arg(long, default_value_t = 100)]
    pub warmup_workflows: usize,
    /// Number of parallel children spawned per workflow. Standard
    /// sweep is {10, 100, 1000}.
    #[arg(long, default_value_t = 10)]
    pub children: usize,
    #[arg(long, default_value_t = 128)]
    pub concurrency: usize,
    #[arg(long, default_value_t = 8)]
    pub workers: usize,
    #[arg(long, default_value_t = 5)]
    pub poll_ms: u64,
    #[arg(long, default_value_t = 128)]
    pub batch_size: usize,
}

#[derive(Parser, Debug)]
pub struct SignalDrivenArgs {
    #[arg(long, default_value_t = 10_000)]
    pub workflows: usize,
    #[arg(long, default_value_t = 128)]
    pub concurrency: usize,
    #[arg(long, default_value_t = 8)]
    pub workers: usize,
    #[arg(long, default_value_t = 5)]
    pub poll_ms: u64,
    #[arg(long, default_value_t = 64)]
    pub batch_size: usize,
    /// Timeout (seconds) applied to the `wait_for_signal` node. The
    /// driver sends the signal almost immediately, so this is just a
    /// safety net; set well above the run length.
    #[arg(long, default_value_t = 600)]
    pub signal_timeout_secs: u64,
}

#[derive(Parser, Debug)]
pub struct SleepingGiantsArgs {
    #[arg(long, default_value_t = 500_000)]
    pub workflows: usize,
    #[arg(long, default_value_t = 512)]
    pub concurrency: usize,
    #[arg(long, default_value_t = 60)]
    pub sleep_secs: u64,
    #[arg(long, default_value_t = 16)]
    pub workers: usize,
    #[arg(long, default_value_t = 50)]
    pub poll_ms: u64,
    #[arg(long, default_value_t = 128)]
    pub batch_size: usize,
    #[arg(long)]
    pub demo_restart: bool,
}

#[derive(Parser, Debug)]
pub struct SweepArgs {
    /// Scenario subcommand name to run per point (e.g. `linear`).
    #[arg(long)]
    pub scenario: String,
    /// Axis to vary. Supported: `workers`, `concurrency`, `batch_size`,
    /// `poll_ms`, `steps`, `children`.
    #[arg(long, default_value = "workers")]
    pub axis: String,
    /// Comma-separated values of the axis (e.g. `1,2,4,8,16,32`).
    #[arg(long, default_value = "1,2,4,8,16,32")]
    pub values: String,
    /// Extra arguments passed through to each scenario invocation
    /// verbatim (use `--` separator on the command line). Useful for
    /// pinning workflow count or warmup across all sweep points.
    #[arg(last = true)]
    pub extra_args: Vec<String>,
}

#[derive(Parser, Debug)]
pub struct CompareArgs {
    /// Scenario name. Used to locate baseline + filter candidate.
    #[arg(long)]
    pub scenario: String,
    /// Path to the baseline JSON. Defaults to
    /// `benchmarks/baselines/<scenario>.json`.
    #[arg(long)]
    pub baseline: Option<String>,
    /// Path to the candidate JSON. Defaults to the most-recent file
    /// in `--results-dir` whose name starts with the scenario name.
    #[arg(long)]
    pub candidate: Option<String>,
    /// Allowed throughput drop, expressed as a positive percentage.
    /// Default 10% — single-run noise on a laptop is ±5–10% so we
    /// don't gate tighter than that without a sweep.
    #[arg(long, default_value_t = 10.0)]
    pub throughput_pct: f64,
    /// Allowed latency increase, expressed as a positive percentage.
    #[arg(long, default_value_t = 25.0)]
    pub latency_pct: f64,
    /// Print the comparison table but always exit 0. Useful for CI
    /// where we want diagnostics without blocking the merge.
    #[arg(long)]
    pub report_only: bool,
    /// Override directory the candidate is looked up in.
    #[arg(long, default_value = "results")]
    pub results_dir: String,
    /// Output shape. `text` (default) is the plain console table;
    /// `markdown` emits a PR-comment-ready block with a status header
    /// and a GFM table the bench-pr-comment CI workflow posts via
    /// `marocchino/sticky-pull-request-comment`. JSON output is
    /// available via `--output-json` separately.
    #[arg(long, value_enum, default_value_t = CompareFormat::Text)]
    pub format: CompareFormat,
    /// Optional file path: write the rendered output to this file in
    /// addition to stdout. CI uses this to capture per-scenario
    /// markdown without parsing stdout.
    #[arg(long)]
    pub output: Option<String>,
}

#[derive(Copy, Clone, Debug, clap::ValueEnum)]
pub enum CompareFormat {
    Text,
    Markdown,
}

#[tokio::main]
async fn main() -> Result<()> {
    init_tracing();

    let cli = Cli::parse();
    std::fs::create_dir_all(&cli.results_dir)
        .with_context(|| format!("creating results directory {}", cli.results_dir))?;

    // Only install the exporter for live runs. `compare` and `sweep`
    // skip it: `compare` is pure local IO, and `sweep` shells out to
    // bench subprocesses that each install their own exporter on
    // different ports (or rebind on the same port across runs — we
    // intentionally serialize sweep points so this Just Works).
    let install_metrics = !matches!(cli.scenario, Scenario::Compare(_) | Scenario::Sweep(_));
    if install_metrics {
        metrics::install_prometheus_exporter(&cli.metrics_addr)
            .context("installing prometheus exporter")?;
    }

    let common = CommonContext {
        postgres_url: cli.postgres_url,
        prometheus_url: cli.prometheus_url,
        results_dir: cli.results_dir,
        reset_db: !cli.no_reset_db,
    };

    let result = match cli.scenario {
        Scenario::Linear(args) => workloads::linear::run(common, args).await,
        Scenario::Fanout(args) => workloads::fanout::run(common, args).await,
        Scenario::SignalDriven(args) => workloads::signal_driven::run(common, args).await,
        Scenario::SleepingGiants(args) => workloads::sleeping::run(common, args).await,
        Scenario::Sweep(args) => sweep::run(common, args).await,
        Scenario::Compare(args) => compare::run(args),
    };

    sayiir_runtime::trace_context::shutdown_tracing();
    result
}

#[derive(Clone)]
pub struct CommonContext {
    pub postgres_url: String,
    pub prometheus_url: String,
    pub results_dir: String,
    pub reset_db: bool,
}

impl CommonContext {
    #[must_use]
    pub fn poll_interval(ms: u64) -> Duration {
        Duration::from_millis(ms)
    }
}

/// Truncate Sayiir tables so each run starts with a clean slate.
pub async fn reset_sayiir_tables(pool: &sqlx::PgPool) -> anyhow::Result<()> {
    let tables = sayiir_postgres::WORKFLOW_CHILD_TABLES
        .iter()
        .copied()
        .chain(std::iter::once("sayiir_workflow_snapshots"))
        .collect::<Vec<_>>()
        .join(", ");
    sqlx::query(sqlx::AssertSqlSafe(format!("TRUNCATE TABLE {tables} RESTART IDENTITY")))
        .execute(pool)
        .await
        .map(|_| ())
        .map_err(anyhow::Error::from)
}

/// If `OTEL_EXPORTER_OTLP_ENDPOINT` is set, ship runtime spans there via Sayiir's
/// OTLP-aware init. Otherwise fall back to a plain stderr fmt subscriber.
fn init_tracing() {
    sayiir_runtime::trace_context::init_tracing("sayiir-bench");
}
