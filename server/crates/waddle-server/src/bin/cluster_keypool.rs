use std::fs::{File, OpenOptions};
use std::io::{BufWriter, Write};
#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use base64::Engine;
use clap::{Parser, Subcommand};
use libp2p::identity::{ed25519, Keypair};

#[derive(Debug, Parser)]
#[command(name = "waddle-cluster-keypool")]
#[command(about = "Generate ADR-0017 clustering keypair pools and enrollment rows")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    Generate(GenerateArgs),
}

#[derive(Debug, Parser)]
struct GenerateArgs {
    #[arg(long, default_value_t = 4)]
    count: usize,
    #[arg(long)]
    pool_output: PathBuf,
    #[arg(long)]
    peer_ids_output: PathBuf,
    #[arg(long)]
    sql_output: Option<PathBuf>,
}

struct GeneratedKey {
    seed_b64: String,
    peer_id: String,
}

fn main() -> Result<()> {
    match Cli::parse().command {
        Command::Generate(args) => generate(args),
    }
}

fn generate(args: GenerateArgs) -> Result<()> {
    anyhow::ensure!(args.count > 0, "--count must be greater than zero");

    let keys = (0..args.count)
        .map(|_| {
            let keypair = ed25519::Keypair::generate();
            let seed_b64 =
                base64::engine::general_purpose::STANDARD.encode(keypair.secret().as_ref());
            let peer_id = Keypair::from(keypair).public().to_peer_id().to_string();
            GeneratedKey { seed_b64, peer_id }
        })
        .collect::<Vec<_>>();

    write_secret_pool(&args.pool_output, &keys)
        .with_context(|| format!("write {}", args.pool_output.display()))?;
    write_peer_ids(&args.peer_ids_output, &keys)
        .with_context(|| format!("write {}", args.peer_ids_output.display()))?;
    if let Some(path) = args.sql_output.as_ref() {
        write_sql(path, &keys).with_context(|| format!("write {}", path.display()))?;
    }

    println!(
        "generated {} clustering keypairs; wrote secret pool to {} and peer IDs to {}",
        keys.len(),
        args.pool_output.display(),
        args.peer_ids_output.display()
    );
    Ok(())
}

fn write_secret_pool(path: &Path, keys: &[GeneratedKey]) -> std::io::Result<()> {
    #[cfg(unix)]
    let file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)?;

    #[cfg(not(unix))]
    let file = OpenOptions::new().write(true).create_new(true).open(path)?;

    let mut writer = BufWriter::new(file);
    for (index, key) in keys.iter().enumerate() {
        if index > 0 {
            writer.write_all(b",")?;
        }
        writer.write_all(key.seed_b64.as_bytes())?;
    }
    writer.write_all(b"\n")?;
    writer.flush()
}

fn write_peer_ids(path: &Path, keys: &[GeneratedKey]) -> std::io::Result<()> {
    let file = File::create(path)?;
    let mut writer = BufWriter::new(file);
    for key in keys {
        writeln!(writer, "{}", key.peer_id)?;
    }
    writer.flush()
}

fn write_sql(path: &Path, keys: &[GeneratedKey]) -> std::io::Result<()> {
    let file = File::create(path)?;
    let mut writer = BufWriter::new(file);
    writeln!(
        writer,
        "CREATE TABLE IF NOT EXISTS clustering_peer_allowlist (peer_id TEXT PRIMARY KEY, enrolled_at TIMESTAMPTZ NOT NULL DEFAULT now());"
    )?;
    for key in keys {
        writeln!(
            writer,
            "INSERT INTO clustering_peer_allowlist (peer_id) VALUES ('{}') ON CONFLICT (peer_id) DO NOTHING;",
            key.peer_id
        )?;
    }
    writer.flush()
}
