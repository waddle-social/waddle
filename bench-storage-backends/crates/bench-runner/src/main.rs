//! `bench-runner` — wires a backend to the workload generator and writes a
//! JSON report under `--out`.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use bench_core::metrics::LatencyRecorder;
use bench_core::report::{peak_rss_kib, RunReport};
use bench_core::store::StanzaStore;
use bench_core::workload::{Scale, WorkloadConfig, WorkloadRunner};
use clap::{Parser, ValueEnum};
use tracing_subscriber::EnvFilter;

#[derive(Debug, Clone, Copy, ValueEnum)]
enum Backend {
    Sqlite,
    Postgres,
    Clickhouse,
}

#[derive(Parser, Debug)]
#[command(about = "Waddle XMPP stanza database benchmark runner")]
struct Args {
    /// Backend to benchmark.
    #[arg(long, value_enum, default_value_t = Backend::Sqlite)]
    backend: Backend,

    /// Scale shortcut: `10`, `10k`, `1m`, or an explicit integer.
    #[arg(long, default_value = "10")]
    scale: String,

    /// Measured duration, e.g. `10s`, `60s`, `5m`.
    #[arg(long, default_value = "10s")]
    duration: humantime::Duration,

    /// Warmup duration (not measured). Use `0s` to skip.
    #[arg(long, default_value = "0s")]
    warmup: humantime::Duration,

    /// Ops per user per minute (drives the Poisson arrival rate).
    #[arg(long, default_value_t = 1.0)]
    ops_per_user_per_min: f64,

    /// Write probability (0.2 = 80/20 read/write).
    #[arg(long, default_value_t = 0.2)]
    p_write: f64,

    /// Output directory for `*.json` run reports and the DB file itself.
    #[arg(long, default_value = "results")]
    out: PathBuf,

    /// Read pool size (SQLite). 32 is a sensible default on NVMe.
    #[arg(long, default_value_t = 32)]
    reader_pool: u32,

    /// PostgreSQL connection string, e.g. postgres://user:pass@127.0.0.1:5432/db.
    #[arg(long)]
    postgres_url: Option<String>,

    /// Max PostgreSQL connections in the benchmark pool.
    #[arg(long, default_value_t = 64)]
    postgres_max_connections: u32,

    /// ClickHouse HTTP URL, e.g. http://127.0.0.1:8123.
    #[arg(long, default_value = "http://127.0.0.1:8123")]
    clickhouse_url: String,

    /// ClickHouse database name.
    #[arg(long, default_value = "default")]
    clickhouse_database: String,

    /// ClickHouse user.
    #[arg(long, default_value = "default")]
    clickhouse_user: String,

    /// ClickHouse password.
    #[arg(long, default_value = "")]
    clickhouse_password: String,

    /// Skip warmup *writes* and just run measured window.
    #[arg(long)]
    no_warmup: bool,

    /// Run SQLite with a shared-cache in-memory DB, snapshotted to disk at
    /// `--flush-interval`. Trades durability for throughput: on crash you
    /// lose up to one flush interval of writes.
    #[arg(long)]
    in_memory: bool,

    /// How often to snapshot the in-memory DB to disk. Only used with
    /// `--in-memory`.
    #[arg(long, default_value = "10s")]
    flush_interval: humantime::Duration,
}

fn parse_scale(s: &str) -> Result<Scale> {
    let t = s.trim().to_lowercase();
    Ok(match t.as_str() {
        "10" | "small" => Scale::Small,
        "10k" | "10_000" | "10000" | "medium" => Scale::Medium,
        "1m" | "1_000_000" | "1000000" | "large" => Scale::Large,
        "w250k" | "write250k" | "250k-writes" => Scale::Write250k,
        other => {
            let n: u64 = other
                .parse()
                .with_context(|| format!("cannot parse scale '{other}'"))?;
            Scale::Custom(n)
        }
    })
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .init();

    let args = Args::parse();
    let scale = parse_scale(&args.scale)?;
    std::fs::create_dir_all(&args.out)?;

    let ts = chrono::Utc::now().format("%Y%m%dT%H%M%SZ").to_string();
    let backend_tag = match args.backend {
        Backend::Sqlite => "sqlite",
        Backend::Postgres => "postgres",
        Backend::Clickhouse => "clickhouse",
    };
    let tag = format!("{backend_tag}-{}-{ts}", scale_tag(scale));
    let db_path = args.out.join(format!("{tag}.db"));
    let report_path = args.out.join(format!("{tag}.json"));

    tracing::info!(
        backend = backend_tag,
        sessions = scale.sessions(),
        warmup_secs = args.warmup.as_secs(),
        duration_secs = args.duration.as_secs(),
        ?db_path,
        ?report_path,
        "starting benchmark"
    );

    let warmup: Duration = args.warmup.into();
    let duration: Duration = args.duration.into();

    let cfg = WorkloadConfig {
        sessions: scale.sessions(),
        ops_per_user_per_min: args.ops_per_user_per_min,
        p_write: args.p_write,
        warmup,
        duration,
        max_in_flight: max_in_flight_for_scale(scale),
    };

    let metrics = Arc::new(LatencyRecorder::default());

    match args.backend {
        Backend::Sqlite => {
            let store = if args.in_memory {
                let name = format!("waddle-bench-{}-{ts}", scale_tag(scale));
                bench_sqlite::SqliteStore::open_with_backing(
                    bench_sqlite::SqliteBacking::Memory {
                        name,
                        snapshot_to: db_path.clone(),
                        flush_interval: args.flush_interval.into(),
                    },
                    args.reader_pool,
                )
                .map_err(|e| anyhow::anyhow!(e.to_string()))?
            } else {
                bench_sqlite::SqliteStore::open(&db_path, args.reader_pool)
                    .map_err(|e| anyhow::anyhow!(e.to_string()))?
            };
            store
                .init()
                .await
                .map_err(|e| anyhow::anyhow!(e.to_string()))?;
            run(
                store,
                cfg,
                metrics,
                backend_tag,
                scale,
                warmup,
                duration,
                args.ops_per_user_per_min,
                !args.no_warmup,
                &report_path,
            )
            .await?;
        }
        Backend::Postgres => {
            let postgres_url = args.postgres_url.as_deref().ok_or_else(|| {
                anyhow::anyhow!("--postgres-url is required for --backend postgres")
            })?;
            let store =
                bench_postgres::PostgresStore::connect(postgres_url, args.postgres_max_connections)
                    .await?;
            store
                .init()
                .await
                .map_err(|e| anyhow::anyhow!(e.to_string()))?;
            run(
                store,
                cfg,
                metrics,
                backend_tag,
                scale,
                warmup,
                duration,
                args.ops_per_user_per_min,
                !args.no_warmup,
                &report_path,
            )
            .await?;
        }
        Backend::Clickhouse => {
            let store = bench_clickhouse::ClickHouseStore::connect(
                &args.clickhouse_url,
                &args.clickhouse_database,
                &args.clickhouse_user,
                &args.clickhouse_password,
            );
            store
                .init()
                .await
                .map_err(|e| anyhow::anyhow!(e.to_string()))?;
            run(
                store,
                cfg,
                metrics,
                backend_tag,
                scale,
                warmup,
                duration,
                args.ops_per_user_per_min,
                !args.no_warmup,
                &report_path,
            )
            .await?;
        }
    }

    tracing::info!(?report_path, "done");
    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn run<S: StanzaStore>(
    store: Arc<S>,
    cfg: WorkloadConfig,
    metrics: Arc<LatencyRecorder>,
    backend_tag: &str,
    scale: Scale,
    warmup: Duration,
    duration: Duration,
    ops_per_user_per_min: f64,
    do_warmup: bool,
    report_path: &std::path::Path,
) -> Result<()> {
    let runner = WorkloadRunner::new(store.clone(), cfg.clone(), metrics.clone());

    if do_warmup && warmup > Duration::ZERO {
        tracing::info!("warmup: seeding archive");
        runner
            .warmup()
            .await
            .map_err(|e| anyhow::anyhow!(e.to_string()))?;
    }

    tracing::info!("measured window starting");
    let samples = runner
        .run()
        .await
        .map_err(|e| anyhow::anyhow!(e.to_string()))?;

    let counters = runner.counters();
    let writes = counters.writes.load(std::sync::atomic::Ordering::Relaxed);
    let reads = counters.reads.load(std::sync::atomic::Ordering::Relaxed);
    let db_size = store.db_size_bytes().await.unwrap_or(0);
    let diagnostics = store.diagnostics().await;

    let report = RunReport {
        backend: backend_tag.to_string(),
        scale: scale.sessions(),
        warmup_seconds: warmup.as_secs(),
        duration_seconds: duration.as_secs(),
        target_ops_per_user_per_min: ops_per_user_per_min,
        total_writes: writes,
        total_reads: reads,
        db_size_bytes: db_size,
        peak_rss_kib: peak_rss_kib(),
        metrics: metrics.snapshot(),
        diagnostics,
        throughput_samples: samples,
    };

    let json = serde_json::to_string_pretty(&report)?;
    std::fs::write(report_path, &json)
        .with_context(|| format!("writing {}", report_path.display()))?;

    tracing::info!(
        total_writes = writes,
        total_reads = reads,
        db_size_bytes = db_size,
        "run complete"
    );
    println!("{json}");
    if writes == 0 && reads == 0 {
        bail!("no ops completed — configuration problem");
    }
    Ok(())
}

fn scale_tag(s: Scale) -> &'static str {
    match s {
        Scale::Small => "10",
        Scale::Medium => "10k",
        Scale::Large => "1m",
        Scale::Write250k => "w250k",
        Scale::Custom(_) => "custom",
    }
}

fn max_in_flight_for_scale(s: Scale) -> usize {
    match s {
        Scale::Small => 64,
        Scale::Medium => 2048,
        Scale::Large => 8192,
        Scale::Write250k => 32_768,
        Scale::Custom(n) => (n / 64).clamp(64, 8192) as usize,
    }
}
