use std::env;
use std::error::Error;
use std::fs::File;
use std::io::{self, Read, Write};
use std::path::Path;

use serde::de::DeserializeOwned;
use serde::Serialize;
use waddle_operational_qualification::{
    account, LocalIsolationManifest, LocalIsolationPlan, Manifest, ObservationSet, Plan,
};

const MAX_INPUT_BYTES: u64 = 8 * 1024 * 1024;

fn read_json<T: DeserializeOwned>(path: &Path) -> Result<T, Box<dyn Error>> {
    parse_json(File::open(path)?, MAX_INPUT_BYTES, "input exceeds 8 MiB")
}

fn parse_json<T: DeserializeOwned>(
    input: impl Read,
    limit: u64,
    oversized_message: &'static str,
) -> Result<T, Box<dyn Error>> {
    let mut bytes = Vec::new();
    input.take(limit + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > limit {
        return Err(oversized_message.into());
    }
    // Do not echo input or parser excerpts: an invalid input might contain secrets.
    serde_json::from_slice(&bytes).map_err(|_| "input is not valid contract JSON".into())
}

fn write_json(value: &impl Serialize) -> Result<(), Box<dyn Error>> {
    let mut output = io::stdout().lock();
    serde_json::to_writer_pretty(&mut output, value)?;
    writeln!(output)?;
    Ok(())
}

fn run() -> Result<(), Box<dyn Error>> {
    let args: Vec<_> = env::args_os().skip(1).collect();
    match args.as_slice() {
        [command] if command == "local-isolation" => {
            let manifest = parse_json::<LocalIsolationManifest>(
                io::stdin().lock(),
                64 * 1024,
                "input exceeds 64 KiB",
            )?;
            write_json(&LocalIsolationPlan::build(manifest)?)
        }
        [command, manifest] if command == "plan" => {
            write_json(&Plan::build(read_json::<Manifest>(Path::new(manifest))?)?)
        }
        [command, manifest, observations] if command == "account" => {
            let plan = Plan::build(read_json::<Manifest>(Path::new(manifest))?)?;
            let observations = read_json::<ObservationSet>(Path::new(observations))?;
            write_json(&account(plan, observations)?)
        }
        _ => Err("usage: waddle-operational-qualification local-isolation < DECLARATION.json | plan MANIFEST.json | account MANIFEST.json OBSERVATIONS.json".into()),
    }
}

fn main() {
    if let Err(error) = run() {
        eprintln!("{error}");
        std::process::exit(1);
    }
}
