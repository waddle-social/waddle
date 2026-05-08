//! CUE-authored XMPP E2E scenarios over the active WebSocket C2S transport.

mod ws_common;

use anyhow::{anyhow, Context, Result};
use jid::Jid;
use serde::Deserialize;
use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};
use std::fs;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::time::{Duration, Instant};
use tokio::sync::Mutex;
use waddle_xmpp::xep::xep0334::{self, Hint};
use waddle_xmpp::xep::xep0444;
use waddle_xmpp::Stanza;
use ws_common::{TestServer, WsXmppClient};
use xmpp_parsers::iq::{Iq, IqType};
use xmpp_parsers::message::{Body, Message, MessageType};
use xmpp_parsers::minidom::Element;
use xmpp_parsers::presence::{Presence, Show as PresenceShow, Type as PresenceType};

static TEST_SERIAL: Mutex<()> = Mutex::const_new(());
const RECV_TIMEOUT: Duration = Duration::from_secs(10);
const SUPPORTED_XEPS: &[&str] = &[
    "XEP-0004", "XEP-0012", "XEP-0030", "XEP-0045", "XEP-0048", "XEP-0049", "XEP-0050", "XEP-0054",
    "XEP-0055", "XEP-0059", "XEP-0060", "XEP-0084", "XEP-0085", "XEP-0092", "XEP-0103", "XEP-0107",
    "XEP-0108", "XEP-0115", "XEP-0118", "XEP-0153", "XEP-0160", "XEP-0163", "XEP-0184", "XEP-0191",
    "XEP-0198", "XEP-0199", "XEP-0201", "XEP-0202", "XEP-0203", "XEP-0237", "XEP-0280", "XEP-0297",
    "XEP-0308", "XEP-0313", "XEP-0317", "XEP-0333", "XEP-0334", "XEP-0357", "XEP-0359", "XEP-0363",
    "XEP-0372", "XEP-0402", "XEP-0410", "XEP-0421", "XEP-0424", "XEP-0425", "XEP-0428", "XEP-0431",
    "XEP-0433", "XEP-0444", "XEP-0446", "XEP-0447", "XEP-0461", "XEP-0503", "XEP-0511", "XEP-0513",
];

#[derive(Debug, Deserialize)]
struct ScenarioFile {
    scenario: Scenario,
}

#[derive(Debug, Deserialize)]
struct Scenario {
    name: String,
    #[serde(default)]
    xeps: Vec<String>,
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
    #[serde(rename = "streamManagement")]
    StreamManagement {
        actor: Actor,
        action: StreamManagementAction,
        #[serde(default)]
        resume: Option<bool>,
        #[serde(default)]
        max: Option<u32>,
    },
    #[serde(rename = "connectActor")]
    ConnectActor { actor: Actor },
    #[serde(rename = "disconnectActor")]
    DisconnectActor { actor: Actor },
    #[serde(rename = "sendIq")]
    SendIq {
        actor: Actor,
        #[serde(rename = "type")]
        type_: IqKindSpec,
        id: Option<String>,
        to: Option<String>,
        payload: Option<XmlElementSpec>,
    },
    #[serde(rename = "expectIq")]
    ExpectIq {
        target: Actor,
        id: Option<String>,
        #[serde(rename = "type")]
        type_: Option<IqResponseKind>,
        #[serde(default)]
        contains: Vec<String>,
        #[serde(default)]
        absent: Vec<String>,
        #[serde(default)]
        elements: Vec<XmlElementSpec>,
        #[serde(default, rename = "absentElements")]
        absent_elements: Vec<XmlElementSpec>,
        #[serde(default)]
        captures: Vec<AttributeCapture>,
    },
    #[serde(rename = "sendPresence")]
    SendPresence {
        actor: Actor,
        to: Option<String>,
        #[serde(rename = "type")]
        type_: Option<PresenceKind>,
        show: Option<String>,
        status: Option<String>,
        priority: Option<i8>,
        #[serde(default)]
        payloads: Vec<XmlElementSpec>,
    },
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
        #[serde(default, rename = "bodyAbsent")]
        body_absent: bool,
        from: Option<Actor>,
        #[serde(default, rename = "captureStanzaIdAs")]
        capture_stanza_id_as: Option<String>,
        #[serde(default, rename = "captureStanzaIdBy")]
        capture_stanza_id_by: Option<String>,
        #[serde(default)]
        payloads: Vec<Payload>,
        #[serde(default)]
        contains: Vec<String>,
        #[serde(default)]
        absent: Vec<String>,
        #[serde(default)]
        elements: Vec<XmlElementSpec>,
        #[serde(default, rename = "absentElements")]
        absent_elements: Vec<XmlElementSpec>,
    },
    #[serde(rename = "expectCarbon")]
    ExpectCarbon {
        target: Actor,
        carbon: CarbonKind,
        body: Option<String>,
        #[serde(default, rename = "bodyAbsent")]
        body_absent: bool,
        #[serde(default)]
        payloads: Vec<Payload>,
        #[serde(default)]
        contains: Vec<String>,
        #[serde(default)]
        absent: Vec<String>,
        #[serde(default)]
        elements: Vec<XmlElementSpec>,
        #[serde(default, rename = "absentElements")]
        absent_elements: Vec<XmlElementSpec>,
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
        #[serde(default)]
        contains: Vec<String>,
        #[serde(default)]
        elements: Vec<XmlElementSpec>,
        #[serde(default, rename = "absentElements")]
        absent_elements: Vec<XmlElementSpec>,
        #[serde(default)]
        captures: Vec<AttributeCapture>,
    },
    #[serde(rename = "queryMam")]
    QueryMam {
        actor: Actor,
        archive: String,
        id: Option<String>,
        max: u32,
        after: Option<String>,
        #[serde(rename = "with")]
        with_jid: Option<String>,
        fulltext: Option<String>,
        #[serde(default)]
        ids: Vec<String>,
    },
    #[serde(rename = "expectMamResult")]
    ExpectMamResult {
        body: Option<String>,
        #[serde(default, rename = "bodyAbsent")]
        body_absent: bool,
        #[serde(default)]
        payloads: Vec<Payload>,
        #[serde(default)]
        contains: Vec<String>,
        #[serde(default)]
        absent: Vec<String>,
        #[serde(default)]
        elements: Vec<XmlElementSpec>,
        #[serde(default, rename = "absentElements")]
        absent_elements: Vec<XmlElementSpec>,
    },
    #[serde(rename = "expectNoMamResult")]
    ExpectNoMamResult {
        body: Option<String>,
        #[serde(default, rename = "bodyAbsent")]
        body_absent: bool,
        #[serde(default)]
        payloads: Vec<Payload>,
        #[serde(default)]
        contains: Vec<String>,
        #[serde(default)]
        elements: Vec<XmlElementSpec>,
    },
    #[serde(rename = "expectFrame")]
    ExpectFrame {
        target: Actor,
        #[serde(default)]
        contains: Vec<String>,
        #[serde(default)]
        absent: Vec<String>,
        #[serde(default)]
        elements: Vec<XmlElementSpec>,
        #[serde(default, rename = "absentElements")]
        absent_elements: Vec<XmlElementSpec>,
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
#[serde(rename_all = "camelCase")]
enum StreamManagementAction {
    Enable,
    RequestAck,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "lowercase")]
enum IqKindSpec {
    Get,
    Set,
    Result,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "lowercase")]
enum IqResponseKind {
    Result,
    Error,
    Get,
    Set,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "lowercase")]
enum PresenceKind {
    Available,
    Unavailable,
    Subscribe,
    Subscribed,
    Unsubscribe,
    Unsubscribed,
    Probe,
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
    #[serde(rename = "messageCorrection")]
    MessageCorrection { id: String },
    #[serde(rename = "reactions")]
    Reactions {
        id: Option<String>,
        #[serde(rename = "idFrom")]
        id_from: Option<String>,
        #[serde(default)]
        emojis: Vec<String>,
    },
    #[serde(rename = "processingHint")]
    ProcessingHint { name: ProcessingHint },
    #[serde(rename = "pinAttachment")]
    PinAttachment {
        id: Option<String>,
        #[serde(rename = "idFrom")]
        id_from: Option<String>,
        action: PinAction,
    },
    #[serde(rename = "pinEvent")]
    PinEvent {
        id: Option<String>,
        #[serde(rename = "idFrom")]
        id_from: Option<String>,
        action: PinAction,
    },
    #[serde(rename = "xml")]
    Xml {
        element: XmlElementSpec,
        #[serde(default, rename = "expectElements")]
        expect_elements: Vec<XmlElementSpec>,
    },
}

#[derive(Debug, Deserialize, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
enum PinAction {
    Pinned,
    Unpinned,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "kebab-case")]
enum ProcessingHint {
    NoPermanentStore,
    NoStore,
    NoCopy,
    Store,
}

#[derive(Debug, Clone, Deserialize)]
struct XmlElementSpec {
    name: String,
    ns: String,
    #[serde(default)]
    attrs: BTreeMap<String, String>,
    #[serde(default, rename = "attrsFrom")]
    attrs_from: BTreeMap<String, String>,
    #[serde(default, rename = "attrsPresent")]
    attrs_present: Vec<String>,
    text: Option<String>,
    #[serde(default)]
    children: Vec<XmlElementSpec>,
}

#[derive(Debug, Deserialize)]
struct AttributeCapture {
    #[serde(rename = "as")]
    capture_as: String,
    name: String,
    element: Option<String>,
    ns: Option<String>,
    contains: Option<String>,
}

struct ScenarioContext {
    clients: HashMap<String, WsXmppClient>,
    pending_frames: HashMap<String, VecDeque<String>>,
    last_mam_frames: VecDeque<String>,
    captures: HashMap<String, String>,
    ws_url: String,
    domain: String,
    admin_password: String,
    account_passwords: BTreeMap<String, String>,
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

#[test]
fn cue_scenarios_cover_supported_xeps_manifest() -> Result<()> {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/xmpp_e2e_scenarios");
    let supported = SUPPORTED_XEPS.iter().copied().collect::<BTreeSet<_>>();
    let mut covered = BTreeSet::new();
    let mut unknown = Vec::new();

    for scenario_file in discover_scenario_files(&root)? {
        let scenario = load_scenario_from_file(&root, &scenario_file)
            .with_context(|| format!("load {}", scenario_file.display()))?;
        for xep in &scenario.xeps {
            if supported.contains(xep.as_str()) {
                covered.insert(xep.clone());
            } else {
                unknown.push(format!("{} declares {xep}", scenario.name));
            }
        }
    }

    let missing = SUPPORTED_XEPS
        .iter()
        .filter(|xep| !covered.contains(**xep))
        .copied()
        .collect::<Vec<_>>();

    if missing.is_empty() && unknown.is_empty() {
        return Ok(());
    }

    Err(anyhow!(
        "CUE XEP coverage drift: missing scenario tags for [{}]; unknown tags [{}]",
        missing.join(", "),
        unknown.join(", ")
    ))
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
    let ws_url = server.ws_url();
    let admin_password = server.fixed_account_password().to_string();
    let mut clients = HashMap::new();

    for user in scenario.users.values() {
        for actor in user.devices.values() {
            let password = account_password(&accounts, &admin_password, &actor.username)?;
            let client = WsXmppClient::connect_and_auth(
                &ws_url,
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
        last_mam_frames: VecDeque::new(),
        captures: HashMap::new(),
        ws_url,
        domain: scenario.domain.clone(),
        admin_password,
        account_passwords: accounts,
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

fn account_password<'a>(
    accounts: &'a BTreeMap<String, String>,
    admin_password: &'a str,
    username: &str,
) -> Result<&'a str> {
    accounts
        .get(username)
        .map(String::as_str)
        .or_else(|| (username == "admin").then_some(admin_password))
        .ok_or_else(|| anyhow!("missing password for {username}"))
}

async fn close_clients(clients: HashMap<String, WsXmppClient>) {
    for client in clients.into_values() {
        let _ = client.close().await;
    }
}

async fn disconnect_actor(ctx: &mut ScenarioContext, actor: &Actor) -> Result<()> {
    let key = actor_key(actor);
    ctx.pending_frames.remove(&key);
    if let Some(client) = ctx.clients.remove(&key) {
        client
            .close()
            .await
            .map_err(|error| anyhow!("disconnect {}.{}: {error}", actor.user, actor.device))?;
    }
    Ok(())
}

async fn reconnect_actor(ctx: &mut ScenarioContext, actor: &Actor) -> Result<()> {
    disconnect_actor(ctx, actor).await?;
    let password = account_password(&ctx.account_passwords, &ctx.admin_password, &actor.username)?;
    let client = WsXmppClient::connect_and_auth(
        &ctx.ws_url,
        &ctx.domain,
        &actor.username,
        password,
        &actor.resource,
    )
    .await
    .map_err(|error| anyhow!("reconnect {}.{}: {error}", actor.user, actor.device))?;
    ctx.clients.insert(actor_key(actor), client);
    Ok(())
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
        Step::StreamManagement {
            actor,
            action,
            resume,
            max,
        } => {
            let element = match action {
                StreamManagementAction::Enable => {
                    let mut builder = Element::builder("enable", "urn:xmpp:sm:3");
                    if let Some(resume) = resume {
                        builder = builder.attr("resume", if *resume { "true" } else { "false" });
                    }
                    let max_value = max.map(|value| value.to_string());
                    if let Some(max) = max_value.as_deref() {
                        builder = builder.attr("max", max);
                    }
                    builder.build()
                }
                StreamManagementAction::RequestAck => {
                    Element::builder("r", "urn:xmpp:sm:3").build()
                }
            };
            let xml = element_xml(&element)?;
            client_mut(ctx, actor)?
                .send(&xml)
                .await
                .map_err(|error| anyhow!(error))?;
        }
        Step::DisconnectActor { actor } => {
            disconnect_actor(ctx, actor).await?;
        }
        Step::ConnectActor { actor } => {
            reconnect_actor(ctx, actor).await?;
        }
        Step::SendIq {
            actor,
            type_,
            id,
            to,
            payload,
        } => {
            let id = id
                .clone()
                .unwrap_or_else(|| format!("cue-iq-{}", uuid::Uuid::new_v4()));
            let payload = payload
                .as_ref()
                .map(|payload| xml_element(payload, Some(ctx)))
                .transpose()?;
            let iq_type = match (type_, payload) {
                (IqKindSpec::Get, Some(payload)) => IqType::Get(payload),
                (IqKindSpec::Set, Some(payload)) => IqType::Set(payload),
                (IqKindSpec::Result, payload) => IqType::Result(payload),
                (IqKindSpec::Get | IqKindSpec::Set, None) => {
                    return Err(anyhow!("sendIq get/set requires a payload"))
                }
            };
            let iq = Iq {
                from: None,
                to: to.as_deref().map(str::parse).transpose()?,
                id,
                payload: iq_type,
            };
            client_mut(ctx, actor)?
                .send(&stanza_xml(Stanza::Iq(iq))?)
                .await
                .map_err(|error| anyhow!(error))?;
        }
        Step::ExpectIq {
            target,
            id,
            type_,
            contains,
            absent,
            elements,
            absent_elements,
            captures,
        } => {
            let mut expected = contains.clone();
            if let Some(id) = id {
                expected.push(format!("id=\"{id}\""));
            }
            if let Some(type_) = type_ {
                expected.push(format!("type=\"{}\"", iq_response_kind_name(type_)));
            }
            let captures_snapshot = ctx.captures.clone();
            let frame = recv_matching(ctx, target, |frame| {
                frame.contains("<iq")
                    && id
                        .as_ref()
                        .is_none_or(|id| frame.contains(&format!("id=\"{id}\"")))
                    && type_.as_ref().is_none_or(|type_| {
                        frame.contains(&format!("type=\"{}\"", iq_response_kind_name(type_)))
                    })
                    && contains.iter().all(|part| frame.contains(part))
                    && elements
                        .iter()
                        .all(|spec| frame_has_element(frame, spec, &captures_snapshot))
            })
            .await?;
            assert_contains_all(&frame, &expected, "IQ expectation")?;
            assert_absent_all(&frame, absent, "IQ expectation")?;
            assert_elements_present(&frame, elements, &captures_snapshot, "IQ expectation")?;
            assert_elements_absent(
                &frame,
                absent_elements,
                &captures_snapshot,
                "IQ expectation",
            )?;
            for capture in captures {
                let value = extract_attr_capture(&frame, capture)?;
                ctx.captures.insert(capture.capture_as.clone(), value);
            }
        }
        Step::SendPresence {
            actor,
            to,
            type_,
            show,
            status,
            priority,
            payloads,
        } => {
            let mut presence = Presence::new(match type_ {
                None | Some(PresenceKind::Available) => PresenceType::None,
                Some(PresenceKind::Unavailable) => PresenceType::Unavailable,
                Some(PresenceKind::Subscribe) => PresenceType::Subscribe,
                Some(PresenceKind::Subscribed) => PresenceType::Subscribed,
                Some(PresenceKind::Unsubscribe) => PresenceType::Unsubscribe,
                Some(PresenceKind::Unsubscribed) => PresenceType::Unsubscribed,
                Some(PresenceKind::Probe) => PresenceType::Probe,
            });
            presence.to = to.as_deref().map(str::parse).transpose()?;
            if let Some(show) = show {
                presence.show = Some(show.parse::<PresenceShow>()?);
            }
            if let Some(status) = status {
                presence.statuses.insert(String::new(), status.clone());
            }
            if let Some(priority) = priority {
                presence.priority = *priority;
            }
            for payload in payloads {
                presence.payloads.push(xml_element(payload, Some(ctx))?);
            }
            client_mut(ctx, actor)?
                .send(&stanza_xml(Stanza::Presence(presence))?)
                .await
                .map_err(|error| anyhow!(error))?;
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
                message.payloads.push(payload_element(payload, ctx)?);
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
            body_absent,
            from,
            capture_stanza_id_as,
            capture_stanza_id_by,
            payloads,
            contains,
            absent,
            elements,
            absent_elements,
        } => {
            let mut expected = contains.clone();
            if let Some(body) = body {
                expected.push(body_text_marker(body));
            }
            let payload_expectations = payload_expectations(payloads, ctx)?;
            let payload_element_expectations = payload_element_expectations(payloads);
            expected.extend(payload_expectations.clone());
            if let Some(from) = from {
                expected.push(format!("from=\"{}", from.bare_jid));
            }
            let captures_snapshot = ctx.captures.clone();
            let frame = recv_matching(ctx, target, |frame| {
                frame.contains("<message")
                    && body
                        .as_ref()
                        .is_none_or(|body| frame_contains_body(frame, body))
                    && (!*body_absent || !frame_has_direct_message_body(frame))
                    && from
                        .as_ref()
                        .is_none_or(|from| frame.contains(&format!("from=\"{}", from.bare_jid)))
                    && payload_expectations.iter().all(|part| frame.contains(part))
                    && payload_element_expectations
                        .iter()
                        .all(|spec| frame_has_element(frame, spec, &captures_snapshot))
                    && contains.iter().all(|part| frame.contains(part))
                    && elements
                        .iter()
                        .all(|spec| frame_has_element(frame, spec, &captures_snapshot))
            })
            .await?;
            if *body_absent && frame_has_direct_message_body(&frame) {
                return Err(anyhow!(
                    "message expectation expected no <body> element, got: {frame}"
                ));
            }
            assert_contains_all(&frame, &expected, "message expectation")?;
            assert_absent_all(&frame, absent, "message expectation")?;
            assert_elements_present(
                &frame,
                &payload_element_expectations,
                &captures_snapshot,
                "message payload expectation",
            )?;
            assert_elements_present(&frame, elements, &captures_snapshot, "message expectation")?;
            assert_elements_absent(
                &frame,
                absent_elements,
                &captures_snapshot,
                "message expectation",
            )?;
            if let Some(capture_name) = capture_stanza_id_as {
                let stanza_id =
                    extract_stanza_id_from_frame(&frame, capture_stanza_id_by.as_deref())?;
                ctx.captures.insert(capture_name.clone(), stanza_id);
            }
        }
        Step::ExpectCarbon {
            target,
            carbon,
            body,
            body_absent,
            payloads,
            contains,
            absent,
            elements,
            absent_elements,
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
            let payload_expectations = payload_expectations(payloads, ctx)?;
            let payload_element_expectations = payload_element_expectations(payloads);
            expected.extend(payload_expectations.clone());
            let captures_snapshot = ctx.captures.clone();
            let frame = recv_matching(ctx, target, |frame| {
                frame.contains("urn:xmpp:carbons:2")
                    && frame.contains(carbon_tag)
                    && body
                        .as_ref()
                        .is_none_or(|body| frame_contains_body(frame, body))
                    && (!*body_absent || !frame_has_direct_message_body(frame))
                    && payload_expectations.iter().all(|part| frame.contains(part))
                    && payload_element_expectations
                        .iter()
                        .all(|spec| frame_has_element(frame, spec, &captures_snapshot))
                    && contains.iter().all(|part| frame.contains(part))
                    && elements
                        .iter()
                        .all(|spec| frame_has_element(frame, spec, &captures_snapshot))
            })
            .await?;
            if *body_absent && frame_has_direct_message_body(&frame) {
                return Err(anyhow!(
                    "carbon expectation expected no <body> element, got: {frame}"
                ));
            }
            assert_contains_all(&frame, &expected, "carbon expectation")?;
            assert_absent_all(&frame, absent, "carbon expectation")?;
            assert_elements_present(
                &frame,
                &payload_element_expectations,
                &captures_snapshot,
                "carbon payload expectation",
            )?;
            assert_elements_present(&frame, elements, &captures_snapshot, "carbon expectation")?;
            assert_elements_absent(
                &frame,
                absent_elements,
                &captures_snapshot,
                "carbon expectation",
            )?;
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
        Step::ExpectPresence {
            target,
            contains,
            elements,
            absent_elements,
            captures,
        } => {
            let captures_snapshot = ctx.captures.clone();
            let frame = recv_matching(ctx, target, |frame| {
                frame.contains("<presence")
                    && contains.iter().all(|part| frame.contains(part))
                    && elements
                        .iter()
                        .all(|spec| frame_has_element(frame, spec, &captures_snapshot))
            })
            .await?;
            assert_contains_all(&frame, contains, "presence expectation")?;
            assert_elements_present(&frame, elements, &captures_snapshot, "presence expectation")?;
            assert_elements_absent(
                &frame,
                absent_elements,
                &captures_snapshot,
                "presence expectation",
            )?;
            for capture in captures {
                let value = extract_attr_capture(&frame, capture)?;
                ctx.captures.insert(capture.capture_as.clone(), value);
            }
        }
        Step::QueryMam {
            actor,
            archive,
            id,
            max,
            after,
            with_jid,
            fulltext,
            ids,
        } => {
            let id = id
                .clone()
                .unwrap_or_else(|| format!("cue-mam-{}", uuid::Uuid::new_v4()));
            let query = mam_query_element(
                *max,
                after.as_deref(),
                with_jid.as_deref(),
                fulltext.as_deref(),
                ids,
            );
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
            .await?
            .into();
        }
        Step::ExpectMamResult {
            body,
            body_absent,
            payloads,
            contains,
            absent,
            elements,
            absent_elements,
        } => {
            let payload_expectations = payload_expectations(payloads, ctx)?;
            let payload_element_expectations = payload_element_expectations(payloads);
            let captures_snapshot = ctx.captures.clone();
            let mut skipped = Vec::new();
            let frame = loop {
                let Some(frame) = ctx.last_mam_frames.pop_front() else {
                    return Err(anyhow!(
                        "no MAM result matched body {:?} and contains {:?}; skipped frames: {:?}; remaining frames: {:?}",
                        body,
                        contains,
                        skipped,
                        ctx.last_mam_frames
                    ));
                };
                if frame.contains("<forwarded")
                    && body
                        .as_ref()
                        .is_none_or(|body| frame_contains_body(&frame, body))
                    && (!*body_absent || !frame_has_direct_message_body(&frame))
                    && payload_expectations.iter().all(|part| frame.contains(part))
                    && payload_element_expectations
                        .iter()
                        .all(|spec| frame_has_element(&frame, spec, &captures_snapshot))
                    && contains.iter().all(|part| frame.contains(part))
                    && elements
                        .iter()
                        .all(|spec| frame_has_element(&frame, spec, &captures_snapshot))
                {
                    break frame;
                }
                skipped.push(frame);
            };
            if let Some(body) = body {
                assert_contains_all(&frame, std::slice::from_ref(body), "MAM result body")?;
            }
            if *body_absent && frame_has_direct_message_body(&frame) {
                return Err(anyhow!(
                    "MAM result expected no <body> element, got: {frame}"
                ));
            }
            assert_contains_all(&frame, &payload_expectations, "MAM result payloads")?;
            assert_elements_present(
                &frame,
                &payload_element_expectations,
                &captures_snapshot,
                "MAM result payloads",
            )?;
            assert_contains_all(&frame, contains, "MAM result contains")?;
            assert_absent_all(&frame, absent, "MAM result absent")?;
            assert_elements_present(&frame, elements, &captures_snapshot, "MAM result elements")?;
            assert_elements_absent(
                &frame,
                absent_elements,
                &captures_snapshot,
                "MAM result elements",
            )?;
        }
        Step::ExpectNoMamResult {
            body,
            body_absent,
            payloads,
            contains,
            elements,
        } => {
            let payload_expectations = payload_expectations(payloads, ctx)?;
            let payload_element_expectations = payload_element_expectations(payloads);
            let captures_snapshot = ctx.captures.clone();
            let matched = ctx.last_mam_frames.iter().find(|frame| {
                frame.contains("<forwarded")
                    && body
                        .as_ref()
                        .is_none_or(|body| frame_contains_body(frame, body))
                    && (!*body_absent || !frame_has_direct_message_body(frame))
                    && payload_expectations.iter().all(|part| frame.contains(part))
                    && payload_element_expectations
                        .iter()
                        .all(|spec| frame_has_element(frame, spec, &captures_snapshot))
                    && contains.iter().all(|part| frame.contains(part))
                    && elements
                        .iter()
                        .all(|spec| frame_has_element(frame, spec, &captures_snapshot))
            });
            if let Some(frame) = matched {
                return Err(anyhow!(
                    "unexpected MAM result matched body {:?} and contains {:?}: {frame}",
                    body,
                    contains
                ));
            }
        }
        Step::ExpectFrame {
            target,
            contains,
            absent,
            elements,
            absent_elements,
        } => {
            let captures_snapshot = ctx.captures.clone();
            let frame = recv_matching(ctx, target, |frame| {
                contains.iter().all(|part| frame.contains(part))
                    && elements
                        .iter()
                        .all(|spec| frame_has_element(frame, spec, &captures_snapshot))
            })
            .await?;
            assert_contains_all(&frame, contains, "frame expectation")?;
            assert_absent_all(&frame, absent, "frame expectation")?;
            assert_elements_present(&frame, elements, &captures_snapshot, "frame expectation")?;
            assert_elements_absent(
                &frame,
                absent_elements,
                &captures_snapshot,
                "frame expectation",
            )?;
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
        let Some(frame) = recv_timeout(ctx, actor, RECV_TIMEOUT).await? else {
            return Err(anyhow!(
                "Timeout waiting for matching frame; skipped frames: {:?}",
                non_matching_frames
            ));
        };
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

fn iq_response_kind_name(kind: &IqResponseKind) -> &'static str {
    match kind {
        IqResponseKind::Result => "result",
        IqResponseKind::Error => "error",
        IqResponseKind::Get => "get",
        IqResponseKind::Set => "set",
    }
}

enum IqKind {
    Get,
    Set,
}

fn mam_query_element(
    max: u32,
    after: Option<&str>,
    with_jid: Option<&str>,
    fulltext: Option<&str>,
    ids: &[String],
) -> Element {
    const MAM_NS: &str = "urn:xmpp:mam:2";
    const RSM_NS: &str = "http://jabber.org/protocol/rsm";
    const DATA_FORMS_NS: &str = "jabber:x:data";
    const FULLTEXT_MAM_FIELD: &str = "{urn:xmpp:fulltext:0}fulltext";

    let mut rsm = Element::builder("set", RSM_NS).append(
        Element::builder("max", RSM_NS)
            .append(max.to_string())
            .build(),
    );
    if let Some(after) = after {
        rsm = rsm.append(Element::builder("after", RSM_NS).append(after).build());
    }

    let has_form = with_jid.is_some() || fulltext.is_some() || !ids.is_empty();
    let mut query = Element::builder("query", MAM_NS).append(rsm.build());
    if has_form {
        let mut form = Element::builder("x", DATA_FORMS_NS)
            .attr("type", "submit")
            .append(data_form_field("FORM_TYPE", &[MAM_NS]));
        if let Some(with_jid) = with_jid {
            form = form.append(data_form_field("with", &[with_jid]));
        }
        if let Some(fulltext) = fulltext {
            form = form.append(data_form_field(FULLTEXT_MAM_FIELD, &[fulltext]));
        }
        if !ids.is_empty() {
            let values = ids.iter().map(String::as_str).collect::<Vec<_>>();
            form = form.append(data_form_field("ids", &values));
        }
        query = query.append(form.build());
    }
    query.build()
}

fn data_form_field(var: &str, values: &[&str]) -> Element {
    let mut field = Element::builder("field", "jabber:x:data").attr("var", var);
    for value in values {
        field = field.append(
            Element::builder("value", "jabber:x:data")
                .append(*value)
                .build(),
        );
    }
    field.build()
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

fn xml_element(spec: &XmlElementSpec, ctx: Option<&ScenarioContext>) -> Result<Element> {
    let mut builder = Element::builder(spec.name.as_str(), spec.ns.as_str());
    for (name, value) in &spec.attrs {
        builder = builder.attr(name.as_str(), value.as_str());
    }
    for (name, capture) in &spec.attrs_from {
        let value = ctx
            .and_then(|ctx| ctx.captures.get(capture))
            .ok_or_else(|| anyhow!("unknown captured attribute value {capture:?}"))?;
        builder = builder.attr(name.as_str(), value.as_str());
    }
    if let Some(text) = &spec.text {
        builder = builder.append(text.as_str());
    }
    for child in &spec.children {
        builder = builder.append(xml_element(child, ctx)?);
    }
    Ok(builder.build())
}

fn element_xml(element: &Element) -> Result<String> {
    let mut buf = Vec::new();
    element.write_to(&mut buf)?;
    Ok(String::from_utf8(buf)?)
}

fn payload_element(payload: &Payload, ctx: &ScenarioContext) -> Result<Element> {
    match payload {
        Payload::FileShare {
            disposition,
            name,
            media_type,
            size,
            url,
        } => Ok(Element::builder("file-sharing", "urn:xmpp:sfs:0")
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
            .build()),
        Payload::LinkMetadata {
            about,
            title,
            description,
            url,
        } => Ok(
            Element::builder("Description", "http://www.w3.org/1999/02/22-rdf-syntax-ns#")
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
        ),
        Payload::MessageCorrection { id } => {
            Ok(Element::builder("replace", "urn:xmpp:message-correct:0")
                .attr("id", id.as_str())
                .build())
        }
        Payload::Reactions {
            id,
            id_from,
            emojis,
        } => {
            let target_id = resolve_payload_id(ctx, id.as_deref(), id_from.as_deref())?;
            let emoji_refs = emojis.iter().map(String::as_str).collect::<Vec<_>>();
            Ok(xep0444::build_reactions_element(&target_id, &emoji_refs))
        }
        Payload::ProcessingHint { name } => Ok(xep0334::build_hint_element(Hint::from(name))),
        Payload::PinAttachment {
            id,
            id_from,
            action,
        } => {
            let target_id = resolve_payload_id(ctx, id.as_deref(), id_from.as_deref())?;
            let stanza_id = waddle_xmpp_core::xep0359::StanzaId::new(
                target_id,
                jid::Jid::from(
                    jid::BareJid::from_str("room@example.com")
                        .map_err(|e| anyhow!("invalid placeholder jid: {e}"))?,
                ),
            );
            let elem = match action {
                PinAction::Pinned => waddle_xmpp::xep::build_pinned_message_element(&stanza_id),
                PinAction::Unpinned => waddle_xmpp::xep::build_unpinned_message_element(&stanza_id),
            };
            Ok(elem)
        }
        Payload::PinEvent { .. } => Err(anyhow!(
            "PinEvent is an expected-only payload; cannot be sent"
        )),
        Payload::Xml { element, .. } => xml_element(element, Some(ctx)),
    }
}

fn payload_expectations(payloads: &[Payload], ctx: &ScenarioContext) -> Result<Vec<String>> {
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
            Payload::MessageCorrection { id } => {
                expected.extend(["urn:xmpp:message-correct:0".to_string(), id.clone()]);
            }
            Payload::Reactions {
                id,
                id_from,
                emojis,
            } => {
                let target_id = resolve_payload_id(ctx, id.as_deref(), id_from.as_deref())?;
                expected.extend([
                    "urn:xmpp:reactions:0".to_string(),
                    format!("id=\"{target_id}\""),
                ]);
                expected.extend(normalized_reaction_text_markers(&target_id, emojis));
            }
            Payload::ProcessingHint { name } => {
                let hint = Hint::from(name);
                expected.extend([
                    "urn:xmpp:hints".to_string(),
                    format!("<{}", hint.element_name()),
                ]);
            }
            Payload::PinAttachment {
                id,
                id_from,
                action,
            } => {
                let target_id = resolve_payload_id(ctx, id.as_deref(), id_from.as_deref())?;
                let marker = match action {
                    PinAction::Pinned => "<pinned",
                    PinAction::Unpinned => "<unpinned",
                };
                expected.extend([
                    "urn:waddle:pin:0".to_string(),
                    marker.to_string(),
                    format!("target=\"{target_id}\""),
                ]);
            }
            Payload::PinEvent {
                id,
                id_from,
                action,
            } => {
                let target_id = resolve_payload_id(ctx, id.as_deref(), id_from.as_deref())?;
                let action_attr = match action {
                    PinAction::Pinned => "action=\"pinned\"",
                    PinAction::Unpinned => "action=\"unpinned\"",
                };
                expected.extend([
                    "urn:waddle:pin:0".to_string(),
                    "<pin-event".to_string(),
                    action_attr.to_string(),
                    format!("target=\"{target_id}\""),
                ]);
            }
            Payload::Xml { .. } => {}
        }
    }
    Ok(expected)
}

fn payload_element_expectations(payloads: &[Payload]) -> Vec<XmlElementSpec> {
    let mut expected = Vec::new();
    for payload in payloads {
        if let Payload::Xml {
            element,
            expect_elements,
        } = payload
        {
            expected.push(element.clone());
            expected.extend(expect_elements.clone());
        }
    }
    expected
}

fn resolve_payload_id(
    ctx: &ScenarioContext,
    id: Option<&str>,
    id_from: Option<&str>,
) -> Result<String> {
    match (id, id_from) {
        (Some(id), None) => Ok(id.to_string()),
        (None, Some(capture)) => ctx
            .captures
            .get(capture)
            .cloned()
            .ok_or_else(|| anyhow!("unknown captured id {capture:?}")),
        (Some(_), Some(_)) => Err(anyhow!("payload must specify id or idFrom, not both")),
        (None, None) => Err(anyhow!("payload requires id or idFrom")),
    }
}

impl From<&ProcessingHint> for Hint {
    fn from(value: &ProcessingHint) -> Self {
        match value {
            ProcessingHint::NoPermanentStore => Self::NoPermanentStore,
            ProcessingHint::NoStore => Self::NoStore,
            ProcessingHint::NoCopy => Self::NoCopy,
            ProcessingHint::Store => Self::Store,
        }
    }
}

fn validate_file_share_fallback_body(body: Option<&str>, payloads: &[Payload]) -> Result<()> {
    let Some(body) = body else {
        return Ok(());
    };
    let represented_by_payload = payloads.iter().any(|payload| match payload {
        Payload::FileShare { url, .. } => body == url,
        Payload::LinkMetadata { .. }
        | Payload::MessageCorrection { .. }
        | Payload::Reactions { .. }
        | Payload::ProcessingHint { .. }
        | Payload::PinAttachment { .. }
        | Payload::PinEvent { .. }
        | Payload::Xml { .. } => false,
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
    parse_frame(frame)
        .is_some_and(|element| element_contains_direct_message_body(&element, Some(body)))
}

fn frame_has_direct_message_body(frame: &str) -> bool {
    parse_frame(frame).is_some_and(|element| element_contains_direct_message_body(&element, None))
}

fn parse_frame(frame: &str) -> Option<Element> {
    Element::from_str(frame).ok()
}

fn frame_has_element(
    frame: &str,
    spec: &XmlElementSpec,
    captures: &HashMap<String, String>,
) -> bool {
    parse_frame(frame).is_some_and(|element| find_matching_element(&element, spec, captures))
}

fn find_matching_element(
    element: &Element,
    spec: &XmlElementSpec,
    captures: &HashMap<String, String>,
) -> bool {
    element_matches_spec(element, spec, captures)
        || element
            .children()
            .any(|child| find_matching_element(child, spec, captures))
}

fn element_matches_spec(
    element: &Element,
    spec: &XmlElementSpec,
    captures: &HashMap<String, String>,
) -> bool {
    if element.name() != spec.name.as_str() || element.ns() != spec.ns.as_str() {
        return false;
    }
    for (name, value) in &spec.attrs {
        if element.attr(name.as_str()) != Some(value.as_str()) {
            return false;
        }
    }
    for (name, capture) in &spec.attrs_from {
        let Some(value) = captures.get(capture) else {
            return false;
        };
        if element.attr(name.as_str()) != Some(value.as_str()) {
            return false;
        }
    }
    for name in &spec.attrs_present {
        if element.attr(name.as_str()).is_none() {
            return false;
        }
    }
    if spec
        .text
        .as_deref()
        .is_some_and(|text| element.text() != text)
    {
        return false;
    }
    spec.children.iter().all(|spec_child| {
        element
            .children()
            .any(|child| element_matches_spec(child, spec_child, captures))
    })
}

fn assert_elements_present(
    frame: &str,
    specs: &[XmlElementSpec],
    captures: &HashMap<String, String>,
    context: &str,
) -> Result<()> {
    for spec in specs {
        if !frame_has_element(frame, spec, captures) {
            return Err(anyhow!("{context} expected element {spec:?}, got: {frame}"));
        }
    }
    Ok(())
}

fn assert_elements_absent(
    frame: &str,
    specs: &[XmlElementSpec],
    captures: &HashMap<String, String>,
    context: &str,
) -> Result<()> {
    for spec in specs {
        if frame_has_element(frame, spec, captures) {
            return Err(anyhow!(
                "{context} expected element {spec:?} to be absent, got: {frame}"
            ));
        }
    }
    Ok(())
}

fn element_contains_direct_message_body(element: &Element, expected: Option<&str>) -> bool {
    let this_element_matches = element.name() == "message"
        && element.children().any(|child| {
            child.name() == "body"
                && child.ns() == element.ns()
                && expected.is_none_or(|body| child.text() == body)
        });

    this_element_matches
        || element
            .children()
            .any(|child| element_contains_direct_message_body(child, expected))
}

fn normalized_reaction_text_markers(target_id: &str, emojis: &[String]) -> Vec<String> {
    let emoji_refs = emojis.iter().map(String::as_str).collect::<Vec<_>>();
    xep0444::build_reactions_element(target_id, &emoji_refs)
        .children()
        .filter(|child| child.name() == "reaction" && child.ns() == xep0444::NS_REACTIONS)
        .map(|child| text_node_marker(&child.text()))
        .collect()
}

fn body_text_marker(body: &str) -> String {
    format!(">{body}</body>")
}

fn text_node_marker(value: &str) -> String {
    format!(">{value}</")
}

fn extract_stanza_id_from_frame(frame: &str, by: Option<&str>) -> Result<String> {
    let element =
        Element::from_str(frame).with_context(|| format!("parse message frame: {frame}"))?;
    find_stanza_id(&element, by)
        .ok_or_else(|| anyhow!("no stanza-id matched by {:?} in frame: {frame}", by))
}

fn find_stanza_id(element: &Element, by: Option<&str>) -> Option<String> {
    if element.name() == "stanza-id" && element.ns() == "urn:xmpp:sid:0" {
        let by_matches = by.is_none_or(|expected| element.attr("by") == Some(expected));
        if by_matches {
            if let Some(id) = element.attr("id").filter(|id| !id.is_empty()) {
                return Some(id.to_string());
            }
        }
    }
    element
        .children()
        .find_map(|child| find_stanza_id(child, by))
}

fn extract_attr_capture(frame: &str, capture: &AttributeCapture) -> Result<String> {
    let element = Element::from_str(frame).with_context(|| format!("parse frame: {frame}"))?;
    find_capture_element(&element, capture)
        .and_then(|element| element.attr(capture.name.as_str()))
        .map(str::to_string)
        .ok_or_else(|| {
            anyhow!(
                "no attribute {:?} matched capture {:?}/{:?} in frame: {frame}",
                capture.name,
                capture.element,
                capture.ns
            )
        })
}

fn find_capture_element<'a>(
    element: &'a Element,
    capture: &AttributeCapture,
) -> Option<&'a Element> {
    let name_matches = capture
        .element
        .as_deref()
        .is_none_or(|name| element.name() == name);
    let ns_matches = capture.ns.as_deref().is_none_or(|ns| element.ns() == ns);
    let contains_matches = capture
        .contains
        .as_deref()
        .is_none_or(|needle| element_xml(element).is_ok_and(|xml| xml.contains(needle)));
    if name_matches && ns_matches && contains_matches {
        return Some(element);
    }
    element
        .children()
        .find_map(|child| find_capture_element(child, capture))
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

fn assert_absent_all<I, S>(frame: &str, absent: I, context: &str) -> Result<()>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    for part in absent {
        let part = part.as_ref();
        if frame.contains(part) {
            return Err(anyhow!(
                "{context} expected {part:?} to be absent, got: {frame}"
            ));
        }
    }
    Ok(())
}
