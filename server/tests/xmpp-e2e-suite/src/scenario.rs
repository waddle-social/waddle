use anyhow::{anyhow, Context, Result};
use serde::Deserialize;
use std::collections::{BTreeMap, HashSet};
use std::path::Path;

#[derive(Debug, Deserialize)]
pub struct ScenarioFile {
    pub scenario: Scenario,
}

#[derive(Debug, Deserialize)]
pub struct Scenario {
    pub name: String,
    pub users: BTreeMap<String, User>,
    #[serde(default)]
    pub fixtures: ScenarioFixtures,
    pub steps: Vec<Step>,
}

#[derive(Debug, Deserialize)]
pub struct User {
    pub devices: BTreeMap<String, Device>,
}

#[derive(Debug, Deserialize)]
pub struct Device {
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
    pub actor: ActorRef,
    pub stanza: String,
}

#[derive(Debug, Deserialize)]
pub struct ExpectStanzaStep {
    pub target: ActorRef,
    pub contains: Vec<String>,
    #[serde(default = "default_expect_stanza_until")]
    pub until: String,
}

#[derive(Debug, Deserialize)]
pub struct ActorRef {
    pub user: String,
    pub device: String,
}

#[derive(Debug, Deserialize)]
pub struct ExpectDbStep {
    pub table: String,
    #[serde(rename = "where")]
    pub where_clause: BTreeMap<String, String>,
    #[serde(rename = "minRows")]
    pub min_rows: u64,
}

#[derive(Debug, Default, Deserialize)]
pub struct ScenarioFixtures {
    #[serde(default)]
    pub channels: Vec<ChannelFixture>,
    #[serde(default, rename = "permissionGrants")]
    pub permission_grants: Vec<PermissionGrant>,
}

#[derive(Debug, Deserialize)]
pub struct ChannelFixture {
    #[serde(rename = "waddleId")]
    pub waddle_id: String,
    #[serde(rename = "channelId")]
    pub channel_id: String,
    #[serde(rename = "channelName")]
    pub channel_name: String,
    #[serde(rename = "channelType")]
    pub channel_type: String,
}

#[derive(Debug, Deserialize)]
pub struct PermissionGrant {
    pub resource: String,
    pub relation: String,
    pub subject: String,
}

fn default_expect_stanza_until() -> String {
    "</message>".to_string()
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

    let mut actor_paths: HashSet<String> = HashSet::new();
    for (user_key, user) in &scenario.users {
        if user.devices.is_empty() {
            return Err(anyhow!(
                "user '{}' must define at least one device",
                user_key
            ));
        }
        for device_key in user.devices.keys() {
            actor_paths.insert(format!("{user_key}.{device_key}"));
        }
    }

    let actor_exists = |actor: &ActorRef| {
        actor_paths.contains(format!("{}.{}", actor.user, actor.device).as_str())
    };

    for (user_key, user) in &scenario.users {
        for (device_key, device) in &user.devices {
            if device.username.is_empty() {
                return Err(anyhow!(
                    "user '{}'.devices.'{}' username must not be empty",
                    user_key,
                    device_key
                ));
            }
            if device.resource.is_empty() {
                return Err(anyhow!(
                    "user '{}'.devices.'{}' resource must not be empty",
                    user_key,
                    device_key
                ));
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
            if !actor_exists(&send.actor) {
                return Err(anyhow!(
                    "step {} references unknown actor '{}.{}'",
                    index,
                    send.actor.user,
                    send.actor.device
                ));
            }
        }
        if let Some(expect) = &step.expect_stanza {
            if !actor_exists(&expect.target) {
                return Err(anyhow!(
                    "step {} references unknown target '{}.{}'",
                    index,
                    expect.target.user,
                    expect.target.device
                ));
            }
            if expect.contains.is_empty() {
                return Err(anyhow!(
                    "step {} expectStanza.contains must not be empty",
                    index
                ));
            }
            if expect.until.is_empty() {
                return Err(anyhow!("step {} expectStanza.until must not be empty", index));
            }
        }
        if let Some(expect_db) = &step.expect_db {
            if expect_db.table.is_empty() {
                return Err(anyhow!("step {} expectDb.table must not be empty", index));
            }
            if expect_db.min_rows == 0 {
                return Err(anyhow!(
                    "step {} expectDb.minRows must be at least 1",
                    index
                ));
            }
            if expect_db.table == "mam_messages" {
                match expect_db.where_clause.get("body") {
                    Some(body) if !body.is_empty() => {}
                    _ => {
                        return Err(anyhow!(
                            "step {} expectDb.where.body must not be empty when expectDb.table is 'mam_messages'",
                            index
                        ));
                    }
                }
            }
        }
    }

    for (index, channel) in scenario.fixtures.channels.iter().enumerate() {
        if channel.waddle_id.is_empty() {
            return Err(anyhow!(
                "fixtures.channels[{}].waddleId must not be empty",
                index
            ));
        }
        if channel.channel_id.is_empty() {
            return Err(anyhow!(
                "fixtures.channels[{}].channelId must not be empty",
                index
            ));
        }
        if channel.channel_name.is_empty() {
            return Err(anyhow!(
                "fixtures.channels[{}].channelName must not be empty",
                index
            ));
        }
        if channel.channel_type.is_empty() {
            return Err(anyhow!(
                "fixtures.channels[{}].channelType must not be empty",
                index
            ));
        }
    }

    for (index, grant) in scenario.fixtures.permission_grants.iter().enumerate() {
        if grant.resource.is_empty() {
            return Err(anyhow!(
                "fixtures.permissionGrants[{}].resource must not be empty",
                index
            ));
        }
        if grant.relation.is_empty() {
            return Err(anyhow!(
                "fixtures.permissionGrants[{}].relation must not be empty",
                index
            ));
        }
        if grant.subject.is_empty() {
            return Err(anyhow!(
                "fixtures.permissionGrants[{}].subject must not be empty",
                index
            ));
        }
    }

    Ok(())
}
