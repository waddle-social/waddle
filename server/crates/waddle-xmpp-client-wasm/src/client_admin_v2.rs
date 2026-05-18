//! Wasm bindings for the V2 admin Spaces + Channels panels.
//!
//! Exposes 14 typed wasm-bindgen methods, one per `urn:waddle:admin:*`
//! ad-hoc command shipped server-side in PRs #685 / #691:
//!
//! Spaces (6):
//! - `admin_spaces_list` — paginated read with optional prefix.
//! - `admin_spaces_create` — name + optional description / icon.
//! - `admin_spaces_update` — patch name / description / icon.
//! - `admin_spaces_delete` — cascade-destroy a space and its channels.
//! - `admin_spaces_members` — paginated read of a space's pubsub roster.
//! - `admin_spaces_set_role` — owner | admin | member | none.
//!
//! Channels (8):
//! - `admin_channels_list` — paginated read with optional space filter.
//! - `admin_channels_create` — name + optional topic / space / is_public.
//! - `admin_channels_update` — patch name / topic / is_public.
//! - `admin_channels_delete` — destroy a MUC room.
//! - `admin_channels_occupants` — live occupancy snapshot.
//! - `admin_channels_affiliations` — persistent affiliation list,
//!   optionally tier-filtered.
//! - `admin_channels_set_affiliation` — owner|admin|member|none|outcast.
//! - `admin_channels_kick` — XEP-0045 §9.1 role→none.
//!
//! All commands follow the V1 `client_admin.rs` shape: serde-typed
//! Args/Result structs cross the JS boundary via `serde_wasm_bindgen`,
//! the XEP-0050 IQ is built with `minidom::Element` (no `format!` per
//! the XML-generation hard rule), and the response form is parsed once
//! into a typed value. JS never sees raw XML.

use super::*;

// ─── Namespace constants (mirrors server::waddle_xmpp::admin::*) ──────

const NS_ADMIN_SPACES_LIST: &str = "urn:waddle:admin:spaces:list:0";
const NS_ADMIN_SPACES_CREATE: &str = "urn:waddle:admin:spaces:create:0";
const NS_ADMIN_SPACES_UPDATE: &str = "urn:waddle:admin:spaces:update:0";
const NS_ADMIN_SPACES_DELETE: &str = "urn:waddle:admin:spaces:delete:0";
const NS_ADMIN_SPACES_MEMBERS: &str = "urn:waddle:admin:spaces:members:0";
const NS_ADMIN_SPACES_SET_ROLE: &str = "urn:waddle:admin:spaces:set-role:0";

const NS_ADMIN_CHANNELS_LIST: &str = "urn:waddle:admin:channels:list:0";
const NS_ADMIN_CHANNELS_CREATE: &str = "urn:waddle:admin:channels:create:0";
const NS_ADMIN_CHANNELS_UPDATE: &str = "urn:waddle:admin:channels:update:0";
const NS_ADMIN_CHANNELS_DELETE: &str = "urn:waddle:admin:channels:delete:0";
const NS_ADMIN_CHANNELS_OCCUPANTS: &str = "urn:waddle:admin:channels:occupants:0";
const NS_ADMIN_CHANNELS_AFFILIATIONS: &str = "urn:waddle:admin:channels:affiliations:0";
const NS_ADMIN_CHANNELS_SET_AFFILIATION: &str = "urn:waddle:admin:channels:set-affiliation:0";
const NS_ADMIN_CHANNELS_KICK: &str = "urn:waddle:admin:channels:kick:0";

const NS_XDATA: &str = "jabber:x:data";

// ─── Typed Args / Result structs ──────────────────────────────────────

#[derive(Debug, Serialize, Deserialize, Default)]
pub struct WaddleAdminSpacesListArgs {
    pub prefix: Option<String>,
    pub page_size: Option<u32>,
    pub after_cursor: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct WaddleAdminSpaceListEntry {
    pub space_jid: String,
    pub name: String,
    pub description: Option<String>,
    pub icon_url: Option<String>,
    pub channel_count: u32,
    pub member_count: u32,
}

#[derive(Debug, Serialize, Deserialize, Default)]
pub struct WaddleAdminSpacesListResult {
    pub entries: Vec<WaddleAdminSpaceListEntry>,
    pub next_cursor: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct WaddleAdminSpacesCreateArgs {
    pub name: String,
    pub description: Option<String>,
    pub icon_url: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct WaddleAdminSpaceRef {
    pub space_jid: String,
    pub name: String,
    pub description: Option<String>,
    pub icon_url: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct WaddleAdminSpacesUpdateArgs {
    pub space_jid: String,
    pub name: Option<String>,
    pub description: Option<String>,
    pub icon_url: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct WaddleAdminSpacesDeleteArgs {
    pub space_jid: String,
    /// MUST be the literal string "yes"; mirrors the server-side guard.
    pub confirm: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct WaddleAdminSpacesMembersArgs {
    pub space_jid: String,
    pub page_size: Option<u32>,
    pub after_cursor: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct WaddleAdminSpaceMemberEntry {
    pub jid: String,
    /// `owner` | `admin` | `member` | `none`.
    pub role: String,
}

#[derive(Debug, Serialize, Deserialize, Default)]
pub struct WaddleAdminSpacesMembersResult {
    pub entries: Vec<WaddleAdminSpaceMemberEntry>,
    pub next_cursor: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct WaddleAdminSpacesSetRoleArgs {
    pub space_jid: String,
    pub member_jid: String,
    /// `owner` | `admin` | `member` | `none`.
    pub role: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct WaddleAdminSpacesSetRoleResult {
    pub member_jid: String,
    pub role: String,
}

// ── Channels typed payloads ──────────────────────────────────────────

#[derive(Debug, Serialize, Deserialize, Default)]
pub struct WaddleAdminChannelsListArgs {
    pub space_jid: Option<String>,
    pub prefix: Option<String>,
    pub page_size: Option<u32>,
    pub after_cursor: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct WaddleAdminChannelListEntry {
    pub channel_jid: String,
    pub name: String,
    pub topic: Option<String>,
    pub is_public: bool,
    pub members_only: bool,
    pub occupant_count: u32,
    pub owner_count: u32,
    pub admin_count: u32,
    pub member_count: u32,
    pub outcast_count: u32,
}

#[derive(Debug, Serialize, Deserialize, Default)]
pub struct WaddleAdminChannelsListResult {
    pub entries: Vec<WaddleAdminChannelListEntry>,
    pub next_cursor: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct WaddleAdminChannelsCreateArgs {
    pub name: String,
    pub topic: Option<String>,
    pub space_jid: Option<String>,
    /// Defaults to `true` per the V2 spec; the client wrapper passes
    /// this through verbatim. The server enforces the same default
    /// when the field is absent.
    pub is_public: Option<bool>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct WaddleAdminChannelRef {
    pub channel_jid: String,
    pub name: String,
    pub topic: Option<String>,
    pub is_public: bool,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct WaddleAdminChannelsUpdateArgs {
    pub channel_jid: String,
    pub name: Option<String>,
    pub topic: Option<String>,
    pub is_public: Option<bool>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct WaddleAdminChannelsDeleteArgs {
    pub channel_jid: String,
    /// MUST be the literal string "yes".
    pub confirm: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct WaddleAdminChannelsOccupantsArgs {
    pub channel_jid: String,
    pub page_size: Option<u32>,
    pub after_cursor: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct WaddleAdminChannelOccupantEntry {
    pub nick: String,
    pub real_jid: String,
    /// `moderator` | `participant` | `visitor` | `none`.
    pub role: String,
    /// `owner` | `admin` | `member` | `none` | `outcast`.
    pub affiliation: String,
}

#[derive(Debug, Serialize, Deserialize, Default)]
pub struct WaddleAdminChannelsOccupantsResult {
    pub entries: Vec<WaddleAdminChannelOccupantEntry>,
    pub next_cursor: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct WaddleAdminChannelsAffiliationsArgs {
    pub channel_jid: String,
    /// Optional tier filter: `owner` | `admin` | `member` | `outcast` | `none`.
    pub filter: Option<String>,
    pub page_size: Option<u32>,
    pub after_cursor: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct WaddleAdminChannelAffiliationEntry {
    pub jid: String,
    pub affiliation: String,
    pub reason: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, Default)]
pub struct WaddleAdminChannelsAffiliationsResult {
    pub entries: Vec<WaddleAdminChannelAffiliationEntry>,
    pub next_cursor: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct WaddleAdminChannelsSetAffiliationArgs {
    pub channel_jid: String,
    pub member_jid: String,
    pub affiliation: String,
    pub reason: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct WaddleAdminChannelsSetAffiliationResult {
    pub member_jid: String,
    pub affiliation: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct WaddleAdminChannelsKickArgs {
    pub channel_jid: String,
    pub occupant_jid: String,
    pub reason: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct WaddleAdminChannelsKickResult {
    pub occupant_jid: String,
}

// ─── #[wasm_bindgen] methods ──────────────────────────────────────────

#[wasm_bindgen]
impl WaddleClient {
    pub fn admin_spaces_list(&self, args: JsValue) -> Promise {
        let inner = self.inner.clone();
        future_to_promise(async move {
            let parsed: WaddleAdminSpacesListArgs =
                serde_wasm_bindgen::from_value(args).map_err(|e| js_error(e.to_string()))?;
            let domain = caller_domain(&inner);
            let iq = build_spaces_list_iq(&domain, &parsed);
            let result = send_iq_command(inner, iq).await?;
            let page = parse_spaces_list_result(&result)?;
            to_js_value(&page)
        })
    }

    pub fn admin_spaces_create(&self, args: JsValue) -> Promise {
        let inner = self.inner.clone();
        future_to_promise(async move {
            let parsed: WaddleAdminSpacesCreateArgs =
                serde_wasm_bindgen::from_value(args).map_err(|e| js_error(e.to_string()))?;
            let domain = caller_domain(&inner);
            let iq = build_spaces_create_iq(&domain, &parsed);
            let result = send_iq_command(inner, iq).await?;
            let space = parse_space_ref_result(&result)?;
            to_js_value(&space)
        })
    }

    pub fn admin_spaces_update(&self, args: JsValue) -> Promise {
        let inner = self.inner.clone();
        future_to_promise(async move {
            let parsed: WaddleAdminSpacesUpdateArgs =
                serde_wasm_bindgen::from_value(args).map_err(|e| js_error(e.to_string()))?;
            let domain = caller_domain(&inner);
            let iq = build_spaces_update_iq(&domain, &parsed);
            let result = send_iq_command(inner, iq).await?;
            let space = parse_space_ref_result(&result)?;
            to_js_value(&space)
        })
    }

    pub fn admin_spaces_delete(&self, args: JsValue) -> Promise {
        let inner = self.inner.clone();
        future_to_promise(async move {
            let parsed: WaddleAdminSpacesDeleteArgs =
                serde_wasm_bindgen::from_value(args).map_err(|e| js_error(e.to_string()))?;
            let domain = caller_domain(&inner);
            let iq = build_spaces_delete_iq(&domain, &parsed);
            let _ = send_iq_command(inner, iq).await?;
            Ok(JsValue::TRUE)
        })
    }

    pub fn admin_spaces_members(&self, args: JsValue) -> Promise {
        let inner = self.inner.clone();
        future_to_promise(async move {
            let parsed: WaddleAdminSpacesMembersArgs =
                serde_wasm_bindgen::from_value(args).map_err(|e| js_error(e.to_string()))?;
            let domain = caller_domain(&inner);
            let iq = build_spaces_members_iq(&domain, &parsed);
            let result = send_iq_command(inner, iq).await?;
            let page = parse_spaces_members_result(&result)?;
            to_js_value(&page)
        })
    }

    pub fn admin_spaces_set_role(&self, args: JsValue) -> Promise {
        let inner = self.inner.clone();
        future_to_promise(async move {
            let parsed: WaddleAdminSpacesSetRoleArgs =
                serde_wasm_bindgen::from_value(args).map_err(|e| js_error(e.to_string()))?;
            let domain = caller_domain(&inner);
            let iq = build_spaces_set_role_iq(&domain, &parsed);
            let result = send_iq_command(inner, iq).await?;
            let payload = parse_spaces_set_role_result(&result)?;
            to_js_value(&payload)
        })
    }

    pub fn admin_channels_list(&self, args: JsValue) -> Promise {
        let inner = self.inner.clone();
        future_to_promise(async move {
            let parsed: WaddleAdminChannelsListArgs =
                serde_wasm_bindgen::from_value(args).map_err(|e| js_error(e.to_string()))?;
            let domain = caller_domain(&inner);
            let iq = build_channels_list_iq(&domain, &parsed);
            let result = send_iq_command(inner, iq).await?;
            let page = parse_channels_list_result(&result)?;
            to_js_value(&page)
        })
    }

    pub fn admin_channels_create(&self, args: JsValue) -> Promise {
        let inner = self.inner.clone();
        future_to_promise(async move {
            let parsed: WaddleAdminChannelsCreateArgs =
                serde_wasm_bindgen::from_value(args).map_err(|e| js_error(e.to_string()))?;
            let domain = caller_domain(&inner);
            let iq = build_channels_create_iq(&domain, &parsed);
            let result = send_iq_command(inner, iq).await?;
            let channel = parse_channel_ref_result(&result)?;
            to_js_value(&channel)
        })
    }

    pub fn admin_channels_update(&self, args: JsValue) -> Promise {
        let inner = self.inner.clone();
        future_to_promise(async move {
            let parsed: WaddleAdminChannelsUpdateArgs =
                serde_wasm_bindgen::from_value(args).map_err(|e| js_error(e.to_string()))?;
            let domain = caller_domain(&inner);
            let iq = build_channels_update_iq(&domain, &parsed);
            let result = send_iq_command(inner, iq).await?;
            let channel = parse_channel_ref_result(&result)?;
            to_js_value(&channel)
        })
    }

    pub fn admin_channels_delete(&self, args: JsValue) -> Promise {
        let inner = self.inner.clone();
        future_to_promise(async move {
            let parsed: WaddleAdminChannelsDeleteArgs =
                serde_wasm_bindgen::from_value(args).map_err(|e| js_error(e.to_string()))?;
            let domain = caller_domain(&inner);
            let iq = build_channels_delete_iq(&domain, &parsed);
            let _ = send_iq_command(inner, iq).await?;
            Ok(JsValue::TRUE)
        })
    }

    pub fn admin_channels_occupants(&self, args: JsValue) -> Promise {
        let inner = self.inner.clone();
        future_to_promise(async move {
            let parsed: WaddleAdminChannelsOccupantsArgs =
                serde_wasm_bindgen::from_value(args).map_err(|e| js_error(e.to_string()))?;
            let domain = caller_domain(&inner);
            let iq = build_channels_occupants_iq(&domain, &parsed);
            let result = send_iq_command(inner, iq).await?;
            let page = parse_channels_occupants_result(&result)?;
            to_js_value(&page)
        })
    }

    pub fn admin_channels_affiliations(&self, args: JsValue) -> Promise {
        let inner = self.inner.clone();
        future_to_promise(async move {
            let parsed: WaddleAdminChannelsAffiliationsArgs =
                serde_wasm_bindgen::from_value(args).map_err(|e| js_error(e.to_string()))?;
            let domain = caller_domain(&inner);
            let iq = build_channels_affiliations_iq(&domain, &parsed);
            let result = send_iq_command(inner, iq).await?;
            let page = parse_channels_affiliations_result(&result)?;
            to_js_value(&page)
        })
    }

    pub fn admin_channels_set_affiliation(&self, args: JsValue) -> Promise {
        let inner = self.inner.clone();
        future_to_promise(async move {
            let parsed: WaddleAdminChannelsSetAffiliationArgs =
                serde_wasm_bindgen::from_value(args).map_err(|e| js_error(e.to_string()))?;
            let domain = caller_domain(&inner);
            let iq = build_channels_set_affiliation_iq(&domain, &parsed);
            let result = send_iq_command(inner, iq).await?;
            let payload = parse_channels_set_affiliation_result(&result)?;
            to_js_value(&payload)
        })
    }

    pub fn admin_channels_kick(&self, args: JsValue) -> Promise {
        let inner = self.inner.clone();
        future_to_promise(async move {
            let parsed: WaddleAdminChannelsKickArgs =
                serde_wasm_bindgen::from_value(args).map_err(|e| js_error(e.to_string()))?;
            let domain = caller_domain(&inner);
            let iq = build_channels_kick_iq(&domain, &parsed);
            let result = send_iq_command(inner, iq).await?;
            let payload = parse_channels_kick_result(&result)?;
            to_js_value(&payload)
        })
    }
}

// ─── IQ helpers ───────────────────────────────────────────────────────

fn caller_domain(inner: &Rc<RefCell<WaddleClientInner>>) -> String {
    let stored = inner.borrow().config.clone();
    jid_domain(&stored.jid)
}

fn text_single_field(var: &str, value: &str) -> Element {
    Element::builder("field", NS_XDATA)
        .attr("var", var)
        .attr("type", "text-single")
        .append(Element::builder("value", NS_XDATA).append(value).build())
        .build()
}

fn boolean_field(var: &str, value: bool) -> Element {
    Element::builder("field", NS_XDATA)
        .attr("var", var)
        .attr("type", "boolean")
        .append(
            Element::builder("value", NS_XDATA)
                .append(if value { "1" } else { "0" })
                .build(),
        )
        .build()
}

fn jid_field(var: &str, value: &str) -> Element {
    Element::builder("field", NS_XDATA)
        .attr("var", var)
        .attr("type", "jid-single")
        .append(Element::builder("value", NS_XDATA).append(value).build())
        .build()
}

fn submit_form_with_type(form_type: &str) -> minidom::element::ElementBuilder {
    Element::builder("x", NS_XDATA)
        .attr("type", "submit")
        .append(
            Element::builder("field", NS_XDATA)
                .attr("var", "FORM_TYPE")
                .attr("type", "hidden")
                .append(
                    Element::builder("value", NS_XDATA)
                        .append(form_type)
                        .build(),
                )
                .build(),
        )
}

fn wrap_command_iq(server_domain: &str, node: &str, form: Element) -> Element {
    let command = Element::builder("command", NS_ADHOC_COMMANDS)
        .attr("node", node)
        .attr("action", "execute")
        .append(form)
        .build();
    Element::builder("iq", NS_CLIENT)
        .attr("type", "set")
        .attr("id", uuid::Uuid::new_v4().to_string())
        .attr("to", server_domain)
        .append(command)
        .build()
}

// ─── IQ builders ──────────────────────────────────────────────────────

fn build_spaces_list_iq(server_domain: &str, args: &WaddleAdminSpacesListArgs) -> Element {
    let mut form = submit_form_with_type(NS_ADMIN_SPACES_LIST);
    if let Some(prefix) = args.prefix.as_deref() {
        form = form.append(text_single_field("prefix", prefix));
    }
    if let Some(page_size) = args.page_size {
        form = form.append(text_single_field("page_size", &page_size.to_string()));
    }
    if let Some(after_cursor) = args.after_cursor.as_deref() {
        form = form.append(text_single_field("after_cursor", after_cursor));
    }
    wrap_command_iq(server_domain, NS_ADMIN_SPACES_LIST, form.build())
}

fn build_spaces_create_iq(server_domain: &str, args: &WaddleAdminSpacesCreateArgs) -> Element {
    let mut form =
        submit_form_with_type(NS_ADMIN_SPACES_CREATE).append(text_single_field("name", &args.name));
    if let Some(description) = args.description.as_deref() {
        form = form.append(text_single_field("description", description));
    }
    if let Some(icon_url) = args.icon_url.as_deref() {
        form = form.append(text_single_field("icon_url", icon_url));
    }
    wrap_command_iq(server_domain, NS_ADMIN_SPACES_CREATE, form.build())
}

fn build_spaces_update_iq(server_domain: &str, args: &WaddleAdminSpacesUpdateArgs) -> Element {
    let mut form = submit_form_with_type(NS_ADMIN_SPACES_UPDATE)
        .append(jid_field("space_jid", &args.space_jid));
    if let Some(name) = args.name.as_deref() {
        form = form.append(text_single_field("name", name));
    }
    if let Some(description) = args.description.as_deref() {
        form = form.append(text_single_field("description", description));
    }
    if let Some(icon_url) = args.icon_url.as_deref() {
        form = form.append(text_single_field("icon_url", icon_url));
    }
    wrap_command_iq(server_domain, NS_ADMIN_SPACES_UPDATE, form.build())
}

fn build_spaces_delete_iq(server_domain: &str, args: &WaddleAdminSpacesDeleteArgs) -> Element {
    let form = submit_form_with_type(NS_ADMIN_SPACES_DELETE)
        .append(jid_field("space_jid", &args.space_jid))
        .append(text_single_field("confirm", &args.confirm));
    wrap_command_iq(server_domain, NS_ADMIN_SPACES_DELETE, form.build())
}

fn build_spaces_members_iq(server_domain: &str, args: &WaddleAdminSpacesMembersArgs) -> Element {
    let mut form = submit_form_with_type(NS_ADMIN_SPACES_MEMBERS)
        .append(jid_field("space_jid", &args.space_jid));
    if let Some(page_size) = args.page_size {
        form = form.append(text_single_field("page_size", &page_size.to_string()));
    }
    if let Some(after_cursor) = args.after_cursor.as_deref() {
        form = form.append(text_single_field("after_cursor", after_cursor));
    }
    wrap_command_iq(server_domain, NS_ADMIN_SPACES_MEMBERS, form.build())
}

fn build_spaces_set_role_iq(server_domain: &str, args: &WaddleAdminSpacesSetRoleArgs) -> Element {
    let form = submit_form_with_type(NS_ADMIN_SPACES_SET_ROLE)
        .append(jid_field("space_jid", &args.space_jid))
        .append(jid_field("member_jid", &args.member_jid))
        .append(text_single_field("role", &args.role));
    wrap_command_iq(server_domain, NS_ADMIN_SPACES_SET_ROLE, form.build())
}

fn build_channels_list_iq(server_domain: &str, args: &WaddleAdminChannelsListArgs) -> Element {
    let mut form = submit_form_with_type(NS_ADMIN_CHANNELS_LIST);
    if let Some(space_jid) = args.space_jid.as_deref() {
        form = form.append(jid_field("space_jid", space_jid));
    }
    if let Some(prefix) = args.prefix.as_deref() {
        form = form.append(text_single_field("prefix", prefix));
    }
    if let Some(page_size) = args.page_size {
        form = form.append(text_single_field("page_size", &page_size.to_string()));
    }
    if let Some(after_cursor) = args.after_cursor.as_deref() {
        form = form.append(text_single_field("after_cursor", after_cursor));
    }
    wrap_command_iq(server_domain, NS_ADMIN_CHANNELS_LIST, form.build())
}

fn build_channels_create_iq(server_domain: &str, args: &WaddleAdminChannelsCreateArgs) -> Element {
    let mut form = submit_form_with_type(NS_ADMIN_CHANNELS_CREATE)
        .append(text_single_field("name", &args.name));
    if let Some(topic) = args.topic.as_deref() {
        form = form.append(text_single_field("topic", topic));
    }
    if let Some(space_jid) = args.space_jid.as_deref() {
        form = form.append(jid_field("space_jid", space_jid));
    }
    if let Some(is_public) = args.is_public {
        form = form.append(boolean_field("is_public", is_public));
    }
    wrap_command_iq(server_domain, NS_ADMIN_CHANNELS_CREATE, form.build())
}

fn build_channels_update_iq(server_domain: &str, args: &WaddleAdminChannelsUpdateArgs) -> Element {
    let mut form = submit_form_with_type(NS_ADMIN_CHANNELS_UPDATE)
        .append(jid_field("channel_jid", &args.channel_jid));
    if let Some(name) = args.name.as_deref() {
        form = form.append(text_single_field("name", name));
    }
    if let Some(topic) = args.topic.as_deref() {
        form = form.append(text_single_field("topic", topic));
    }
    if let Some(is_public) = args.is_public {
        form = form.append(boolean_field("is_public", is_public));
    }
    wrap_command_iq(server_domain, NS_ADMIN_CHANNELS_UPDATE, form.build())
}

fn build_channels_delete_iq(server_domain: &str, args: &WaddleAdminChannelsDeleteArgs) -> Element {
    let form = submit_form_with_type(NS_ADMIN_CHANNELS_DELETE)
        .append(jid_field("channel_jid", &args.channel_jid))
        .append(text_single_field("confirm", &args.confirm));
    wrap_command_iq(server_domain, NS_ADMIN_CHANNELS_DELETE, form.build())
}

fn build_channels_occupants_iq(
    server_domain: &str,
    args: &WaddleAdminChannelsOccupantsArgs,
) -> Element {
    let mut form = submit_form_with_type(NS_ADMIN_CHANNELS_OCCUPANTS)
        .append(jid_field("channel_jid", &args.channel_jid));
    if let Some(page_size) = args.page_size {
        form = form.append(text_single_field("page_size", &page_size.to_string()));
    }
    if let Some(after_cursor) = args.after_cursor.as_deref() {
        form = form.append(text_single_field("after_cursor", after_cursor));
    }
    wrap_command_iq(server_domain, NS_ADMIN_CHANNELS_OCCUPANTS, form.build())
}

fn build_channels_affiliations_iq(
    server_domain: &str,
    args: &WaddleAdminChannelsAffiliationsArgs,
) -> Element {
    let mut form = submit_form_with_type(NS_ADMIN_CHANNELS_AFFILIATIONS)
        .append(jid_field("channel_jid", &args.channel_jid));
    if let Some(filter) = args.filter.as_deref() {
        form = form.append(text_single_field("filter", filter));
    }
    if let Some(page_size) = args.page_size {
        form = form.append(text_single_field("page_size", &page_size.to_string()));
    }
    if let Some(after_cursor) = args.after_cursor.as_deref() {
        form = form.append(text_single_field("after_cursor", after_cursor));
    }
    wrap_command_iq(server_domain, NS_ADMIN_CHANNELS_AFFILIATIONS, form.build())
}

fn build_channels_set_affiliation_iq(
    server_domain: &str,
    args: &WaddleAdminChannelsSetAffiliationArgs,
) -> Element {
    let mut form = submit_form_with_type(NS_ADMIN_CHANNELS_SET_AFFILIATION)
        .append(jid_field("channel_jid", &args.channel_jid))
        .append(jid_field("member_jid", &args.member_jid))
        .append(text_single_field("affiliation", &args.affiliation));
    if let Some(reason) = args.reason.as_deref() {
        form = form.append(text_single_field("reason", reason));
    }
    wrap_command_iq(
        server_domain,
        NS_ADMIN_CHANNELS_SET_AFFILIATION,
        form.build(),
    )
}

fn build_channels_kick_iq(server_domain: &str, args: &WaddleAdminChannelsKickArgs) -> Element {
    let mut form = submit_form_with_type(NS_ADMIN_CHANNELS_KICK)
        .append(jid_field("channel_jid", &args.channel_jid))
        .append(jid_field("occupant_jid", &args.occupant_jid));
    if let Some(reason) = args.reason.as_deref() {
        form = form.append(text_single_field("reason", reason));
    }
    wrap_command_iq(server_domain, NS_ADMIN_CHANNELS_KICK, form.build())
}

// ─── Response parsers ─────────────────────────────────────────────────

fn command_form(iq: &Element) -> Result<&Element, JsValue> {
    let command = iq
        .get_child("command", NS_ADHOC_COMMANDS)
        .ok_or_else(|| js_error("admin response missing <command/>"))?;
    command
        .get_child("x", NS_XDATA)
        .ok_or_else(|| js_error("admin response missing <x xmlns='jabber:x:data'/>"))
}

fn maybe_command_form(iq: &Element) -> Option<&Element> {
    iq.get_child("command", NS_ADHOC_COMMANDS)
        .and_then(|cmd| cmd.get_child("x", NS_XDATA))
}

fn field_text(item: &Element, var: &str) -> Option<String> {
    item.children()
        .filter(|c| c.name() == "field" && c.attr("var") == Some(var))
        .find_map(|field| field.get_child("value", NS_XDATA).map(|value| value.text()))
}

fn field_text_required(item: &Element, var: &str) -> String {
    field_text(item, var).unwrap_or_default()
}

fn field_bool(item: &Element, var: &str) -> bool {
    field_text(item, var)
        .map(|raw| matches!(raw.as_str(), "1" | "true"))
        .unwrap_or(false)
}

fn field_u32(item: &Element, var: &str) -> u32 {
    field_text(item, var)
        .and_then(|raw| raw.parse::<u32>().ok())
        .unwrap_or(0)
}

fn top_level_next_cursor(form: &Element) -> Option<String> {
    form.children()
        .filter(|c| c.name() == "field" && c.attr("var") == Some("next_cursor"))
        .find_map(|field| {
            field
                .get_child("value", NS_XDATA)
                .map(|value| value.text())
                .filter(|s| !s.is_empty())
        })
}

fn parse_spaces_list_result(iq: &Element) -> Result<WaddleAdminSpacesListResult, JsValue> {
    let Some(form) = maybe_command_form(iq) else {
        return Ok(WaddleAdminSpacesListResult::default());
    };
    let mut entries = Vec::new();
    for item in form.children().filter(|c| c.name() == "item") {
        entries.push(WaddleAdminSpaceListEntry {
            space_jid: field_text_required(item, "space_jid"),
            name: field_text_required(item, "name"),
            description: field_text(item, "description").filter(|s| !s.is_empty()),
            icon_url: field_text(item, "icon_url").filter(|s| !s.is_empty()),
            channel_count: field_u32(item, "channel_count"),
            member_count: field_u32(item, "member_count"),
        });
    }
    Ok(WaddleAdminSpacesListResult {
        entries,
        next_cursor: top_level_next_cursor(form),
    })
}

fn parse_space_ref_result(iq: &Element) -> Result<WaddleAdminSpaceRef, JsValue> {
    let form = command_form(iq)?;
    Ok(WaddleAdminSpaceRef {
        space_jid: top_level_field_text(form, "space_jid"),
        name: top_level_field_text(form, "name"),
        description: top_level_field_text_opt(form, "description"),
        icon_url: top_level_field_text_opt(form, "icon_url"),
    })
}

fn top_level_field_text(form: &Element, var: &str) -> String {
    form.children()
        .filter(|c| c.name() == "field" && c.attr("var") == Some(var))
        .find_map(|field| field.get_child("value", NS_XDATA).map(|value| value.text()))
        .unwrap_or_default()
}

fn top_level_field_text_opt(form: &Element, var: &str) -> Option<String> {
    let text = top_level_field_text(form, var);
    if text.is_empty() {
        None
    } else {
        Some(text)
    }
}

fn parse_spaces_members_result(iq: &Element) -> Result<WaddleAdminSpacesMembersResult, JsValue> {
    let Some(form) = maybe_command_form(iq) else {
        return Ok(WaddleAdminSpacesMembersResult::default());
    };
    let mut entries = Vec::new();
    for item in form.children().filter(|c| c.name() == "item") {
        entries.push(WaddleAdminSpaceMemberEntry {
            jid: field_text_required(item, "jid"),
            role: field_text_required(item, "role"),
        });
    }
    Ok(WaddleAdminSpacesMembersResult {
        entries,
        next_cursor: top_level_next_cursor(form),
    })
}

fn parse_spaces_set_role_result(iq: &Element) -> Result<WaddleAdminSpacesSetRoleResult, JsValue> {
    let form = command_form(iq)?;
    Ok(WaddleAdminSpacesSetRoleResult {
        member_jid: top_level_field_text(form, "member_jid"),
        role: top_level_field_text(form, "role"),
    })
}

fn parse_channels_list_result(iq: &Element) -> Result<WaddleAdminChannelsListResult, JsValue> {
    let Some(form) = maybe_command_form(iq) else {
        return Ok(WaddleAdminChannelsListResult::default());
    };
    let mut entries = Vec::new();
    for item in form.children().filter(|c| c.name() == "item") {
        entries.push(WaddleAdminChannelListEntry {
            channel_jid: field_text_required(item, "channel_jid"),
            name: field_text_required(item, "name"),
            topic: field_text(item, "topic").filter(|s| !s.is_empty()),
            is_public: field_bool(item, "is_public"),
            members_only: field_bool(item, "members_only"),
            occupant_count: field_u32(item, "occupant_count"),
            owner_count: field_u32(item, "owner_count"),
            admin_count: field_u32(item, "admin_count"),
            member_count: field_u32(item, "member_count"),
            outcast_count: field_u32(item, "outcast_count"),
        });
    }
    Ok(WaddleAdminChannelsListResult {
        entries,
        next_cursor: top_level_next_cursor(form),
    })
}

fn parse_channel_ref_result(iq: &Element) -> Result<WaddleAdminChannelRef, JsValue> {
    let form = command_form(iq)?;
    Ok(WaddleAdminChannelRef {
        channel_jid: top_level_field_text(form, "channel_jid"),
        name: top_level_field_text(form, "name"),
        topic: top_level_field_text_opt(form, "topic"),
        is_public: matches!(
            top_level_field_text(form, "is_public").as_str(),
            "1" | "true"
        ),
    })
}

fn parse_channels_occupants_result(
    iq: &Element,
) -> Result<WaddleAdminChannelsOccupantsResult, JsValue> {
    let Some(form) = maybe_command_form(iq) else {
        return Ok(WaddleAdminChannelsOccupantsResult::default());
    };
    let mut entries = Vec::new();
    for item in form.children().filter(|c| c.name() == "item") {
        entries.push(WaddleAdminChannelOccupantEntry {
            nick: field_text_required(item, "nick"),
            real_jid: field_text_required(item, "real_jid"),
            role: field_text_required(item, "role"),
            affiliation: field_text_required(item, "affiliation"),
        });
    }
    Ok(WaddleAdminChannelsOccupantsResult {
        entries,
        next_cursor: top_level_next_cursor(form),
    })
}

fn parse_channels_affiliations_result(
    iq: &Element,
) -> Result<WaddleAdminChannelsAffiliationsResult, JsValue> {
    let Some(form) = maybe_command_form(iq) else {
        return Ok(WaddleAdminChannelsAffiliationsResult::default());
    };
    let mut entries = Vec::new();
    for item in form.children().filter(|c| c.name() == "item") {
        entries.push(WaddleAdminChannelAffiliationEntry {
            jid: field_text_required(item, "jid"),
            affiliation: field_text_required(item, "affiliation"),
            reason: field_text(item, "reason").filter(|s| !s.is_empty()),
        });
    }
    Ok(WaddleAdminChannelsAffiliationsResult {
        entries,
        next_cursor: top_level_next_cursor(form),
    })
}

fn parse_channels_set_affiliation_result(
    iq: &Element,
) -> Result<WaddleAdminChannelsSetAffiliationResult, JsValue> {
    let form = command_form(iq)?;
    Ok(WaddleAdminChannelsSetAffiliationResult {
        member_jid: top_level_field_text(form, "member_jid"),
        affiliation: top_level_field_text(form, "affiliation"),
    })
}

fn parse_channels_kick_result(iq: &Element) -> Result<WaddleAdminChannelsKickResult, JsValue> {
    let form = command_form(iq)?;
    Ok(WaddleAdminChannelsKickResult {
        occupant_jid: top_level_field_text(form, "occupant_jid"),
    })
}

// ─── Tests ────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn wrap_response(node: &str, form_inner: &str) -> Element {
        let raw = format!(
            r#"<iq xmlns='jabber:client' type='result' id='test'><command xmlns='http://jabber.org/protocol/commands' node='{node}' status='completed'><x xmlns='jabber:x:data' type='result'><field type="hidden" var="FORM_TYPE"><value>{node}</value></field>{form_inner}</x></command></iq>"#
        );
        raw.parse().expect("parse iq")
    }

    #[test]
    fn parse_spaces_list_handles_empty_form() {
        let iq = wrap_response(NS_ADMIN_SPACES_LIST, "");
        let result = parse_spaces_list_result(&iq).expect("ok");
        assert!(result.entries.is_empty());
        assert!(result.next_cursor.is_none());
    }

    #[test]
    fn parse_spaces_list_extracts_counts_and_cursor() {
        let item = r#"<item>
            <field var="space_jid"><value>eng@spaces.localhost</value></field>
            <field var="name"><value>Engineering</value></field>
            <field var="description"><value>Hack stuff</value></field>
            <field var="icon_url"><value></value></field>
            <field var="channel_count"><value>3</value></field>
            <field var="member_count"><value>5</value></field>
        </item>"#;
        let cursor = r#"<field var="next_cursor" type="text-single"><value>eng@spaces.localhost</value></field>"#;
        let iq = wrap_response(NS_ADMIN_SPACES_LIST, &format!("{item}{cursor}"));
        let result = parse_spaces_list_result(&iq).expect("ok");
        assert_eq!(result.entries.len(), 1);
        let entry = &result.entries[0];
        assert_eq!(entry.space_jid, "eng@spaces.localhost");
        assert_eq!(entry.name, "Engineering");
        assert_eq!(entry.description.as_deref(), Some("Hack stuff"));
        assert!(entry.icon_url.is_none());
        assert_eq!(entry.channel_count, 3);
        assert_eq!(entry.member_count, 5);
        assert_eq!(result.next_cursor.as_deref(), Some("eng@spaces.localhost"));
    }

    #[test]
    fn parse_space_ref_round_trip() {
        let inner = r#"<field var="space_jid"><value>eng@spaces.localhost</value></field>
            <field var="name"><value>Engineering</value></field>
            <field var="description"><value>Hack stuff</value></field>"#;
        let iq = wrap_response(NS_ADMIN_SPACES_CREATE, inner);
        let space = parse_space_ref_result(&iq).expect("ok");
        assert_eq!(space.space_jid, "eng@spaces.localhost");
        assert_eq!(space.name, "Engineering");
        assert_eq!(space.description.as_deref(), Some("Hack stuff"));
        assert!(space.icon_url.is_none());
    }

    #[test]
    fn parse_spaces_members_extracts_roles() {
        let inner = r#"<item>
            <field var="jid"><value>alice@localhost</value></field>
            <field var="role"><value>owner</value></field>
        </item>
        <item>
            <field var="jid"><value>bob@localhost</value></field>
            <field var="role"><value>member</value></field>
        </item>"#;
        let iq = wrap_response(NS_ADMIN_SPACES_MEMBERS, inner);
        let result = parse_spaces_members_result(&iq).expect("ok");
        assert_eq!(result.entries.len(), 2);
        assert_eq!(result.entries[0].role, "owner");
        assert_eq!(result.entries[1].jid, "bob@localhost");
    }

    #[test]
    fn parse_channels_list_extracts_booleans_and_counts() {
        let item = r#"<item>
            <field var="channel_jid"><value>general@muc.localhost</value></field>
            <field var="name"><value>General</value></field>
            <field var="topic"><value>All things</value></field>
            <field var="is_public" type="boolean"><value>1</value></field>
            <field var="members_only" type="boolean"><value>0</value></field>
            <field var="occupant_count"><value>7</value></field>
            <field var="owner_count"><value>1</value></field>
            <field var="admin_count"><value>2</value></field>
            <field var="member_count"><value>3</value></field>
            <field var="outcast_count"><value>0</value></field>
        </item>"#;
        let iq = wrap_response(NS_ADMIN_CHANNELS_LIST, item);
        let result = parse_channels_list_result(&iq).expect("ok");
        assert_eq!(result.entries.len(), 1);
        let entry = &result.entries[0];
        assert!(entry.is_public);
        assert!(!entry.members_only);
        assert_eq!(entry.occupant_count, 7);
        assert_eq!(entry.owner_count, 1);
        assert_eq!(entry.admin_count, 2);
        assert_eq!(entry.member_count, 3);
    }

    #[test]
    fn parse_channels_occupants_extracts_role_and_affiliation() {
        let inner = r#"<item>
            <field var="nick"><value>alice</value></field>
            <field var="real_jid"><value>alice@localhost/web</value></field>
            <field var="role"><value>moderator</value></field>
            <field var="affiliation"><value>owner</value></field>
        </item>"#;
        let iq = wrap_response(NS_ADMIN_CHANNELS_OCCUPANTS, inner);
        let result = parse_channels_occupants_result(&iq).expect("ok");
        assert_eq!(result.entries.len(), 1);
        let entry = &result.entries[0];
        assert_eq!(entry.nick, "alice");
        assert_eq!(entry.role, "moderator");
        assert_eq!(entry.affiliation, "owner");
    }

    #[test]
    fn build_spaces_list_iq_omits_unset_fields() {
        let args = WaddleAdminSpacesListArgs::default();
        let iq = build_spaces_list_iq("localhost", &args);
        assert_eq!(iq.name(), "iq");
        assert_eq!(iq.attr("to"), Some("localhost"));
        let cmd = iq.get_child("command", NS_ADHOC_COMMANDS).expect("command");
        assert_eq!(cmd.attr("node"), Some(NS_ADMIN_SPACES_LIST));
        let form = cmd.get_child("x", NS_XDATA).expect("form");
        let var_names: Vec<&str> = form
            .children()
            .filter(|c| c.name() == "field")
            .filter_map(|f| f.attr("var"))
            .collect();
        assert_eq!(var_names, vec!["FORM_TYPE"]);
    }

    #[test]
    fn build_channels_create_iq_includes_all_optional_fields() {
        let args = WaddleAdminChannelsCreateArgs {
            name: "general".to_string(),
            topic: Some("All things".to_string()),
            space_jid: Some("eng@spaces.localhost".to_string()),
            is_public: Some(true),
        };
        let iq = build_channels_create_iq("localhost", &args);
        let form = iq
            .get_child("command", NS_ADHOC_COMMANDS)
            .and_then(|c| c.get_child("x", NS_XDATA))
            .expect("form");
        let var_names: Vec<&str> = form
            .children()
            .filter(|c| c.name() == "field")
            .filter_map(|f| f.attr("var"))
            .collect();
        assert!(var_names.contains(&"name"));
        assert!(var_names.contains(&"topic"));
        assert!(var_names.contains(&"space_jid"));
        assert!(var_names.contains(&"is_public"));
    }

    #[test]
    fn build_spaces_delete_iq_carries_confirm() {
        let args = WaddleAdminSpacesDeleteArgs {
            space_jid: "eng@spaces.localhost".to_string(),
            confirm: "yes".to_string(),
        };
        let iq = build_spaces_delete_iq("localhost", &args);
        let form = iq
            .get_child("command", NS_ADHOC_COMMANDS)
            .and_then(|c| c.get_child("x", NS_XDATA))
            .expect("form");
        let confirm_value = form
            .children()
            .filter(|c| c.name() == "field" && c.attr("var") == Some("confirm"))
            .find_map(|f| f.get_child("value", NS_XDATA).map(|v| v.text()))
            .expect("confirm field");
        assert_eq!(confirm_value, "yes");
    }
}
