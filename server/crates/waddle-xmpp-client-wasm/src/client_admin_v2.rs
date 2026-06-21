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
//! - `admin_channels_create` — name + optional topic / space / XEP-0045 visibility policy.
//! - `admin_channels_update` — patch name / topic / XEP-0045 visibility policy.
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
    pub space_node: String,
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
    pub space_node: String,
    pub name: String,
    pub description: Option<String>,
    pub icon_url: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct WaddleAdminSpacesUpdateArgs {
    pub space_jid: String,
    pub space_node: Option<String>,
    pub name: Option<String>,
    pub description: Option<String>,
    pub icon_url: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct WaddleAdminSpacesDeleteArgs {
    pub space_jid: String,
    pub space_node: Option<String>,
    /// MUST be the literal string "yes"; mirrors the server-side guard.
    pub confirm: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct WaddleAdminSpacesMembersArgs {
    pub space_jid: String,
    pub space_node: Option<String>,
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
    pub space_node: Option<String>,
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
    pub space_node: Option<String>,
    pub prefix: Option<String>,
    pub page_size: Option<u32>,
    pub after_cursor: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct WaddleAdminChannelListEntry {
    pub channel_jid: String,
    pub name: String,
    pub topic: Option<String>,
    pub channel_type: String,
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
    pub channel_type: Option<String>,
    pub space_jid: Option<String>,
    pub space_node: Option<String>,
    /// Defaults to `true` per the V2 spec; the client wrapper passes
    /// this through verbatim. The server enforces the same default
    /// when the field is absent.
    pub is_public: Option<bool>,
    /// Optional XEP-0045 `muc#roomconfig_membersonly`; omitted lets the
    /// server derive the default from `is_public`.
    pub members_only: Option<bool>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct WaddleAdminChannelRef {
    pub channel_jid: String,
    pub name: String,
    pub topic: Option<String>,
    pub channel_type: String,
    pub is_public: bool,
    pub members_only: bool,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct WaddleAdminChannelsUpdateArgs {
    pub channel_jid: String,
    pub name: Option<String>,
    pub topic: Option<String>,
    pub channel_type: Option<String>,
    pub is_public: Option<bool>,
    pub members_only: Option<bool>,
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
            let page = parse_spaces_list_result(&result).map_err(js_error)?;
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
            let space = parse_space_ref_result(&result).map_err(js_error)?;
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
            let space = parse_space_ref_result(&result).map_err(js_error)?;
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
            let page = parse_spaces_members_result(&result).map_err(js_error)?;
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
            let payload = parse_spaces_set_role_result(&result).map_err(js_error)?;
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
            let page = parse_channels_list_result(&result).map_err(js_error)?;
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
            let channel = parse_channel_ref_result(&result).map_err(js_error)?;
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
            let channel = parse_channel_ref_result(&result).map_err(js_error)?;
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
            let page = parse_channels_occupants_result(&result).map_err(js_error)?;
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
            let page = parse_channels_affiliations_result(&result).map_err(js_error)?;
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
            let payload = parse_channels_set_affiliation_result(&result).map_err(js_error)?;
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
            let payload = parse_channels_kick_result(&result).map_err(js_error)?;
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
        .attr(minidom::rxml::xml_ncname!("var").to_owned(), var)
        .attr(minidom::rxml::xml_ncname!("type").to_owned(), "text-single")
        .append(Element::builder("value", NS_XDATA).append(value).build())
        .build()
}

fn boolean_field(var: &str, value: bool) -> Element {
    Element::builder("field", NS_XDATA)
        .attr(minidom::rxml::xml_ncname!("var").to_owned(), var)
        .attr(minidom::rxml::xml_ncname!("type").to_owned(), "boolean")
        .append(
            Element::builder("value", NS_XDATA)
                .append(if value { "1" } else { "0" })
                .build(),
        )
        .build()
}

fn jid_field(var: &str, value: &str) -> Element {
    Element::builder("field", NS_XDATA)
        .attr(minidom::rxml::xml_ncname!("var").to_owned(), var)
        .attr(minidom::rxml::xml_ncname!("type").to_owned(), "jid-single")
        .append(Element::builder("value", NS_XDATA).append(value).build())
        .build()
}

fn submit_form_with_type(form_type: &str) -> minidom::element::ElementBuilder {
    Element::builder("x", NS_XDATA)
        .attr(minidom::rxml::xml_ncname!("type").to_owned(), "submit")
        .append(
            Element::builder("field", NS_XDATA)
                .attr(minidom::rxml::xml_ncname!("var").to_owned(), "FORM_TYPE")
                .attr(minidom::rxml::xml_ncname!("type").to_owned(), "hidden")
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
        .attr(minidom::rxml::xml_ncname!("node").to_owned(), node)
        .attr(minidom::rxml::xml_ncname!("action").to_owned(), "execute")
        .append(form)
        .build();
    Element::builder("iq", NS_CLIENT)
        .attr(minidom::rxml::xml_ncname!("type").to_owned(), "set")
        .attr(
            minidom::rxml::xml_ncname!("id").to_owned(),
            uuid::Uuid::new_v4().to_string(),
        )
        .attr(minidom::rxml::xml_ncname!("to").to_owned(), server_domain)
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
    if let Some(space_node) = args.space_node.as_deref() {
        form = form.append(text_single_field("space_node", space_node));
    }
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
    let mut form = submit_form_with_type(NS_ADMIN_SPACES_DELETE)
        .append(jid_field("space_jid", &args.space_jid));
    if let Some(space_node) = args.space_node.as_deref() {
        form = form.append(text_single_field("space_node", space_node));
    }
    form = form.append(text_single_field("confirm", &args.confirm));
    wrap_command_iq(server_domain, NS_ADMIN_SPACES_DELETE, form.build())
}

fn build_spaces_members_iq(server_domain: &str, args: &WaddleAdminSpacesMembersArgs) -> Element {
    let mut form = submit_form_with_type(NS_ADMIN_SPACES_MEMBERS)
        .append(jid_field("space_jid", &args.space_jid));
    if let Some(space_node) = args.space_node.as_deref() {
        form = form.append(text_single_field("space_node", space_node));
    }
    if let Some(page_size) = args.page_size {
        form = form.append(text_single_field("page_size", &page_size.to_string()));
    }
    if let Some(after_cursor) = args.after_cursor.as_deref() {
        form = form.append(text_single_field("after_cursor", after_cursor));
    }
    wrap_command_iq(server_domain, NS_ADMIN_SPACES_MEMBERS, form.build())
}

fn build_spaces_set_role_iq(server_domain: &str, args: &WaddleAdminSpacesSetRoleArgs) -> Element {
    let mut form = submit_form_with_type(NS_ADMIN_SPACES_SET_ROLE)
        .append(jid_field("space_jid", &args.space_jid));
    if let Some(space_node) = args.space_node.as_deref() {
        form = form.append(text_single_field("space_node", space_node));
    }
    form = form
        .append(jid_field("member_jid", &args.member_jid))
        .append(text_single_field("role", &args.role));
    wrap_command_iq(server_domain, NS_ADMIN_SPACES_SET_ROLE, form.build())
}

fn build_channels_list_iq(server_domain: &str, args: &WaddleAdminChannelsListArgs) -> Element {
    let mut form = submit_form_with_type(NS_ADMIN_CHANNELS_LIST);
    if let Some(space_jid) = args.space_jid.as_deref() {
        form = form.append(jid_field("space_jid", space_jid));
    }
    if let Some(space_node) = args.space_node.as_deref() {
        form = form.append(text_single_field("space_node", space_node));
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
    if let Some(channel_type) = args.channel_type.as_deref() {
        form = form.append(text_single_field("channel_type", channel_type));
    }
    if let Some(space_jid) = args.space_jid.as_deref() {
        form = form.append(jid_field("space_jid", space_jid));
    }
    if let Some(space_node) = args.space_node.as_deref() {
        form = form.append(text_single_field("space_node", space_node));
    }
    if let Some(is_public) = args.is_public {
        form = form.append(boolean_field("is_public", is_public));
    }
    if let Some(members_only) = args.members_only {
        form = form.append(boolean_field("members_only", members_only));
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
    if let Some(channel_type) = args.channel_type.as_deref() {
        form = form.append(text_single_field("channel_type", channel_type));
    }
    if let Some(is_public) = args.is_public {
        form = form.append(boolean_field("is_public", is_public));
    }
    if let Some(members_only) = args.members_only {
        form = form.append(boolean_field("members_only", members_only));
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

type AdminParseResult<T> = Result<T, String>;

fn command_form(iq: &Element) -> AdminParseResult<&Element> {
    let command = iq
        .get_child("command", NS_ADHOC_COMMANDS)
        .ok_or_else(|| "admin response missing <command/>".to_string())?;
    command
        .get_child("x", NS_XDATA)
        .ok_or_else(|| "admin response missing <x xmlns='jabber:x:data'/>".to_string())
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

fn field_text_required(item: &Element, var: &str) -> AdminParseResult<String> {
    field_text(item, var)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| format!("admin response item missing required field `{var}`"))
}

fn parse_bool_field(raw: &str, var: &str) -> AdminParseResult<bool> {
    match raw {
        "1" | "true" => Ok(true),
        "0" | "false" => Ok(false),
        _ => Err(format!("admin response field `{var}` is not a boolean")),
    }
}

fn field_bool(item: &Element, var: &str) -> AdminParseResult<bool> {
    let raw = field_text_required(item, var)?;
    parse_bool_field(raw.as_str(), var)
}

fn field_u32(item: &Element, var: &str) -> AdminParseResult<u32> {
    let raw = field_text_required(item, var)?;
    raw.parse::<u32>()
        .map_err(|err| format!("admin response field `{var}` is not a u32: {err}"))
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

fn parse_spaces_list_result(iq: &Element) -> AdminParseResult<WaddleAdminSpacesListResult> {
    let Some(form) = maybe_command_form(iq) else {
        return Ok(WaddleAdminSpacesListResult::default());
    };
    let mut entries = Vec::new();
    for item in form.children().filter(|c| c.name() == "item") {
        entries.push(WaddleAdminSpaceListEntry {
            space_jid: field_text_required(item, "space_jid")?,
            space_node: field_text_required(item, "space_node")?,
            name: field_text_required(item, "name")?,
            description: field_text(item, "description").filter(|s| !s.is_empty()),
            icon_url: field_text(item, "icon_url").filter(|s| !s.is_empty()),
            channel_count: field_u32(item, "channel_count")?,
            member_count: field_u32(item, "member_count")?,
        });
    }
    Ok(WaddleAdminSpacesListResult {
        entries,
        next_cursor: top_level_next_cursor(form),
    })
}

fn parse_space_ref_result(iq: &Element) -> AdminParseResult<WaddleAdminSpaceRef> {
    let form = command_form(iq)?;
    Ok(WaddleAdminSpaceRef {
        space_jid: top_level_field_text_required(form, "space_jid")?,
        space_node: top_level_field_text_required(form, "space_node")?,
        name: top_level_field_text_required(form, "name")?,
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

fn top_level_field_text_required(form: &Element, var: &str) -> AdminParseResult<String> {
    let value = top_level_field_text(form, var);
    if value.is_empty() {
        Err(format!(
            "admin response missing required top-level field `{var}`"
        ))
    } else {
        Ok(value)
    }
}

fn top_level_field_text_opt(form: &Element, var: &str) -> Option<String> {
    let text = top_level_field_text(form, var);
    if text.is_empty() {
        None
    } else {
        Some(text)
    }
}

fn top_level_field_bool(form: &Element, var: &str) -> AdminParseResult<bool> {
    let raw = top_level_field_text_required(form, var)?;
    parse_bool_field(raw.as_str(), var)
}

fn parse_spaces_members_result(iq: &Element) -> AdminParseResult<WaddleAdminSpacesMembersResult> {
    let Some(form) = maybe_command_form(iq) else {
        return Ok(WaddleAdminSpacesMembersResult::default());
    };
    let mut entries = Vec::new();
    for item in form.children().filter(|c| c.name() == "item") {
        entries.push(WaddleAdminSpaceMemberEntry {
            jid: field_text_required(item, "jid")?,
            role: field_text_required(item, "role")?,
        });
    }
    Ok(WaddleAdminSpacesMembersResult {
        entries,
        next_cursor: top_level_next_cursor(form),
    })
}

fn parse_spaces_set_role_result(iq: &Element) -> AdminParseResult<WaddleAdminSpacesSetRoleResult> {
    let form = command_form(iq)?;
    Ok(WaddleAdminSpacesSetRoleResult {
        member_jid: top_level_field_text_required(form, "member_jid")?,
        role: top_level_field_text_required(form, "role")?,
    })
}

fn parse_channels_list_result(iq: &Element) -> AdminParseResult<WaddleAdminChannelsListResult> {
    let Some(form) = maybe_command_form(iq) else {
        return Ok(WaddleAdminChannelsListResult::default());
    };
    let mut entries = Vec::new();
    for item in form.children().filter(|c| c.name() == "item") {
        entries.push(WaddleAdminChannelListEntry {
            channel_jid: field_text_required(item, "channel_jid")?,
            name: field_text_required(item, "name")?,
            topic: field_text(item, "topic").filter(|s| !s.is_empty()),
            channel_type: field_text_required(item, "channel_type")?,
            is_public: field_bool(item, "is_public")?,
            members_only: field_bool(item, "members_only")?,
            occupant_count: field_u32(item, "occupant_count")?,
            owner_count: field_u32(item, "owner_count")?,
            admin_count: field_u32(item, "admin_count")?,
            member_count: field_u32(item, "member_count")?,
            outcast_count: field_u32(item, "outcast_count")?,
        });
    }
    Ok(WaddleAdminChannelsListResult {
        entries,
        next_cursor: top_level_next_cursor(form),
    })
}

fn parse_channel_ref_result(iq: &Element) -> AdminParseResult<WaddleAdminChannelRef> {
    let form = command_form(iq)?;
    Ok(WaddleAdminChannelRef {
        channel_jid: top_level_field_text_required(form, "channel_jid")?,
        name: top_level_field_text_required(form, "name")?,
        topic: top_level_field_text_opt(form, "topic"),
        channel_type: top_level_field_text_required(form, "channel_type")?,
        is_public: top_level_field_bool(form, "is_public")?,
        members_only: top_level_field_bool(form, "members_only")?,
    })
}

fn parse_channels_occupants_result(
    iq: &Element,
) -> AdminParseResult<WaddleAdminChannelsOccupantsResult> {
    let Some(form) = maybe_command_form(iq) else {
        return Ok(WaddleAdminChannelsOccupantsResult::default());
    };
    let mut entries = Vec::new();
    for item in form.children().filter(|c| c.name() == "item") {
        entries.push(WaddleAdminChannelOccupantEntry {
            nick: field_text_required(item, "nick")?,
            real_jid: field_text_required(item, "real_jid")?,
            role: field_text_required(item, "role")?,
            affiliation: field_text_required(item, "affiliation")?,
        });
    }
    Ok(WaddleAdminChannelsOccupantsResult {
        entries,
        next_cursor: top_level_next_cursor(form),
    })
}

fn parse_channels_affiliations_result(
    iq: &Element,
) -> AdminParseResult<WaddleAdminChannelsAffiliationsResult> {
    let Some(form) = maybe_command_form(iq) else {
        return Ok(WaddleAdminChannelsAffiliationsResult::default());
    };
    let mut entries = Vec::new();
    for item in form.children().filter(|c| c.name() == "item") {
        entries.push(WaddleAdminChannelAffiliationEntry {
            jid: field_text_required(item, "jid")?,
            affiliation: field_text_required(item, "affiliation")?,
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
) -> AdminParseResult<WaddleAdminChannelsSetAffiliationResult> {
    let form = command_form(iq)?;
    Ok(WaddleAdminChannelsSetAffiliationResult {
        member_jid: top_level_field_text_required(form, "member_jid")?,
        affiliation: top_level_field_text_required(form, "affiliation")?,
    })
}

fn parse_channels_kick_result(iq: &Element) -> AdminParseResult<WaddleAdminChannelsKickResult> {
    let form = command_form(iq)?;
    Ok(WaddleAdminChannelsKickResult {
        occupant_jid: top_level_field_text_required(form, "occupant_jid")?,
    })
}

// ─── Tests ────────────────────────────────────────────────────────────

#[cfg(test)]
#[path = "client_admin_v2_tests.rs"]
mod tests;
