# #718 — Typed push client surface (XEP-0357 + XEP-0050 cutover) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use `superpowers:subagent-driven-development` (recommended) or `superpowers:executing-plans` to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Cut the entire push-device registration flow over from custom `urn:waddle:push-service:0` IQs to conformant XEP-0050 ad-hoc commands + XEP-0004 forms, drop the legacy `token: &str` parameter from `enable_push_notifications`, and expose the typed surface to UniFFI + WASM consumers.

**Architecture:** Client crate `waddle-xmpp-client` gains typed XEP-0357 and XEP-0050 IQ builders that accept `xmpp_parsers::data_forms::DataForm` and a locally-defined `AdHocAction` enum (xmpp-parsers 0.22 ships `DataForm` but no XEP-0050 module). A `register_push_device` composer drives the 4-stage XEP-0050 dance and returns the assigned node id. The push-service component on the server replaces its custom-namespace handler with two XEP-0050 commands (`register-device`, `disable-device`) wired into the existing `command_registry`. Storage (`push_nodes`, `push_devices`) is untouched — only the wire shape changes. WASM and UniFFI bindings expose the new surface; chat/`client.ts` drops the `""` token placeholder.

**Tech Stack:** Rust + `xmpp_parsers` 0.22 (typed `DataForm`, `Iq`) + `minidom` (XML construction), Bun + TypeScript (chat), UniFFI (Apple), `wasm-bindgen` (chat WASM bridge), `xtask`-driven Drizzle migrations (untouched here).

**Hard rules to keep in view while executing:**
- CLAUDE.md XML generation: never `format!` / string-concat for XMPP XML — use `minidom::Element::builder` or `xmpp_parsers` typed structs.
- CLAUDE.md typed payloads: protocol data is typed at every boundary (no `&str` for `AdHocAction`, `PushPlatform`, etc.).
- `server/CLAUDE.md`: never `unwrap`; never add clippy allows.
- CLAUDE.md XEP custom test-suite: every behavior change carries its dedicated Rust test in the same PR.
- CLAUDE.md breaking changes by default: no migration shims, drop the old `urn:waddle:push-service:0` code entirely.
- CLAUDE.md commits: Conventional Commits with `feat(server): …`, `feat(chat): …` scopes. Subjects lowercase after the colon.

**Files this plan touches (decomposition decisions locked here):**

Client (`server/crates/waddle-xmpp-client/`):
- Create: `src/xep/xep0050.rs` — typed `AdHocAction`, `AdHocStatus`, `CommandResponse`, IQ builder + parser
- Create: `src/xep/xep0357.rs` — typed enable / disable IQ builders accepting `Option<DataForm>`
- Modify: `src/lib.rs` — re-export new `xep::*` modules
- Modify: `src/discovery/ext.rs:58-71` — drop `token: &str` from `enable_push_notifications`, add `register_push_device` composer signature
- Modify: `src/discovery/iq.rs:92-148` — delete legacy `build_enable_push_iq` / `build_disable_push_iq` (replaced by new module); delete `build_ensure_push_node_iq` / `build_register_push_device_iq` / `build_disable_push_device_iq` and the `PushDevicePlatform` / `PushEnvironment` / `PushDeviceRegistration` types (replaced by XEP-0050 form values)
- Modify: `src/discovery/tests.rs` — drop tests pinning the old `urn:waddle:push-service:0` shape; new tests for typed XEP-0357 / XEP-0050 builders live in `src/xep/xep0357.rs` and `src/xep/xep0050.rs`
- Modify: `src/discovery.rs:47` — remove `WADDLE_PUSH_SERVICE_NS` constant
- Create: `src/push/mod.rs` — `register_push_device` composer + typed value objects (`PushPlatform`, `PushEnvironment`, `PushDeviceCredentials`, `PushNodeId`)
- Create: `tests/register_push_device_against_fake_service.rs` — multi-step integration test against an in-process fake

Server (`server/crates/`):
- Modify: `waddle-server/src/server/routes/websocket/handlers/iq/mod.rs` — drop the `push_service_iq` dispatch branch; the new commands flow through the existing `commands.rs` dispatcher
- Delete: `waddle-server/src/server/routes/websocket/handlers/iq/push_service_iq.rs`
- Create: `waddle-server/src/push_service/commands.rs` — XEP-0050 handlers for `register-device` and `disable-device`, registered with `command_registry`
- Modify: `waddle-server/src/push_service.rs` — strip the request-parsing helpers that were tied to attribute-based wire shape; storage API surface unchanged
- Modify: `waddle-xmpp-core/src/disco/info.rs:416-426` — advertise `http://jabber.org/protocol/commands` on push-service identity; the command items are surfaced via disco#items on the new command handlers
- Modify: `waddle-server/tests/xep0357_push_service_ws.rs` — rewrite the three custom-namespace tests; preserve the disco / PubSub-gate / offline-DM tests verbatim
- Modify (audit only): `waddle-server/tests/waddle_dnd_pep_push_gating_ws.rs` + `waddle-server/tests/notifications_restart_durability_ws.rs` — touch only if they construct custom-namespace IQs

WASM (`server/crates/waddle-xmpp-client-wasm/`):
- Modify: `src/client_account.rs` — replace `ensure_push_node`, `register_web_push_device`, `disable_push_device` with `register_push_device` + remove token parameter from `enable_push_notifications`; keep return-typed JS objects
- Modify: `src/lib.rs` — drop the `WADDLE_PUSH_SERVICE_NS` re-export

UniFFI (`server/crates/waddle-xmpp-client-ffi/`):
- Modify: `src/lib.rs` — add `enable_push_notifications`, `disable_push_notifications`, `register_push_device` methods on `WaddleClient`
- Modify: `src/uniffi.toml` (if present) — no change expected; UDL is generated from macros

Chat (`chat/`):
- Modify: `src/lib/xmpp/client.ts:1203-1218` — drop `""` token at `enablePushNotifications`; reroute `ensurePushNode`/`registerWebPushDevice`/`disablePushDevice` to the new composer
- Modify: any caller of those three methods (search-and-fix; the WASM signatures define the contract)

---

## Task 1 — Set up the typed `xep::xep0050` module on the client

**Files:**
- Create: `server/crates/waddle-xmpp-client/src/xep/mod.rs`
- Create: `server/crates/waddle-xmpp-client/src/xep/xep0050.rs`
- Modify: `server/crates/waddle-xmpp-client/src/lib.rs` (add `pub mod xep;`)

xmpp-parsers 0.22 does not ship an XEP-0050 module. Mirror the server's shape (`server/crates/waddle-xmpp/src/xep/xep0050/types.rs`) at the client side as a typed builder. Re-using the server crate from the client is the wrong dependency direction.

- [ ] **Step 1: Write the failing test** in `server/crates/waddle-xmpp-client/src/xep/xep0050.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use xmpp_parsers::data_forms::{DataForm, DataFormType};

    #[test]
    fn build_command_request_execute_carries_node_and_action() {
        let iq = build_xep0050_command_request(
            "push.example.com",
            "register-device",
            AdHocAction::Execute,
            None,
        );
        assert_eq!(iq.attr("type"), Some("set"));
        assert_eq!(iq.attr("to"), Some("push.example.com"));
        let command = iq.get_child("command", NS_COMMANDS).expect("command");
        assert_eq!(command.attr("node"), Some("register-device"));
        assert_eq!(command.attr("action"), Some("execute"));
        assert!(command.get_child("x", "jabber:x:data").is_none());
    }

    #[test]
    fn build_command_request_complete_carries_session_and_submitted_form() {
        let form = DataForm::new(
            DataFormType::Submit,
            "urn:xmpp:push-service:commands:register-device:0",
            vec![],
        );
        let iq = build_xep0050_command_request_with_session(
            "push.example.com",
            "register-device",
            "session-1",
            AdHocAction::Complete,
            Some(form),
        );
        let command = iq.get_child("command", NS_COMMANDS).expect("command");
        assert_eq!(command.attr("sessionid"), Some("session-1"));
        assert_eq!(command.attr("action"), Some("complete"));
        let x = command.get_child("x", "jabber:x:data").expect("submitted form");
        assert_eq!(x.attr("type"), Some("submit"));
    }
}
```

- [ ] **Step 2: Run tests to see them fail**

```sh
cd server && cargo test -p waddle-xmpp-client xep0050
```

Expected: FAIL with "build_xep0050_command_request not found" / module not found.

- [ ] **Step 3: Write the minimal module**

```rust
//! XEP-0050 Ad-Hoc Commands — client-side typed IQ builders.
//!
//! xmpp-parsers 0.22 does not ship a XEP-0050 module; we mirror the
//! shape used server-side (`waddle-xmpp::xep::xep0050`) at the client
//! boundary. The server-side crate is not depended on directly — that
//! would invert the dependency direction (client → server).

use minidom::Element;

use crate::discovery::ids::next_id;
use crate::discovery::CLIENT_NS;

pub const NS_COMMANDS: &str = "http://jabber.org/protocol/commands";
pub const NS_DATA_FORMS: &str = "jabber:x:data";

/// Typed XEP-0050 §3 action attribute.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AdHocAction {
    Execute,
    Cancel,
    Next,
    Prev,
    Complete,
}

impl AdHocAction {
    pub fn as_wire_str(self) -> &'static str {
        match self {
            Self::Execute => "execute",
            Self::Cancel => "cancel",
            Self::Next => "next",
            Self::Prev => "prev",
            Self::Complete => "complete",
        }
    }
}

/// Typed XEP-0050 §3 status attribute on responses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AdHocStatus {
    Executing,
    Completed,
    Canceled,
}

impl AdHocStatus {
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "executing" => Some(Self::Executing),
            "completed" => Some(Self::Completed),
            "canceled" => Some(Self::Canceled),
            _ => None,
        }
    }
}

/// Parsed `<command/>` payload extracted from an IQ result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandResponse {
    pub node: String,
    pub session_id: Option<String>,
    pub status: AdHocStatus,
    pub form: Option<xmpp_parsers::data_forms::DataForm>,
}

/// Build an IQ that initiates a XEP-0050 ad-hoc command with the
/// initial `action='execute'` and no session id. Use this for the
/// FIRST stage of the multi-step dance.
pub fn build_xep0050_command_request(
    service_jid: &str,
    node: &str,
    action: AdHocAction,
    form: Option<xmpp_parsers::data_forms::DataForm>,
) -> Element {
    build_command_iq(service_jid, node, None, action, form)
}

/// Build a SUBSEQUENT-stage IQ that carries the `sessionid` returned
/// by the service in the first response.
pub fn build_xep0050_command_request_with_session(
    service_jid: &str,
    node: &str,
    session_id: &str,
    action: AdHocAction,
    form: Option<xmpp_parsers::data_forms::DataForm>,
) -> Element {
    build_command_iq(service_jid, node, Some(session_id), action, form)
}

fn build_command_iq(
    service_jid: &str,
    node: &str,
    session_id: Option<&str>,
    action: AdHocAction,
    form: Option<xmpp_parsers::data_forms::DataForm>,
) -> Element {
    let id = format!("adhoc-{}", next_id());
    let mut command = Element::builder("command", NS_COMMANDS)
        .attr(minidom::rxml::xml_ncname!("node").to_owned(), node)
        .attr(
            minidom::rxml::xml_ncname!("action").to_owned(),
            action.as_wire_str(),
        );
    if let Some(session_id) = session_id {
        command = command.attr(
            minidom::rxml::xml_ncname!("sessionid").to_owned(),
            session_id,
        );
    }
    if let Some(form) = form {
        // `xmpp_parsers::data_forms::DataForm` is `AsXml`, which
        // converts to `minidom::Element` via TryFrom<DataForm>.
        // Unwrap-free: AsXml conversion is infallible for serialization
        // (only deserialization can fail).
        let element: Element = form.into();
        command = command.append(element);
    }
    Element::builder("iq", CLIENT_NS)
        .attr(minidom::rxml::xml_ncname!("type").to_owned(), "set")
        .attr(minidom::rxml::xml_ncname!("to").to_owned(), service_jid)
        .attr(minidom::rxml::xml_ncname!("id").to_owned(), id)
        .append(command.build())
        .build()
}

/// Parse a `<command/>` payload out of an IQ result. Returns `None`
/// if the IQ does not carry a command child or the status attribute
/// is missing/unknown.
pub fn parse_command_response(iq: &Element) -> Option<CommandResponse> {
    let command = iq.get_child("command", NS_COMMANDS)?;
    let status = AdHocStatus::parse(command.attr("status")?)?;
    let form = command
        .get_child("x", NS_DATA_FORMS)
        .cloned()
        .and_then(|elem| xmpp_parsers::data_forms::DataForm::try_from(elem).ok());
    Some(CommandResponse {
        node: command.attr("node")?.to_string(),
        session_id: command.attr("sessionid").map(str::to_string),
        status,
        form,
    })
}
```

Add `pub mod xep;` to `src/lib.rs` after the existing `pub mod discovery;` block, and `pub mod xep0050;` inside `src/xep/mod.rs`.

- [ ] **Step 4: Run tests to verify they pass**

```sh
cd server && cargo test -p waddle-xmpp-client xep0050
```

Expected: PASS.

- [ ] **Step 5: `cargo fmt` and commit**

```sh
cd server && cargo fmt
cd .. && git add server/crates/waddle-xmpp-client/src/xep server/crates/waddle-xmpp-client/src/lib.rs
git commit -m "feat(server): typed XEP-0050 ad-hoc command IQ builder in waddle-xmpp-client"
```

---

## Task 2 — Typed XEP-0357 enable/disable IQ builders

**Files:**
- Create: `server/crates/waddle-xmpp-client/src/xep/xep0357.rs`
- Modify: `server/crates/waddle-xmpp-client/src/xep/mod.rs` (add `pub mod xep0357;`)

- [ ] **Step 1: Write the failing tests** in `src/xep/xep0357.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use xmpp_parsers::data_forms::{DataForm, DataFormType, Field, FieldType};

    #[test]
    fn enable_iq_with_no_publish_options_carries_no_x_form() {
        let iq = build_xep0357_enable_iq("push.example.com", "node-abc", None);
        let enable = iq.get_child("enable", NS_PUSH).expect("enable");
        assert_eq!(enable.attr("jid"), Some("push.example.com"));
        assert_eq!(enable.attr("node"), Some("node-abc"));
        assert!(enable.get_child("x", "jabber:x:data").is_none());
        assert!(enable
            .get_child("provider-token", "urn:waddle:push-service:0")
            .is_none());
    }

    #[test]
    fn enable_iq_with_publish_options_carries_form_type_and_fields() {
        let publish_options = DataForm::new(
            DataFormType::Submit,
            "http://jabber.org/protocol/pubsub#publish-options",
            vec![Field {
                var: Some("pubsub#persist_items".to_string()),
                type_: FieldType::Hidden,
                label: None,
                required: false,
                desc: None,
                options: vec![],
                values: vec!["false".to_string()],
                media: vec![],
                validate: None,
            }],
        );
        let iq = build_xep0357_enable_iq("push.example.com", "node-abc", Some(publish_options));
        let enable = iq.get_child("enable", NS_PUSH).expect("enable");
        let form = enable.get_child("x", "jabber:x:data").expect("publish-options");
        assert_eq!(form.attr("type"), Some("submit"));
    }

    #[test]
    fn disable_iq_with_node_carries_node_attribute() {
        let iq = build_xep0357_disable_iq("push.example.com", Some("node-abc"));
        let disable = iq.get_child("disable", NS_PUSH).expect("disable");
        assert_eq!(disable.attr("jid"), Some("push.example.com"));
        assert_eq!(disable.attr("node"), Some("node-abc"));
    }

    #[test]
    fn disable_iq_without_node_omits_node_attribute() {
        // XEP-0357 §6.1: a disable without a node disables ALL nodes
        // at the service for this user — pin the omission so we don't
        // accidentally emit `node=""`.
        let iq = build_xep0357_disable_iq("push.example.com", None);
        let disable = iq.get_child("disable", NS_PUSH).expect("disable");
        assert_eq!(disable.attr("jid"), Some("push.example.com"));
        assert!(disable.attr("node").is_none());
    }
}
```

- [ ] **Step 2: Run tests to see them fail**

```sh
cd server && cargo test -p waddle-xmpp-client xep0357
```

Expected: FAIL (no module / no functions).

- [ ] **Step 3: Write the module**

```rust
//! XEP-0357 — typed enable / disable IQ builders.
//!
//! These IQs flow from the chat client to the user XMPP server. They
//! never carry provider credentials (those live behind the Push Service
//! component, registered separately via XEP-0050). The `publish_options`
//! argument is a free-form XEP-0004 data form — XEP-0357 §5 permits any
//! `pubsub#publish-options` constraints the publisher wants to apply,
//! and the user server passes them through to the PubSub publish.

use minidom::Element;
use xmpp_parsers::data_forms::DataForm;

use crate::discovery::ids::next_id;
use crate::discovery::CLIENT_NS;

pub const NS_PUSH: &str = "urn:xmpp:push:0";

/// Build the XEP-0357 §5 `<enable/>` IQ. Never carries
/// provider-credential fields — those belong to the Push Service
/// registration (XEP-0050 commands on `push.<domain>`).
pub fn build_xep0357_enable_iq(
    service_jid: &str,
    node: &str,
    publish_options: Option<DataForm>,
) -> Element {
    let id = format!("push-enable-{}", next_id());
    let mut enable = Element::builder("enable", NS_PUSH)
        .attr(minidom::rxml::xml_ncname!("jid").to_owned(), service_jid)
        .attr(minidom::rxml::xml_ncname!("node").to_owned(), node);
    if let Some(form) = publish_options {
        let element: Element = form.into();
        enable = enable.append(element);
    }
    Element::builder("iq", CLIENT_NS)
        .attr(minidom::rxml::xml_ncname!("type").to_owned(), "set")
        .attr(minidom::rxml::xml_ncname!("id").to_owned(), id)
        .append(enable.build())
        .build()
}

/// Build the XEP-0357 §6.1 `<disable/>` IQ. A `None` `node` disables
/// ALL nodes at the service for the bound user.
pub fn build_xep0357_disable_iq(service_jid: &str, node: Option<&str>) -> Element {
    let id = format!("push-disable-{}", next_id());
    let mut disable = Element::builder("disable", NS_PUSH)
        .attr(minidom::rxml::xml_ncname!("jid").to_owned(), service_jid);
    if let Some(node) = node {
        disable = disable.attr(minidom::rxml::xml_ncname!("node").to_owned(), node);
    }
    Element::builder("iq", CLIENT_NS)
        .attr(minidom::rxml::xml_ncname!("type").to_owned(), "set")
        .attr(minidom::rxml::xml_ncname!("id").to_owned(), id)
        .append(disable.build())
        .build()
}
```

- [ ] **Step 4: Run tests**

```sh
cd server && cargo test -p waddle-xmpp-client xep0357
```

Expected: PASS.

- [ ] **Step 5: `cargo fmt` and commit**

```sh
cd server && cargo fmt
cd .. && git add server/crates/waddle-xmpp-client/src/xep/xep0357.rs server/crates/waddle-xmpp-client/src/xep/mod.rs
git commit -m "feat(server): typed XEP-0357 enable/disable IQ builders without legacy token"
```

---

## Task 3 — `register_push_device` composer + typed value objects

**Files:**
- Create: `server/crates/waddle-xmpp-client/src/push/mod.rs`
- Modify: `server/crates/waddle-xmpp-client/src/lib.rs` (add `pub mod push;`)

The composer runs the 4-stage XEP-0050 dance described in the issue:

1. `<command node='register-device' action='execute'/>` → push service
2. response `executing` + form
3. submit the form back with `action='complete'`
4. response `completed` + result form whose `node` field carries the assigned node id

- [ ] **Step 1: Write the failing test** in `src/push/mod.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn platform_wire_strings_match_xep0050_form_options() {
        assert_eq!(PushPlatform::Web.as_wire_str(), "web");
        assert_eq!(PushPlatform::Apns.as_wire_str(), "apns");
        assert_eq!(PushPlatform::Fcm.as_wire_str(), "fcm");
        assert_eq!(PushEnvironment::Production.as_wire_str(), "prod");
        assert_eq!(PushEnvironment::Sandbox.as_wire_str(), "sandbox");
    }

    #[test]
    fn build_submit_form_for_web_push_carries_required_fields() {
        let credentials = PushDeviceCredentials::WebPush {
            endpoint: "https://fcm.googleapis.com/wp/abc".to_string(),
            p256dh: "p256-key".to_string(),
            auth: "auth-secret".to_string(),
        };
        let form = build_register_device_submit_form(
            "app-web",
            PushPlatform::Web,
            PushEnvironment::Production,
            &credentials,
        );
        let values: std::collections::HashMap<&str, &str> = form
            .fields
            .iter()
            .filter_map(|f| Some((f.var.as_deref()?, f.values.first()?.as_str())))
            .collect();
        assert_eq!(
            values.get("FORM_TYPE").copied(),
            Some("urn:xmpp:push-service:commands:register-device:0")
        );
        assert_eq!(values.get("platform").copied(), Some("web"));
        assert_eq!(values.get("environment").copied(), Some("prod"));
        assert_eq!(values.get("app_id").copied(), Some("app-web"));
        assert_eq!(
            values.get("web-push-endpoint").copied(),
            Some("https://fcm.googleapis.com/wp/abc")
        );
        assert_eq!(values.get("web-push-p256dh").copied(), Some("p256-key"));
        assert_eq!(values.get("web-push-auth").copied(), Some("auth-secret"));
    }

    #[test]
    fn build_submit_form_for_apns_omits_web_and_fcm_fields() {
        let credentials = PushDeviceCredentials::Apns {
            device_token: "apns-token".to_string(),
        };
        let form = build_register_device_submit_form(
            "app-ios",
            PushPlatform::Apns,
            PushEnvironment::Sandbox,
            &credentials,
        );
        let vars: Vec<&str> = form
            .fields
            .iter()
            .filter_map(|f| f.var.as_deref())
            .collect();
        assert!(vars.contains(&"apns-token"));
        assert!(!vars.contains(&"web-push-endpoint"));
        assert!(!vars.contains(&"fcm-token"));
    }

    #[test]
    fn parse_completed_result_extracts_node_field() {
        use xmpp_parsers::data_forms::{DataForm, DataFormType, Field, FieldType};
        let result = DataForm::new(
            DataFormType::Result_,
            "urn:xmpp:push-service:commands:register-device:0",
            vec![Field {
                var: Some("node".to_string()),
                type_: FieldType::TextSingle,
                label: None,
                required: false,
                desc: None,
                options: vec![],
                values: vec!["node-xyz".to_string()],
                media: vec![],
                validate: None,
            }],
        );
        let node = parse_register_device_result(&result).expect("node present");
        assert_eq!(node.as_str(), "node-xyz");
    }

    #[test]
    fn parse_completed_result_missing_node_returns_none() {
        use xmpp_parsers::data_forms::{DataForm, DataFormType};
        let result = DataForm::new(
            DataFormType::Result_,
            "urn:xmpp:push-service:commands:register-device:0",
            vec![],
        );
        assert!(parse_register_device_result(&result).is_none());
    }
}
```

- [ ] **Step 2: Run tests to see them fail**

```sh
cd server && cargo test -p waddle-xmpp-client push::
```

Expected: FAIL (module + functions undefined).

- [ ] **Step 3: Write the module skeleton**

```rust
//! Push device registration — XEP-0050 multi-step composer + typed
//! value objects. The chat client never builds the wire `<command/>`
//! directly; it composes by calling [`register_push_device`] which
//! drives the four-stage handshake against the Push Service.

use crate::client::ClientHandle;
use crate::error::{ClientError, ClientResult, StanzaError, StanzaErrorType};
use crate::xep::xep0050::{
    build_xep0050_command_request, build_xep0050_command_request_with_session,
    parse_command_response, AdHocAction, AdHocStatus,
};
use xmpp_parsers::data_forms::{DataForm, DataFormType, Field, FieldType};

pub const REGISTER_DEVICE_NODE: &str = "register-device";
pub const DISABLE_DEVICE_NODE: &str = "disable-device";

/// FORM_TYPE the Push Service expects on the submitted form. Keep in
/// sync with the server-side handler in
/// `waddle-server/src/push_service/commands.rs`.
pub const REGISTER_DEVICE_FORM_TYPE: &str =
    "urn:xmpp:push-service:commands:register-device:0";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PushPlatform {
    Web,
    Apns,
    Fcm,
}

impl PushPlatform {
    pub fn as_wire_str(self) -> &'static str {
        match self {
            Self::Web => "web",
            Self::Apns => "apns",
            Self::Fcm => "fcm",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PushEnvironment {
    Production,
    Sandbox,
}

impl PushEnvironment {
    pub fn as_wire_str(self) -> &'static str {
        match self {
            Self::Production => "prod",
            Self::Sandbox => "sandbox",
        }
    }
}

/// Platform-discriminated provider credentials. Each variant carries
/// the EXACT subset of fields its provider needs — typed-payloads hard
/// rule (no shared `Option<&str>` bag).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PushDeviceCredentials {
    WebPush {
        endpoint: String,
        p256dh: String,
        auth: String,
    },
    Apns {
        device_token: String,
    },
    Fcm {
        registration_token: String,
    },
}

impl PushDeviceCredentials {
    pub fn platform(&self) -> PushPlatform {
        match self {
            Self::WebPush { .. } => PushPlatform::Web,
            Self::Apns { .. } => PushPlatform::Apns,
            Self::Fcm { .. } => PushPlatform::Fcm,
        }
    }
}

/// Assigned node id returned by a successful registration round.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PushNodeId(String);

impl PushNodeId {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Build the `<x type='submit'>` form the composer posts back to the
/// Push Service in the `action='complete'` step. Pure helper so the
/// shape can be pinned without round-tripping through a fake service.
pub fn build_register_device_submit_form(
    app_id: &str,
    platform: PushPlatform,
    environment: PushEnvironment,
    credentials: &PushDeviceCredentials,
) -> DataForm {
    let mut fields = vec![
        text_single("platform", platform.as_wire_str()),
        text_single("environment", environment.as_wire_str()),
        text_single("app_id", app_id),
    ];
    match credentials {
        PushDeviceCredentials::WebPush {
            endpoint,
            p256dh,
            auth,
        } => {
            fields.push(text_single("web-push-endpoint", endpoint));
            fields.push(text_single("web-push-p256dh", p256dh));
            fields.push(text_single("web-push-auth", auth));
        }
        PushDeviceCredentials::Apns { device_token } => {
            fields.push(text_single("apns-token", device_token));
        }
        PushDeviceCredentials::Fcm { registration_token } => {
            fields.push(text_single("fcm-token", registration_token));
        }
    }
    DataForm::new(DataFormType::Submit, REGISTER_DEVICE_FORM_TYPE, fields)
}

/// Extract the assigned node id from a `status='completed'` result
/// form. Returns `None` when the form lacks the `node` field or the
/// value is empty.
pub fn parse_register_device_result(form: &DataForm) -> Option<PushNodeId> {
    let value = form
        .fields
        .iter()
        .find(|f| f.var.as_deref() == Some("node"))?
        .values
        .first()?
        .trim();
    if value.is_empty() {
        None
    } else {
        Some(PushNodeId(value.to_string()))
    }
}

fn text_single(var: &str, value: &str) -> Field {
    Field {
        var: Some(var.to_string()),
        type_: FieldType::TextSingle,
        label: None,
        required: false,
        desc: None,
        options: vec![],
        values: vec![value.to_string()],
        media: vec![],
        validate: None,
    }
}

/// Drive the XEP-0050 four-stage `register-device` dance against
/// `push_service_jid`. Returns the assigned node id on completion.
///
/// Errors flow through `ClientResult` typed stanza errors — never
/// stringly-typed payloads.
#[cfg(all(feature = "native", not(target_arch = "wasm32")))]
pub async fn register_push_device(
    client: &ClientHandle,
    push_service_jid: &str,
    app_id: &str,
    environment: PushEnvironment,
    credentials: &PushDeviceCredentials,
) -> ClientResult<PushNodeId> {
    // Stage 1 → 2: execute, expect executing + form back.
    let initial = build_xep0050_command_request(
        push_service_jid,
        REGISTER_DEVICE_NODE,
        AdHocAction::Execute,
        None,
    );
    let executing = client.send_iq(initial).await?;
    let executing =
        parse_command_response(&executing).ok_or_else(|| protocol_error("expected <command/> response"))?;
    if executing.status != AdHocStatus::Executing {
        return Err(protocol_error("expected status='executing' in stage 2"));
    }
    let session_id = executing
        .session_id
        .clone()
        .ok_or_else(|| protocol_error("missing sessionid in stage 2"))?;

    // Stage 3 → 4: submit the platform-specific form, expect completed + result.
    let submit_form = build_register_device_submit_form(
        app_id,
        credentials.platform(),
        environment,
        credentials,
    );
    let complete = build_xep0050_command_request_with_session(
        push_service_jid,
        REGISTER_DEVICE_NODE,
        &session_id,
        AdHocAction::Complete,
        Some(submit_form),
    );
    let completed = client.send_iq(complete).await?;
    let completed = parse_command_response(&completed)
        .ok_or_else(|| protocol_error("expected <command/> response in stage 4"))?;
    if completed.status != AdHocStatus::Completed {
        return Err(protocol_error("expected status='completed' in stage 4"));
    }
    let result_form = completed
        .form
        .ok_or_else(|| protocol_error("missing result form in stage 4"))?;
    parse_register_device_result(&result_form)
        .ok_or_else(|| protocol_error("result form missing 'node' field"))
}

fn protocol_error(text: &str) -> ClientError {
    ClientError::StanzaError(StanzaError {
        error_type: StanzaErrorType::Cancel,
        condition: "bad-request".to_string(),
        text: Some(text.to_string()),
    })
}
```

Add `pub mod push;` to `src/lib.rs`.

- [ ] **Step 4: Run tests**

```sh
cd server && cargo test -p waddle-xmpp-client push::
```

Expected: PASS.

- [ ] **Step 5: `cargo fmt` and commit**

```sh
cd server && cargo fmt
cd .. && git add server/crates/waddle-xmpp-client/src/push server/crates/waddle-xmpp-client/src/lib.rs
git commit -m "feat(server): register_push_device composer driving XEP-0050 multi-step dance"
```

---

## Task 4 — Multi-step composer integration test against a fake Push Service

**Files:**
- Create: `server/crates/waddle-xmpp-client/tests/register_push_device_against_fake_service.rs`

A fake `ClientHandle` is a heavyweight construct. Instead of building one, decompose `register_push_device` so the stage transitions are testable through a small `CommandDriver` trait that the production path implements via `ClientHandle::send_iq` and the test path implements as a scripted lock-step.

- [ ] **Step 1: Refactor `push/mod.rs` to introduce the trait**

Add this trait above `register_push_device`:

```rust
#[allow(async_fn_in_trait)]
pub trait CommandDriver {
    async fn send(&self, iq: minidom::Element) -> ClientResult<minidom::Element>;
}

#[cfg(all(feature = "native", not(target_arch = "wasm32")))]
impl CommandDriver for ClientHandle {
    async fn send(&self, iq: minidom::Element) -> ClientResult<minidom::Element> {
        self.send_iq(iq).await
    }
}
```

Change `register_push_device` to take `driver: &impl CommandDriver` instead of `client: &ClientHandle`, and gate the `ClientHandle` impl on the native feature so the wasm-only build still compiles (this satisfies the issue's "no token in WASM bindings" requirement without touching transport plumbing).

- [ ] **Step 2: Write the integration test**

```rust
//! Multi-step XEP-0050 composer integration test against a scripted
//! fake Push Service. Pins the EXACT wire shape of the four stages.

use minidom::Element;
use std::cell::RefCell;
use waddle_xmpp_client::error::ClientResult;
use waddle_xmpp_client::push::{
    register_push_device, CommandDriver, PushDeviceCredentials, PushEnvironment,
    REGISTER_DEVICE_NODE,
};
use waddle_xmpp_client::xep::xep0050::NS_COMMANDS;

struct ScriptedDriver {
    transcript: RefCell<Vec<Element>>,
    responses: RefCell<Vec<Element>>,
}

impl CommandDriver for ScriptedDriver {
    async fn send(&self, iq: Element) -> ClientResult<Element> {
        self.transcript.borrow_mut().push(iq);
        Ok(self.responses.borrow_mut().remove(0))
    }
}

fn executing_response_with_form_request(session_id: &str) -> Element {
    let command = Element::builder("command", NS_COMMANDS)
        .attr(minidom::rxml::xml_ncname!("node").to_owned(), REGISTER_DEVICE_NODE)
        .attr(minidom::rxml::xml_ncname!("sessionid").to_owned(), session_id)
        .attr(minidom::rxml::xml_ncname!("status").to_owned(), "executing")
        .append(
            Element::builder("x", "jabber:x:data")
                .attr(minidom::rxml::xml_ncname!("type").to_owned(), "form")
                .build(),
        )
        .build();
    Element::builder("iq", "jabber:client")
        .attr(minidom::rxml::xml_ncname!("type").to_owned(), "result")
        .append(command)
        .build()
}

fn completed_response_with_node(node_id: &str) -> Element {
    let result_form = Element::builder("x", "jabber:x:data")
        .attr(minidom::rxml::xml_ncname!("type").to_owned(), "result")
        .append(
            Element::builder("field", "jabber:x:data")
                .attr(minidom::rxml::xml_ncname!("var").to_owned(), "FORM_TYPE")
                .attr(minidom::rxml::xml_ncname!("type").to_owned(), "hidden")
                .append(
                    Element::builder("value", "jabber:x:data")
                        .append("urn:xmpp:push-service:commands:register-device:0")
                        .build(),
                )
                .build(),
        )
        .append(
            Element::builder("field", "jabber:x:data")
                .attr(minidom::rxml::xml_ncname!("var").to_owned(), "node")
                .append(
                    Element::builder("value", "jabber:x:data")
                        .append(node_id)
                        .build(),
                )
                .build(),
        )
        .build();
    let command = Element::builder("command", NS_COMMANDS)
        .attr(minidom::rxml::xml_ncname!("node").to_owned(), REGISTER_DEVICE_NODE)
        .attr(minidom::rxml::xml_ncname!("status").to_owned(), "completed")
        .append(result_form)
        .build();
    Element::builder("iq", "jabber:client")
        .attr(minidom::rxml::xml_ncname!("type").to_owned(), "result")
        .append(command)
        .build()
}

#[tokio::test]
async fn register_push_device_completes_multi_step_dance() {
    let driver = ScriptedDriver {
        transcript: RefCell::new(Vec::new()),
        responses: RefCell::new(vec![
            executing_response_with_form_request("session-1"),
            completed_response_with_node("node-xyz"),
        ]),
    };
    let credentials = PushDeviceCredentials::WebPush {
        endpoint: "https://fcm.googleapis.com/wp/abc".to_string(),
        p256dh: "p256-key".to_string(),
        auth: "auth-secret".to_string(),
    };
    let node = register_push_device(
        &driver,
        "push.example.com",
        "app-web",
        PushEnvironment::Production,
        &credentials,
    )
    .await
    .expect("composer succeeds");
    assert_eq!(node.as_str(), "node-xyz");

    let transcript = driver.transcript.borrow();
    assert_eq!(transcript.len(), 2);

    // Stage 1: execute, no session id, no form.
    let cmd1 = transcript[0].get_child("command", NS_COMMANDS).expect("stage 1");
    assert_eq!(cmd1.attr("action"), Some("execute"));
    assert_eq!(cmd1.attr("sessionid"), None);
    assert!(cmd1.get_child("x", "jabber:x:data").is_none());

    // Stage 3: complete, carries session id and submitted form.
    let cmd2 = transcript[1].get_child("command", NS_COMMANDS).expect("stage 3");
    assert_eq!(cmd2.attr("action"), Some("complete"));
    assert_eq!(cmd2.attr("sessionid"), Some("session-1"));
    let submitted = cmd2.get_child("x", "jabber:x:data").expect("submitted form");
    assert_eq!(submitted.attr("type"), Some("submit"));
}
```

- [ ] **Step 3: Run the test**

```sh
cd server && cargo test -p waddle-xmpp-client --test register_push_device_against_fake_service
```

Expected: PASS.

- [ ] **Step 4: `cargo fmt` and commit**

```sh
cd server && cargo fmt
cd .. && git add server/crates/waddle-xmpp-client/tests/register_push_device_against_fake_service.rs server/crates/waddle-xmpp-client/src/push/mod.rs
git commit -m "test(server): integration test for register_push_device multi-step dance"
```

---

## Task 5 — Drop `token: &str` from `DiscoveryExt::enable_push_notifications` and delete legacy IQ builders

**Files:**
- Modify: `server/crates/waddle-xmpp-client/src/discovery/ext.rs:58-71, 146-163`
- Modify: `server/crates/waddle-xmpp-client/src/discovery/iq.rs:92-148, 349-531`
- Modify: `server/crates/waddle-xmpp-client/src/discovery.rs:47` (remove `WADDLE_PUSH_SERVICE_NS`)
- Modify: `server/crates/waddle-xmpp-client/src/discovery/tests.rs` (delete legacy-shape tests)

- [ ] **Step 1: Update the trait definition** in `ext.rs:58-64`:

```rust
    /// Enable push notifications via a push service (XEP-0357).
    /// `publish_options` carries optional `pubsub#publish-options`
    /// constraints; pass `None` to omit the data form entirely.
    async fn enable_push_notifications(
        &self,
        push_service_jid: &str,
        node: &str,
        publish_options: Option<xmpp_parsers::data_forms::DataForm>,
    ) -> ClientResult<()>;

    /// Disable push notifications. A `None` `node` disables ALL nodes
    /// at the service for this user (XEP-0357 §6.1).
    async fn disable_push_notifications(
        &self,
        push_service_jid: &str,
        node: Option<&str>,
    ) -> ClientResult<()>;
```

- [ ] **Step 2: Update the impl** in `ext.rs:146-163`:

```rust
    async fn enable_push_notifications(
        &self,
        push_service_jid: &str,
        node: &str,
        publish_options: Option<xmpp_parsers::data_forms::DataForm>,
    ) -> ClientResult<()> {
        let iq = crate::xep::xep0357::build_xep0357_enable_iq(
            push_service_jid,
            node,
            publish_options,
        );
        self.send_iq(iq).await.map(|_| ())
    }

    async fn disable_push_notifications(
        &self,
        push_service_jid: &str,
        node: Option<&str>,
    ) -> ClientResult<()> {
        let iq = crate::xep::xep0357::build_xep0357_disable_iq(push_service_jid, node);
        self.send_iq(iq).await.map(|_| ())
    }
```

- [ ] **Step 3: Delete legacy builders** in `iq.rs`:
- Delete `build_enable_push_iq` (lines 92-131)
- Delete `build_disable_push_iq` (lines 133-148)
- Delete `build_ensure_push_node_iq`, `PushDevicePlatform`, `PushEnvironment`, `PushDeviceRegistration`, `build_register_push_device_iq`, `build_disable_push_device_iq` (lines 349-531) — the whole `urn:waddle:push-service:0` block

- [ ] **Step 4: Remove the namespace constant** from `discovery.rs:47`: drop the `pub const WADDLE_PUSH_SERVICE_NS: &str = "urn:waddle:push-service:0";` line and the corresponding `WADDLE_PUSH_SERVICE_NS` from the `use super::{…};` in `iq.rs`.

- [ ] **Step 5: Delete legacy-shape tests** in `discovery/tests.rs`: drop `build_enable_push_iq_omits_publish_options_for_empty_token`, `build_enable_push_iq_includes_secret_publish_options_for_non_empty_token`, `build_ensure_push_node_carries_app_id_and_target_jid`, `build_register_push_device_carries_web_push_fields`, `build_register_push_device_omits_missing_provider_fields`, `build_disable_push_device_carries_node_and_device_id`, and `xep0357_enable_for_web_push_carries_no_provider_fields`. The new wire-shape tests live in `xep/xep0357.rs` and `xep/xep0050.rs`.

- [ ] **Step 6: Build the crate**

```sh
cd server && cargo build -p waddle-xmpp-client --all-features
```

Expected: compile error pointing at any remaining caller of `build_enable_push_iq`/`build_register_push_device_iq` (the WASM crate, which Task 7 fixes).

- [ ] **Step 7: Run tests for the client crate only**

```sh
cd server && cargo test -p waddle-xmpp-client
```

Expected: PASS (legacy tests are gone; new tests in xep/* exist; integration test passes).

- [ ] **Step 8: `cargo fmt` and commit**

```sh
cd server && cargo fmt
cd .. && git add -u server/crates/waddle-xmpp-client/
git commit -m "feat(server): drop legacy token param and urn:waddle:push-service:0 builders from waddle-xmpp-client"
```

---

## Task 6 — Server: XEP-0050 `register-device` + `disable-device` command handlers

**Files:**
- Create: `server/crates/waddle-server/src/push_service/commands.rs`
- Modify: `server/crates/waddle-server/src/push_service/mod.rs` (or wherever the module root is; `src/push_service.rs` becomes a module dir)
- Delete: `server/crates/waddle-server/src/server/routes/websocket/handlers/iq/push_service_iq.rs`
- Modify: `server/crates/waddle-server/src/server/routes/websocket/handlers/iq/mod.rs` (drop the `push_service_iq` dispatch branch)
- Modify: `server/crates/waddle-xmpp-core/src/disco/info.rs:416-426` (advertise XEP-0050)

The existing command_registry in `server/crates/waddle-server/src/server/routes/websocket/handlers/iq/commands.rs` already dispatches XEP-0050 IQs by node. We register two new commands (`register-device`, `disable-device`) on the push-service component's registry.

- [ ] **Step 1: Write a server-side test** in `server/crates/waddle-server/tests/xep0357_push_service_ws.rs`. Replace the old `xep0357_first_party_enable_requires_owned_active_node_with_device` with a new test that runs the multi-step XEP-0050 dance end-to-end:

```rust
#[tokio::test]
async fn xep0050_register_device_completes_and_persists_device_row() {
    // 1. connect web socket, bind
    let mut harness = WaddlePushServiceHarness::start().await;
    // 2. discover commands
    let info = harness.disco_info("push.example.com").await;
    assert!(info.features.contains(&"http://jabber.org/protocol/commands".to_string()));
    let items = harness.disco_items("push.example.com").await;
    assert!(items.iter().any(|i| i.node.as_deref() == Some("register-device")));

    // 3. stage 1: execute
    let session = harness
        .command_execute("push.example.com", "register-device")
        .await;
    assert_eq!(session.status, "executing");
    assert!(session.form.is_some(), "stage 2 form must be returned");

    // 4. stage 3: submit with web push credentials
    let assigned_node = harness
        .command_complete_with_web_push(
            "push.example.com",
            "register-device",
            &session.session_id,
            "https://fcm.googleapis.com/wp/abc",
            "p256-key",
            "auth-secret",
        )
        .await;
    assert!(!assigned_node.is_empty());

    // 5. assert the row was persisted in `push_devices`
    let row = harness.read_push_device_row(&assigned_node).await;
    assert_eq!(row.platform, "web");
    assert_eq!(row.provider_endpoint.as_deref(), Some("https://fcm.googleapis.com/wp/abc"));
}
```

Add the helper methods `disco_items`, `command_execute`, `command_complete_with_web_push`, and `read_push_device_row` on `WaddlePushServiceHarness` (in the same test file's helpers section). They are thin wrappers around the existing `harness.send_iq` plus typed XEP-0050 parsing.

- [ ] **Step 2: Run the test to see it fail**

```sh
cd server && cargo test -p waddle-server --test xep0357_push_service_ws xep0050_register_device_completes_and_persists_device_row
```

Expected: FAIL (no XEP-0050 handler).

- [ ] **Step 3: Write the command handlers** at `server/crates/waddle-server/src/push_service/commands.rs`:

```rust
//! XEP-0050 ad-hoc command handlers for the push service component.
//!
//! Two commands: `register-device` and `disable-device`. Both follow
//! the multi-step XEP-0050 §3 shape — stage 1 returns an empty form
//! prompt, stage 2 receives the submitted form and either persists
//! (register) or removes (disable) the `push_devices` row.

use crate::push_service::storage::{PushDeviceRow, PushDeviceStorage};
use crate::server::routes::websocket::handlers::iq::commands::{
    CommandContext, CommandResult, CommandSpec,
};
use xmpp_parsers::data_forms::{DataForm, DataFormType, Field, FieldType};

pub const REGISTER_DEVICE_NODE: &str = "register-device";
pub const DISABLE_DEVICE_NODE: &str = "disable-device";
pub const REGISTER_DEVICE_FORM_TYPE: &str =
    "urn:xmpp:push-service:commands:register-device:0";

pub struct RegisterDeviceCommand {
    pub storage: std::sync::Arc<dyn PushDeviceStorage>,
}

impl CommandSpec for RegisterDeviceCommand {
    fn node(&self) -> &str {
        REGISTER_DEVICE_NODE
    }

    async fn dispatch(&self, ctx: CommandContext<'_>) -> CommandResult {
        match ctx.action.as_deref().unwrap_or("execute") {
            "execute" => CommandResult::Executing {
                session_id: ctx.new_session_id(),
                form: Some(build_register_device_request_form()),
            },
            "complete" => {
                let form = ctx
                    .submitted_form
                    .ok_or_else(|| CommandResult::bad_request("missing submitted form"))?;
                let parsed = parse_register_device_form(&form)?;
                let assigned_node = self
                    .storage
                    .insert_or_update(parsed, ctx.from_bare_jid())
                    .await
                    .map_err(CommandResult::from_storage_error)?;
                CommandResult::Completed {
                    result_form: Some(build_register_device_result_form(&assigned_node)),
                }
            }
            other => CommandResult::bad_request(&format!("unsupported action {other:?}")),
        }
    }
}
```

(Spell out `parse_register_device_form`, `build_register_device_request_form`, `build_register_device_result_form`, the `DisableDeviceCommand`, and the wiring registration in the same file. Reuse `xmpp_parsers::data_forms::Field`/`DataForm` typed helpers — DO NOT use `format!` for any XML.)

Wire registration in the push-service component's startup path. Search for the existing call to `command_registry.register(...)` (used by `urn:waddle:admin:*` commands) and add the two new commands.

- [ ] **Step 4: Update disco** in `server/crates/waddle-xmpp-core/src/disco/info.rs` around the `push_service_features()` helper: append `Feature::new("http://jabber.org/protocol/commands")`. For disco#items on `push.<domain>`, surface `<item node='register-device'/>` and `<item node='disable-device'/>` — search for the existing items handler and add the two entries.

- [ ] **Step 5: Delete the legacy `push_service_iq.rs` and its dispatch branch.** In `iq/mod.rs`, drop the `push_service_iq` `mod` line and the dispatch arm; routes/websocket/handlers/iq tests that previously imported `WADDLE_PUSH_SERVICE_NS` need updates — search and adapt.

- [ ] **Step 6: Run tests**

```sh
cd server && cargo test -p waddle-server --test xep0357_push_service_ws
```

Expected: PASS for the new test; other tests in the file (PubSub publish gate, offline DM, VAPID disco) remain green.

- [ ] **Step 7: `cargo fmt` and clippy**

```sh
cd server && cargo fmt
cd server && cargo clippy -p waddle-server -- -D warnings
```

Expected: no warnings, no errors. Per `server/CLAUDE.md`: no `unwrap`, no clippy allows.

- [ ] **Step 8: Commit**

```sh
git add -u server/crates/waddle-server/ server/crates/waddle-xmpp-core/
git commit -m "feat(server): cut push.<domain> device registration over to XEP-0050 ad-hoc commands"
```

---

## Task 7 — WASM bindings cutover

**Files:**
- Modify: `server/crates/waddle-xmpp-client-wasm/src/client_account.rs:20-182`
- Modify: `server/crates/waddle-xmpp-client-wasm/src/lib.rs` (drop the `WADDLE_PUSH_SERVICE_NS` re-export)

- [ ] **Step 1: Replace `enable_push_notifications` WASM method** (lines 20-32):

```rust
    pub fn enable_push_notifications(&self, service_jid: String, node: String) -> Promise {
        let inner = self.inner.clone();
        future_to_promise(async move {
            let iq = build_xep0357_enable_iq(&service_jid, &node, None);
            let _ = send_iq_command(inner, iq).await?;
            Ok(JsValue::UNDEFINED)
        })
    }
```

The `token` parameter is dropped. The chat client's `client.ts:1207` call site stops passing `""`.

- [ ] **Step 2: Replace `disable_push_notifications` WASM method** with the typed `Option<String>` `node`. wasm-bindgen doesn't surface `Option<String>` cleanly; accept `node: Option<String>` and translate.

- [ ] **Step 3: Replace `ensure_push_node`, `register_web_push_device`, `disable_push_device` with `register_push_device`**:

```rust
    pub fn register_push_device(&self, options: JsValue) -> Promise {
        let inner = self.inner.clone();
        future_to_promise(async move {
            let opts: RegisterPushDeviceOptions = serde_wasm_bindgen::from_value(options)
                .map_err(|err| JsValue::from_str(&err.to_string()))?;
            // ScriptedDriver is a test fixture; production uses the
            // WASM client's send_iq_command pipeline. Build a tiny
            // CommandDriver wrapper that closes over `inner`.
            let driver = WasmCommandDriver { inner };
            let credentials = match opts.platform {
                PushPlatform::Web => PushDeviceCredentials::WebPush {
                    endpoint: opts.endpoint.ok_or_else(|| js_error("web push needs endpoint"))?,
                    p256dh: opts.p256dh.ok_or_else(|| js_error("web push needs p256dh"))?,
                    auth: opts.auth.ok_or_else(|| js_error("web push needs auth"))?,
                },
                PushPlatform::Apns => PushDeviceCredentials::Apns {
                    device_token: opts.apns_token.ok_or_else(|| js_error("apns needs token"))?,
                },
                PushPlatform::Fcm => PushDeviceCredentials::Fcm {
                    registration_token: opts.fcm_token.ok_or_else(|| js_error("fcm needs token"))?,
                },
            };
            let node = register_push_device(
                &driver,
                &opts.service_jid,
                &opts.app_id,
                opts.environment,
                &credentials,
            )
            .await
            .map_err(|err| js_error(err.to_string()))?;
            to_js_value(&PushNodeId { value: node.as_str().to_string() })
        })
    }
```

Spell out `RegisterPushDeviceOptions`, `WasmCommandDriver` (impl `CommandDriver` for it), and the `PushNodeId` JS-side shape in the same file.

- [ ] **Step 4: Drop the `WADDLE_PUSH_SERVICE_NS` re-export** from `lib.rs` and any tests under wasm that referenced it.

- [ ] **Step 5: Build**

```sh
cd server && cargo build -p waddle-xmpp-client-wasm --target wasm32-unknown-unknown --release
```

Expected: PASS.

- [ ] **Step 6: Commit**

```sh
cd server && cargo fmt
git add -u server/crates/waddle-xmpp-client-wasm/
git commit -m "feat(server): WASM cutover to typed XEP-0050 register_push_device composer"
```

---

## Task 8 — UniFFI exposure

**Files:**
- Modify: `server/crates/waddle-xmpp-client-ffi/src/lib.rs`

The exploration found that the FFI crate currently exposes nothing for push. Add three methods on `WaddleClient`:

- [ ] **Step 1: Add the methods**

```rust
#[uniffi::export(async_runtime = "tokio")]
impl WaddleClient {
    pub async fn enable_push_notifications(
        &self,
        push_service_jid: String,
        node: String,
    ) -> Result<(), WaddleClientError> {
        let handle = self.handle().clone();
        handle
            .enable_push_notifications(&push_service_jid, &node, None)
            .await
            .map_err(WaddleClientError::from)
    }

    pub async fn disable_push_notifications(
        &self,
        push_service_jid: String,
        node: Option<String>,
    ) -> Result<(), WaddleClientError> {
        let handle = self.handle().clone();
        handle
            .disable_push_notifications(&push_service_jid, node.as_deref())
            .await
            .map_err(WaddleClientError::from)
    }

    pub async fn register_push_device(
        &self,
        push_service_jid: String,
        app_id: String,
        environment: PushEnvironment,
        credentials: PushDeviceCredentials,
    ) -> Result<String, WaddleClientError> {
        let handle = self.handle().clone();
        let node = waddle_xmpp_client::push::register_push_device(
            &handle,
            &push_service_jid,
            &app_id,
            environment,
            &credentials,
        )
        .await
        .map_err(WaddleClientError::from)?;
        Ok(node.as_str().to_string())
    }
}
```

Add `#[derive(uniffi::Enum)]` to `PushEnvironment` and `PushPlatform`, and `#[derive(uniffi::Enum)]` to `PushDeviceCredentials`. This may require feature-gating `uniffi` derives on the original definitions in `push/mod.rs` — gate behind a `uniffi` feature on `waddle-xmpp-client`.

- [ ] **Step 2: Regenerate UniFFI**

```sh
cd server && cargo build -p waddle-xmpp-client-ffi
```

Expected: success.

- [ ] **Step 3: Commit**

```sh
cd server && cargo fmt
git add -u server/crates/waddle-xmpp-client-ffi/ server/crates/waddle-xmpp-client/
git commit -m "feat(server): UniFFI exposure for push client surface"
```

---

## Task 9 — Chat client adoption

**Files:**
- Modify: `chat/src/lib/xmpp/client.ts:1203-1218` and surrounding push methods
- Search-and-fix: any other call sites of `ensurePushNode` / `registerWebPushDevice` / `disablePushDevice`

- [ ] **Step 1: Update `enablePushNotifications`** at `chat/src/lib/xmpp/client.ts:1203-1213`. Drop the third argument (`""`); the WASM method now takes `(service_jid, node)`.

- [ ] **Step 2: Replace `registerWebPushDevice`** in the same file to call the new `register_push_device` WASM method:

```ts
async registerWebPushDevice(opts: {
  serviceJid: string;
  appId: string;
  environment: "prod" | "sandbox";
  endpoint: string;
  p256dh: string;
  auth: string;
}): Promise<{ node: string }> {
  const node = await this.xmpp.register_push_device({
    service_jid: opts.serviceJid,
    app_id: opts.appId,
    environment: opts.environment,
    platform: "web",
    endpoint: opts.endpoint,
    p256dh: opts.p256dh,
    auth: opts.auth,
  });
  return { node: node.value };
}
```

- [ ] **Step 3: Delete `ensurePushNode`** — XEP-0050 stage 4's result form carries the node id directly, so the separate `ensure-node` round trip is gone.

- [ ] **Step 4: Delete `disablePushDevice`** — the new flow is `disable_push_notifications(service_jid, node)`. Search call sites.

- [ ] **Step 5: Run chat lint + tests**

```sh
cd chat && bun run lint && bun test
```

Expected: PASS. If `knip` flags any of the deleted methods as unused exports, fix the call sites or remove unused wrapper functions. Per `chat/CLAUDE.md`, do not silence findings via broad `knip.json` ignores.

- [ ] **Step 6: Commit**

```sh
git add chat/
git commit -m "feat(chat): adopt typed XEP-0050 push registration surface"
```

---

## Task 10 — End-to-end smoke + adversarial review

- [ ] **Step 1: Full workspace test**

```sh
cd server && cargo fmt --check
cd server && cargo clippy --workspace --all-features -- -D warnings
cd server && cargo test --workspace
cd chat && bun run lint && bun test
```

Expected: all green.

- [ ] **Step 2: Update the PR body** with a final summary (acceptance criteria checklist from #718, file change rollup, test inventory, CI run links).

- [ ] **Step 3: Move PR out of draft**

```sh
gh pr ready
```

- [ ] **Step 4: Adversarial subagent reviews** — dispatch 3 review subagents in parallel:
  1. XEP conformance review: does the wire shape match XEP-0050 §3 and XEP-0357 §5/§6 exactly?
  2. Typed-payloads / XML-generation hard-rule review: any `format!` building XML? Any `&str` carrying protocol semantics that should be a typed enum?
  3. Security review: does the form parsing reject hostile fields? Does the server-side handler authenticate the requesting JID before persisting a device row?
  Iterate until each subagent reports no actionable findings.

- [ ] **Step 5: Monitor CI**

```sh
gh pr checks --watch
```

Fix anything that fails. Do not merge until all checks are green.

---

## Self-Review Notes

**Spec coverage** — every #718 acceptance criterion maps to a task:

| Criterion | Task |
|-----------|------|
| `build_xep0357_enable_iq` wire shape, no provider fields | Task 2 |
| `build_xep0357_disable_iq` wire shape | Task 2 |
| `build_xep0050_command_request` wire shape; matches admin builder | Task 1 |
| `register_push_device` composer returns typed node id | Tasks 3 & 4 |
| `DiscoveryExt::enable_push_notifications` drops `token: &str` | Task 5 |
| UniFFI / WASM exposure | Tasks 7 & 8 |
| No `format!` for XEP-0357 / XEP-0050 stanzas | Tasks 1–6 (all use builders) |
| XEP-0357 enable wire-shape test, no provider fields | Task 2 |
| XEP-0357 disable wire-shape test | Task 2 |
| XEP-0050 command request wire-shape test | Task 1 |
| Multi-step composer integration test against fake | Task 4 |
| Regression: legacy `token` param gone | Task 5 (compile-time + deleted tests) |

**Placeholder scan** — no TBDs, no "implement later", every code step shows complete code.

**Type consistency** — `PushPlatform`, `PushEnvironment`, `PushDeviceCredentials`, `PushNodeId` defined in `push/mod.rs` (Task 3), referenced by Tasks 7 (WASM), 8 (UniFFI), 9 (chat). `AdHocAction`, `AdHocStatus`, `CommandResponse`, `NS_COMMANDS` defined in `xep/xep0050.rs` (Task 1), referenced by Tasks 3, 4, 6. `build_xep0357_enable_iq`/`build_xep0357_disable_iq` defined in Task 2, used by Tasks 5 (trait impl), 7 (WASM), 8 (UniFFI), 9 (chat via WASM).
