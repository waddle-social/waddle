//! CUE-authored XMPP E2E scenarios over the active WebSocket C2S transport.

mod ws_common;

use anyhow::{anyhow, Context, Result};
use jid::Jid;
use serde::Deserialize;
use std::collections::{BTreeMap, HashMap, VecDeque};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use tokio::sync::Mutex;
use waddle_xmpp::Stanza;
use ws_common::{TestServer, WsXmppClient};
use xmpp_parsers::iq::{Iq, IqType};
use xmpp_parsers::message::{Body, Message, MessageType};
use xmpp_parsers::minidom::Element;
use xmpp_parsers::presence::Presence;

static TEST_SERIAL: Mutex<()> = Mutex::const_new(());
const RECV_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Debug, Deserialize)]
struct ScenarioFile {
    scenario: Scenario,
}

#[derive(Debug, Deserialize)]
struct Scenario {
    name: String,
    domain: String,
    users: BTreeMap<String, User>,
    steps: Vec<Step>,
}

#[derive(Debug, Deserialize)]
struct User {
    devices: BTreeMap<String, Actor>,
}

#[derive(Debug, Clone, Deserialize)]
struct Actor {
    user: String,
    device: String,
    username: String,
    resource: String,
    #[serde(rename = "bareJid")]
    bare_jid: String,
    jid: String,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "kind")]
enum Step {
    #[serde(rename = "enableCarbons")]
    EnableCarbons { actor: Actor },
    #[serde(rename = "sendMessage")]
    SendMessage {
        from: Actor,
        to: Option<Actor>,
        #[serde(rename = "toJid")]
        to_jid: Option<String>,
        #[serde(rename = "type")]
        type_: MessageKind,
        id: Option<String>,
        body: Option<String>,
        #[serde(default)]
        payloads: Vec<Payload>,
    },
    #[serde(rename = "expectMessage")]
    ExpectMessage {
        target: Actor,
        body: Option<String>,
        from: Option<Actor>,
        #[serde(default)]
        payloads: Vec<Payload>,
        #[serde(default)]
        contains: Vec<String>,
    },
    #[serde(rename = "expectCarbon")]
    ExpectCarbon {
        target: Actor,
        carbon: CarbonKind,
        body: Option<String>,
        #[serde(default)]
        payloads: Vec<Payload>,
        #[serde(default)]
        contains: Vec<String>,
    },
    #[serde(rename = "joinMuc")]
    JoinMuc {
        actor: Actor,
        room: String,
        nick: String,
    },
    #[serde(rename = "setMucAffiliation")]
    SetMucAffiliation {
        actor: Actor,
        room: String,
        jid: String,
        affiliation: String,
        id: Option<String>,
    },
    #[serde(rename = "expectMucAffiliation")]
    ExpectMucAffiliation {
        actor: Actor,
        room: String,
        jid: String,
        affiliation: String,
        id: Option<String>,
    },
    #[serde(rename = "expectMucAdminDenied")]
    ExpectMucAdminDenied {
        actor: Actor,
        room: String,
        jid: String,
        affiliation: String,
        id: Option<String>,
    },
    #[serde(rename = "expectPresence")]
    ExpectPresence {
        target: Actor,
        contains: Vec<String>,
    },
    #[serde(rename = "queryMam")]
    QueryMam {
        actor: Actor,
        archive: String,
        id: Option<String>,
        max: u32,
    },
    #[serde(rename = "expectMamResult")]
    ExpectMamResult {
        body: Option<String>,
        #[serde(default)]
        payloads: Vec<Payload>,
        #[serde(default)]
        contains: Vec<String>,
    },
    #[serde(rename = "expectNoStanza")]
    ExpectNoStanza {
        target: Actor,
        body: Option<String>,
        #[serde(default)]
        contains: Vec<String>,
        millis: u64,
    },
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "lowercase")]
enum MessageKind {
    Chat,
    Normal,
    Groupchat,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "lowercase")]
enum CarbonKind {
    Sent,
    Received,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "kind")]
enum Payload {
    #[serde(rename = "fileShare")]
    FileShare {
        disposition: String,
        name: String,
        #[serde(rename = "mediaType")]
        media_type: String,
        size: u64,
        url: String,
    },
    #[serde(rename = "linkMetadata")]
    LinkMetadata {
        about: String,
        title: String,
        description: String,
        url: String,
    },
}

struct ScenarioContext {
    clients: HashMap<String, WsXmppClient>,
    pending_frames: HashMap<String, VecDeque<String>>,
    last_mam_frames: Vec<String>,
}

#[tokio::test]
async fn cue_scenarios_run_over_websocket() -> Result<()> {
    let _serial = TEST_SERIAL.lock().await;
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/xmpp_e2e_scenarios");
    let mut scenarios = Vec::new();
    for scenario_file in discover_scenario_files(&root)? {
        let scenario = load_scenario_from_file(&root, &scenario_file)
            .with_context(|| format!("load {}", scenario_file.display()))?;
        scenarios.push((scenario_file, scenario));
    }

    for (scenario_file, scenario) in scenarios {
        run_scenario(scenario)
            .await
            .with_context(|| format!("scenario {} failed", scenario_file.display()))?;
    }
    Ok(())
}

fn discover_scenario_files(root: &Path) -> Result<Vec<PathBuf>> {
    let mut files = Vec::new();
    for entry in fs::read_dir(root).with_context(|| format!("read {}", root.display()))? {
        let path = entry?.path();
        if path.extension().and_then(|ext| ext.to_str()) == Some("cue")
            && path.file_name().and_then(|name| name.to_str()) != Some("schema.cue")
        {
            files.push(path);
        }
    }
    files.sort();
    if files.is_empty() {
        return Err(anyhow!("no CUE scenario files in {}", root.display()));
    }
    Ok(files)
}

fn load_scenario_from_file(root: &Path, scenario_file: &Path) -> Result<Scenario> {
    let temp_dir = tempfile::tempdir().context("create temporary CUE package")?;
    copy_dir_recursive(&root.join("cue.mod"), &temp_dir.path().join("cue.mod"))?;
    fs::copy(root.join("schema.cue"), temp_dir.path().join("schema.cue"))?;
    fs::copy(scenario_file, temp_dir.path().join("scenario.cue"))?;

    let parsed: ScenarioFile =
        cuengine::evaluate_cue_package_typed(temp_dir.path(), "xmpp_e2e_scenarios")
            .with_context(|| format!("evaluate CUE package for {}", scenario_file.display()))?;
    validate_scenario(&parsed.scenario)?;
    Ok(parsed.scenario)
}

fn copy_dir_recursive(source: &Path, target: &Path) -> Result<()> {
    fs::create_dir_all(target).with_context(|| format!("create {}", target.display()))?;
    for entry in fs::read_dir(source).with_context(|| format!("read {}", source.display()))? {
        let entry = entry?;
        let path = entry.path();
        let destination = target.join(entry.file_name());
        if path.is_dir() {
            copy_dir_recursive(&path, &destination)?;
        } else {
            fs::copy(&path, &destination)
                .with_context(|| format!("copy {} to {}", path.display(), destination.display()))?;
        }
    }
    Ok(())
}

fn validate_scenario(scenario: &Scenario) -> Result<()> {
    if scenario.users.is_empty() {
        return Err(anyhow!("scenario {} has no users", scenario.name));
    }
    if scenario.steps.is_empty() {
        return Err(anyhow!("scenario {} has no steps", scenario.name));
    }
    Ok(())
}

async fn run_scenario(scenario: Scenario) -> Result<()> {
    let accounts = scenario_accounts(&scenario);
    let account_refs = accounts
        .iter()
        .map(|(username, password)| (username.as_str(), password.as_str()))
        .collect::<Vec<_>>();
    let server = TestServer::start_with_extra_accounts(&account_refs);
    let mut clients = HashMap::new();

    for user in scenario.users.values() {
        for actor in user.devices.values() {
            let admin_password = server.fixed_account_password();
            let password = accounts
                .get(&actor.username)
                .map(String::as_str)
                .or_else(|| (actor.username == "admin").then_some(admin_password))
                .ok_or_else(|| anyhow!("missing password for {}", actor.username))?;
            let client = WsXmppClient::connect_and_auth(
                &server.ws_url(),
                &scenario.domain,
                &actor.username,
                password,
                &actor.resource,
            )
            .await
            .map_err(|error| anyhow!("connect {}.{}: {error}", actor.user, actor.device))?;
            clients.insert(actor_key(actor), client);
        }
    }

    let mut ctx = ScenarioContext {
        clients,
        pending_frames: HashMap::new(),
        last_mam_frames: Vec::new(),
    };

    for (index, step) in scenario.steps.iter().enumerate() {
        execute_step(&mut ctx, step)
            .await
            .with_context(|| format!("step {index} in scenario {}", scenario.name))?;
    }

    close_clients(ctx.clients).await;
    Ok(())
}

fn scenario_accounts(scenario: &Scenario) -> BTreeMap<String, String> {
    let mut accounts = BTreeMap::new();
    for user in scenario.users.values() {
        for actor in user.devices.values() {
            if actor.username == "admin" {
                continue;
            }
            accounts
                .entry(actor.username.clone())
                .or_insert_with(|| format!("{}-{}", actor.username, uuid::Uuid::new_v4()));
        }
    }
    accounts
}

async fn close_clients(clients: HashMap<String, WsXmppClient>) {
    for client in clients.into_values() {
        client.close().await;
    }
}

async fn execute_step(ctx: &mut ScenarioContext, step: &Step) -> Result<()> {
    match step {
        Step::EnableCarbons { actor } => {
            let id = format!("cue-enable-carbons-{}", uuid::Uuid::new_v4());
            let enable = Element::builder("enable", "urn:xmpp:carbons:2").build();
            let iq = Iq {
                from: None,
                to: None,
                id: id.clone(),
                payload: IqType::Set(enable),
            };
            let client = client_mut(ctx, actor)?;
            client
                .send(&stanza_xml(Stanza::Iq(iq))?)
                .await
                .map_err(|error| anyhow!(error))?;
            let response = recv_matching(ctx, actor, |frame| frame.contains(&id)).await?;
            assert_contains_all(&response, ["type=\"result\""], "enable carbons response")?;
        }
        Step::SendMessage {
            from,
            to,
            to_jid,
            type_,
            id,
            body,
            payloads,
        } => {
            let to = to_jid
                .clone()
                .or_else(|| to.as_ref().map(|actor| actor.jid.clone()))
                .ok_or_else(|| anyhow!("sendMessage requires to or toJid"))?;
            let mut message = Message::new_with_type(message_type(type_), Some(to.parse::<Jid>()?));
            message.id = id.clone();
            if let Some(body) = body {
                message.bodies.insert(String::new(), Body(body.clone()));
            }
            for payload in payloads {
                message.payloads.push(payload_element(payload));
            }
            if body.is_some()
                && payloads
                    .iter()
                    .any(|payload| matches!(payload, Payload::FileShare { .. }))
            {
                validate_file_share_fallback_body(body.as_deref(), payloads)?;
                message.payloads.push(file_share_fallback_element());
            }
            let xml = stanza_xml(Stanza::Message(message))?;
            client_mut(ctx, from)?
                .send(&xml)
                .await
                .map_err(|error| anyhow!(error))?;
        }
        Step::ExpectMessage {
            target,
            body,
            from,
            payloads,
            contains,
        } => {
            let mut expected = contains.clone();
            if let Some(body) = body {
                expected.push(body_text_marker(body));
            }
            expected.extend(payload_expectations(payloads)?);
            if let Some(from) = from {
                expected.push(format!("from=\"{}", from.bare_jid));
            }
            let frame = recv_matching(ctx, target, |frame| {
                frame.contains("<message")
                    && body
                        .as_ref()
                        .is_none_or(|body| frame_contains_body(frame, body))
                    && from
                        .as_ref()
                        .is_none_or(|from| frame.contains(&format!("from=\"{}", from.bare_jid)))
                    && payload_expectations(payloads)
                        .map(|parts| parts.iter().all(|part| frame.contains(part)))
                        .unwrap_or(false)
                    && contains.iter().all(|part| frame.contains(part))
            })
            .await?;
            assert_contains_all(&frame, &expected, "message expectation")?;
        }
        Step::ExpectCarbon {
            target,
            carbon,
            body,
            payloads,
            contains,
        } => {
            let carbon_tag = match carbon {
                CarbonKind::Sent => "<sent",
                CarbonKind::Received => "<received",
            };
            let mut expected = contains.clone();
            expected.push("urn:xmpp:carbons:2".to_string());
            expected.push(carbon_tag.to_string());
            if let Some(body) = body {
                expected.push(body_text_marker(body));
            }
            expected.extend(payload_expectations(payloads)?);
            let frame = recv_matching(ctx, target, |frame| {
                frame.contains("urn:xmpp:carbons:2")
                    && frame.contains(carbon_tag)
                    && body
                        .as_ref()
                        .is_none_or(|body| frame_contains_body(frame, body))
                    && payload_expectations(payloads)
                        .map(|parts| parts.iter().all(|part| frame.contains(part)))
                        .unwrap_or(false)
                    && contains.iter().all(|part| frame.contains(part))
            })
            .await?;
            assert_contains_all(&frame, &expected, "carbon expectation")?;
        }
        Step::JoinMuc { actor, room, nick } => {
            let mut presence = Presence::available();
            presence.to = Some(format!("{room}/{nick}").parse()?);
            presence.payloads.push(
                Element::builder("x", "http://jabber.org/protocol/muc")
                    .append(
                        Element::builder("history", "http://jabber.org/protocol/muc")
                            .attr("maxstanzas", "0")
                            .build(),
                    )
                    .build(),
            );
            let xml = stanza_xml(Stanza::Presence(presence))?;
            let client = client_mut(ctx, actor)?;
            client.send(&xml).await.map_err(|error| anyhow!(error))?;
            recv_until(ctx, actor, |frame| {
                frame.contains("status code=\"110\"") || frame.contains("<subject")
            })
            .await?;
        }
        Step::SetMucAffiliation {
            actor,
            room,
            jid,
            affiliation,
            id,
        } => {
            let id = id
                .clone()
                .unwrap_or_else(|| format!("cue-muc-admin-set-{}", uuid::Uuid::new_v4()));
            send_muc_admin_iq(ctx, actor, room, jid, affiliation, &id, IqKind::Set).await?;
            let response = recv_matching(ctx, actor, |frame| frame.contains(&id)).await?;
            assert_contains_all(&response, ["type=\"result\""], "MUC admin set response")?;
        }
        Step::ExpectMucAffiliation {
            actor,
            room,
            jid,
            affiliation,
            id,
        } => {
            let id = id
                .clone()
                .unwrap_or_else(|| format!("cue-muc-admin-get-{}", uuid::Uuid::new_v4()));
            send_muc_admin_iq(ctx, actor, room, jid, affiliation, &id, IqKind::Get).await?;
            let response = recv_matching(ctx, actor, |frame| frame.contains(&id)).await?;
            assert_contains_all(
                &response,
                [
                    "type=\"result\"",
                    "http://jabber.org/protocol/muc#admin",
                    jid.as_str(),
                    affiliation.as_str(),
                ],
                "MUC admin affiliation query",
            )?;
        }
        Step::ExpectMucAdminDenied {
            actor,
            room,
            jid,
            affiliation,
            id,
        } => {
            let id = id
                .clone()
                .unwrap_or_else(|| format!("cue-muc-admin-denied-{}", uuid::Uuid::new_v4()));
            send_muc_admin_iq(ctx, actor, room, jid, affiliation, &id, IqKind::Set).await?;
            let response = recv_matching(ctx, actor, |frame| frame.contains(&id)).await?;
            assert_contains_all(
                &response,
                ["type=\"error\"", "forbidden"],
                "MUC admin denial",
            )?;
        }
        Step::ExpectPresence { target, contains } => {
            let frame = recv_matching(ctx, target, |frame| {
                frame.contains("<presence") && contains.iter().all(|part| frame.contains(part))
            })
            .await?;
            assert_contains_all(&frame, contains, "presence expectation")?;
        }
        Step::QueryMam {
            actor,
            archive,
            id,
            max,
        } => {
            let id = id
                .clone()
                .unwrap_or_else(|| format!("cue-mam-{}", uuid::Uuid::new_v4()));
            let rsm = Element::builder("set", "http://jabber.org/protocol/rsm")
                .append(
                    Element::builder("max", "http://jabber.org/protocol/rsm")
                        .append(max.to_string())
                        .build(),
                )
                .build();
            let query = Element::builder("query", "urn:xmpp:mam:2")
                .append(rsm)
                .build();
            let iq = Iq {
                from: None,
                to: Some(archive.parse()?),
                id: id.clone(),
                payload: IqType::Set(query),
            };
            client_mut(ctx, actor)?
                .send(&stanza_xml(Stanza::Iq(iq))?)
                .await
                .map_err(|error| anyhow!(error))?;
            ctx.last_mam_frames = recv_until(ctx, actor, |frame| {
                frame.contains("urn:xmpp:mam:2") && frame.contains("<fin") && frame.contains(&id)
            })
            .await?;
        }
        Step::ExpectMamResult {
            body,
            payloads,
            contains,
        } => {
            let payload_expectations = payload_expectations(payloads)?;
            let matched = ctx.last_mam_frames.iter().find(|frame| {
                frame.contains("<forwarded")
                    && body
                        .as_ref()
                        .is_none_or(|body| frame_contains_body(frame, body))
                    && payload_expectations.iter().all(|part| frame.contains(part))
                    && contains.iter().all(|part| frame.contains(part))
            });
            let Some(frame) = matched else {
                return Err(anyhow!(
                    "no MAM result matched body {:?} and contains {:?}; frames: {:?}",
                    body,
                    contains,
                    ctx.last_mam_frames
                ));
            };
            if let Some(body) = body {
                assert_contains_all(frame, std::slice::from_ref(body), "MAM result body")?;
            }
            assert_contains_all(frame, &payload_expectations, "MAM result payloads")?;
            assert_contains_all(frame, contains, "MAM result contains")?;
        }
        Step::ExpectNoStanza {
            target,
            body,
            contains,
            millis,
        } => {
            let deadline = Instant::now() + Duration::from_millis(*millis);
            let mut non_matching_frames = Vec::new();
            loop {
                let now = Instant::now();
                if now >= deadline {
                    break;
                }
                let Some(frame) = recv_timeout(ctx, target, deadline - now).await? else {
                    break;
                };
                let matches = body
                    .as_ref()
                    .is_none_or(|body| frame_contains_body(&frame, body))
                    && contains.iter().all(|part| frame.contains(part));
                if matches {
                    return Err(anyhow!("unexpected matching stanza: {frame}"));
                }
                non_matching_frames.push(frame);
            }
            for frame in non_matching_frames.into_iter().rev() {
                push_pending_front(ctx, target, frame);
            }
        }
    }
    Ok(())
}

async fn recv_matching<F>(ctx: &mut ScenarioContext, actor: &Actor, predicate: F) -> Result<String>
where
    F: Fn(&str) -> bool,
{
    let mut non_matching_frames = Vec::new();
    loop {
        let frame = recv_next(ctx, actor).await?;
        if predicate(&frame) {
            for frame in non_matching_frames.into_iter().rev() {
                push_pending_front(ctx, actor, frame);
            }
            return Ok(frame);
        }
        non_matching_frames.push(frame);
    }
}

async fn recv_until<F>(
    ctx: &mut ScenarioContext,
    actor: &Actor,
    predicate: F,
) -> Result<Vec<String>>
where
    F: Fn(&str) -> bool,
{
    let mut frames = Vec::new();
    loop {
        let frame = recv_next(ctx, actor).await?;
        let done = predicate(&frame);
        frames.push(frame);
        if done {
            return Ok(frames);
        }
    }
}

async fn recv_next(ctx: &mut ScenarioContext, actor: &Actor) -> Result<String> {
    recv_timeout(ctx, actor, RECV_TIMEOUT)
        .await?
        .ok_or_else(|| anyhow!("Timeout waiting for message"))
}

async fn recv_timeout(
    ctx: &mut ScenarioContext,
    actor: &Actor,
    timeout: Duration,
) -> Result<Option<String>> {
    let key = actor_key(actor);
    if let Some(frame) = ctx
        .pending_frames
        .get_mut(&key)
        .and_then(VecDeque::pop_front)
    {
        return Ok(Some(frame));
    }
    match client_mut(ctx, actor)?.recv_timeout(timeout).await {
        Ok(frame) => Ok(Some(frame)),
        Err(error) if error == "Timeout waiting for message" => Ok(None),
        Err(error) => Err(anyhow!(error)),
    }
}

fn push_pending_front(ctx: &mut ScenarioContext, actor: &Actor, frame: String) {
    ctx.pending_frames
        .entry(actor_key(actor))
        .or_default()
        .push_front(frame);
}

fn client_mut<'a>(ctx: &'a mut ScenarioContext, actor: &Actor) -> Result<&'a mut WsXmppClient> {
    ctx.clients
        .get_mut(&actor_key(actor))
        .ok_or_else(|| anyhow!("unknown actor {}.{}", actor.user, actor.device))
}

fn actor_key(actor: &Actor) -> String {
    format!("{}.{}", actor.user, actor.device)
}

fn message_type(kind: &MessageKind) -> MessageType {
    match kind {
        MessageKind::Chat => MessageType::Chat,
        MessageKind::Normal => MessageType::Normal,
        MessageKind::Groupchat => MessageType::Groupchat,
    }
}

enum IqKind {
    Get,
    Set,
}

async fn send_muc_admin_iq(
    ctx: &mut ScenarioContext,
    actor: &Actor,
    room: &str,
    jid: &str,
    affiliation: &str,
    id: &str,
    kind: IqKind,
) -> Result<()> {
    let item = match kind {
        IqKind::Get => Element::builder("item", "http://jabber.org/protocol/muc#admin")
            .attr("affiliation", affiliation)
            .build(),
        IqKind::Set => Element::builder("item", "http://jabber.org/protocol/muc#admin")
            .attr("jid", jid)
            .attr("affiliation", affiliation)
            .build(),
    };
    let query = Element::builder("query", "http://jabber.org/protocol/muc#admin")
        .append(item)
        .build();
    let payload = match kind {
        IqKind::Get => IqType::Get(query),
        IqKind::Set => IqType::Set(query),
    };
    let iq = Iq {
        from: None,
        to: Some(room.parse()?),
        id: id.to_string(),
        payload,
    };
    client_mut(ctx, actor)?
        .send(&stanza_xml(Stanza::Iq(iq))?)
        .await
        .map_err(|error| anyhow!(error))?;
    Ok(())
}

fn payload_element(payload: &Payload) -> Element {
    match payload {
        Payload::FileShare {
            disposition,
            name,
            media_type,
            size,
            url,
        } => Element::builder("file-sharing", "urn:xmpp:sfs:0")
            .attr("disposition", disposition.as_str())
            .append(
                Element::builder("file", "urn:xmpp:file:metadata:0")
                    .append(
                        Element::builder("media-type", "urn:xmpp:file:metadata:0")
                            .append(media_type.as_str())
                            .build(),
                    )
                    .append(
                        Element::builder("name", "urn:xmpp:file:metadata:0")
                            .append(name.as_str())
                            .build(),
                    )
                    .append(
                        Element::builder("size", "urn:xmpp:file:metadata:0")
                            .append(size.to_string())
                            .build(),
                    )
                    .build(),
            )
            .append(
                Element::builder("sources", "urn:xmpp:sfs:0")
                    .append(
                        Element::builder("url-data", "http://jabber.org/protocol/url-data")
                            .attr("target", url.as_str())
                            .build(),
                    )
                    .build(),
            )
            .build(),
        Payload::LinkMetadata {
            about,
            title,
            description,
            url,
        } => Element::builder("Description", "http://www.w3.org/1999/02/22-rdf-syntax-ns#")
            .prefix(
                Some("rdf".to_string()),
                "http://www.w3.org/1999/02/22-rdf-syntax-ns#",
            )
            .expect("static RDF prefix is unique")
            .attr("rdf:about", about.as_str())
            .append(
                Element::builder("title", "https://ogp.me/ns#")
                    .append(title.as_str())
                    .build(),
            )
            .append(
                Element::builder("description", "https://ogp.me/ns#")
                    .append(description.as_str())
                    .build(),
            )
            .append(
                Element::builder("url", "https://ogp.me/ns#")
                    .append(url.as_str())
                    .build(),
            )
            .build(),
    }
}

fn payload_expectations(payloads: &[Payload]) -> Result<Vec<String>> {
    let mut expected = Vec::new();
    for payload in payloads {
        match payload {
            Payload::FileShare {
                disposition,
                name,
                media_type,
                size,
                url,
            } => {
                expected.extend([
                    "urn:xmpp:sfs:0".to_string(),
                    "urn:xmpp:file:metadata:0".to_string(),
                    "http://jabber.org/protocol/url-data".to_string(),
                    "disposition=".to_string(),
                    disposition.clone(),
                    text_node_marker(media_type),
                    text_node_marker(name),
                    text_node_marker(&size.to_string()),
                    "target=".to_string(),
                    url.clone(),
                ]);
            }
            Payload::LinkMetadata {
                about,
                title,
                description,
                url,
            } => {
                expected.extend([
                    "http://www.w3.org/1999/02/22-rdf-syntax-ns#".to_string(),
                    "https://ogp.me/ns#".to_string(),
                    "rdf:about=".to_string(),
                    about.clone(),
                    text_node_marker(title),
                    text_node_marker(description),
                    text_node_marker(url),
                ]);
            }
        }
    }
    Ok(expected)
}

fn validate_file_share_fallback_body(body: Option<&str>, payloads: &[Payload]) -> Result<()> {
    let Some(body) = body else {
        return Ok(());
    };
    let represented_by_payload = payloads.iter().any(|payload| match payload {
        Payload::FileShare { url, .. } => body == url,
        Payload::LinkMetadata { .. } => false,
    });
    if represented_by_payload {
        Ok(())
    } else {
        Err(anyhow!(
            "fileShare body is marked as XEP-0428 fallback, so it must be represented by the file-sharing payload"
        ))
    }
}

fn file_share_fallback_element() -> Element {
    Element::builder("fallback", "urn:xmpp:fallback:0")
        .attr("for", "urn:xmpp:sfs:0")
        .append(Element::builder("body", "urn:xmpp:fallback:0").build())
        .build()
}

fn stanza_xml(stanza: Stanza) -> Result<String> {
    let mut buf = Vec::new();
    stanza.to_element().write_to(&mut buf)?;
    Ok(String::from_utf8(buf)?)
}

fn frame_contains_body(frame: &str, body: &str) -> bool {
    frame.contains("<body") && frame.contains(&body_text_marker(body))
}

fn body_text_marker(body: &str) -> String {
    format!(">{body}</body>")
}

fn text_node_marker(value: &str) -> String {
    format!(">{value}</")
}

fn assert_contains_all<I, S>(frame: &str, expected: I, context: &str) -> Result<()>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    for part in expected {
        let part = part.as_ref();
        if !frame.contains(part) {
            return Err(anyhow!("{context} expected {part:?}, got: {frame}"));
        }
    }
    Ok(())
}
