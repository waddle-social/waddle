use anyhow::{anyhow, Context, Result};
use serde::Deserialize;
use std::collections::BTreeMap;
use std::path::Path;

#[derive(Debug, Deserialize)]
pub struct ScenarioFile {
    pub scenario: Scenario,
}

#[derive(Debug, Deserialize)]
pub struct Scenario {
    pub name: String,
    pub users: Vec<User>,
    pub steps: Vec<Step>,
}

#[derive(Debug, Deserialize)]
pub struct User {
    pub id: String,
    pub devices: Vec<Device>,
}

#[derive(Debug, Deserialize)]
pub struct Device {
    pub id: String,
    pub username: String,
    pub resource: String,
}

#[derive(Debug, Deserialize)]
pub struct Step {
    #[serde(default)]
    pub send: Option<SendStep>,
    #[serde(default, rename = "expectStanza")]
    pub expect_stanza: Option<ExpectStanzaStep>,
    #[serde(default, rename = "expectDb")]
    pub expect_db: Option<ExpectDbStep>,
}

#[derive(Debug, Deserialize)]
pub struct SendStep {
    pub actor: String,
    pub stanza: String,
}

#[derive(Debug, Deserialize)]
pub struct ExpectStanzaStep {
    pub target: String,
    pub contains: Vec<String>,
}

#[derive(Debug, Deserialize)]
pub struct ExpectDbStep {
    pub table: String,
    #[serde(rename = "where")]
    pub where_clause: BTreeMap<String, String>,
    #[serde(rename = "minRows")]
    pub min_rows: u64,
}

pub fn load_scenario_from_dir(dir_path: &Path, package_name: &str) -> Result<Scenario> {
    let parsed: ScenarioFile = cuengine::evaluate_cue_package_typed(dir_path, package_name)
        .with_context(|| format!("failed to evaluate CUE scenario in {}", dir_path.display()))?;
    validate(&parsed.scenario)?;
    Ok(parsed.scenario)
}

fn validate(scenario: &Scenario) -> Result<()> {
    if scenario.users.is_empty() {
        return Err(anyhow!("scenario.users must not be empty"));
    }
    if scenario.steps.is_empty() {
        return Err(anyhow!("scenario.steps must not be empty"));
    }

    let mut devices = std::collections::HashSet::new();
    for user in &scenario.users {
        if user.devices.is_empty() {
            return Err(anyhow!("user '{}' must define at least one device", user.id));
        }
        for device in &user.devices {
            if !devices.insert(device.id.as_str()) {
                return Err(anyhow!("duplicate device id '{}'", device.id));
            }
        }
    }

    for (index, step) in scenario.steps.iter().enumerate() {
        let variants = step.send.is_some() as u8
            + step.expect_stanza.is_some() as u8
            + step.expect_db.is_some() as u8;
        if variants != 1 {
            return Err(anyhow!(
                "step {} must contain exactly one of send/expectStanza/expectDb",
                index
            ));
        }
        if let Some(send) = &step.send {
            if !devices.contains(send.actor.as_str()) {
                return Err(anyhow!(
                    "step {} references unknown actor '{}'",
                    index,
                    send.actor
                ));
            }
        }
        if let Some(expect) = &step.expect_stanza {
            if !devices.contains(expect.target.as_str()) {
                return Err(anyhow!(
                    "step {} references unknown target '{}'",
                    index,
                    expect.target
                ));
            }
            if expect.contains.is_empty() {
                return Err(anyhow!(
                    "step {} expectStanza.contains must not be empty",
                    index
                ));
            }
        }
        if let Some(expect_db) = &step.expect_db {
            if expect_db.table.is_empty() {
                return Err(anyhow!("step {} expectDb.table must not be empty", index));
            }
        }
    }
    Ok(())
}
