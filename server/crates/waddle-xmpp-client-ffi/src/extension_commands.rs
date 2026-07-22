//! `urn:waddle:extension:1` extension-command exports (slash
//! commands over XEP-0050 ad-hoc commands).
//!
//! Mirrors the wasm chat client's dynamic discovery one-to-one:
//! nothing is hardcoded — the extension service is found via
//! `disco#items` on the account domain (with the conventional
//! `extensions.<domain>` fallback), the command list via
//! `disco#items` on the service with the XEP-0050 commands node, and
//! per-command composer metadata via `disco#info` extension forms.
//! The shared builders and parsers live in
//! `waddle_xmpp_client::extension_commands`; this file adapts the
//! typed uniffi Records and orchestrates the IQ round-trips.
//!
//! Discovery is cache-free by design: the Kotlin/Swift session layer
//! caches per session and clears on login/logout.

use jid::BareJid;
use minidom::Element;

use waddle_xmpp_client::discovery::{
    build_disco_info_iq, build_disco_items_iq, parse_disco_info_result, parse_disco_items_result,
};
use waddle_xmpp_client::extension_commands as core;
use waddle_xmpp_client::xep::xep0050::{AdHocAction, AdHocStatus};
use waddle_xmpp_client::{ClientError, ClientHandle};

use crate::boundary_convert::jid_domain;
use crate::error::client_error_to_waddle;
use crate::messaging_verbs::send_iq_with_timeout;
use crate::{
    WaddleAdhocAction, WaddleAdhocStatus, WaddleClient, WaddleError, WaddleExtensionCommand,
    WaddleExtensionCommandForm, WaddleExtensionCommandFormField, WaddleExtensionCommandNote,
    WaddleExtensionCommandResult, WaddleExtensionCommandScope, WaddleExtensionFieldOption,
    WaddleExtensionFieldType, WaddleExtensionFormField, WaddleExtensionNoteType,
};

// ── FFI ↔ core mappings ─────────────────────────────────────────────────────

pub(crate) fn adhoc_action_from_ffi(action: WaddleAdhocAction) -> AdHocAction {
    match action {
        WaddleAdhocAction::Execute => AdHocAction::Execute,
        WaddleAdhocAction::Cancel => AdHocAction::Cancel,
        WaddleAdhocAction::Next => AdHocAction::Next,
        WaddleAdhocAction::Prev => AdHocAction::Prev,
        WaddleAdhocAction::Complete => AdHocAction::Complete,
    }
}

fn adhoc_action_to_ffi(action: AdHocAction) -> WaddleAdhocAction {
    match action {
        AdHocAction::Execute => WaddleAdhocAction::Execute,
        AdHocAction::Cancel => WaddleAdhocAction::Cancel,
        AdHocAction::Next => WaddleAdhocAction::Next,
        AdHocAction::Prev => WaddleAdhocAction::Prev,
        AdHocAction::Complete => WaddleAdhocAction::Complete,
    }
}

fn adhoc_status_to_ffi(status: AdHocStatus) -> WaddleAdhocStatus {
    match status {
        AdHocStatus::Executing => WaddleAdhocStatus::Executing,
        AdHocStatus::Completed => WaddleAdhocStatus::Completed,
        AdHocStatus::Canceled => WaddleAdhocStatus::Canceled,
    }
}

fn scope_to_ffi(scope: core::ExtensionCommandScope) -> WaddleExtensionCommandScope {
    match scope {
        core::ExtensionCommandScope::Global => WaddleExtensionCommandScope::Global,
        core::ExtensionCommandScope::Channel => WaddleExtensionCommandScope::Channel,
    }
}

fn field_type_to_ffi(field_type: core::ExtensionFieldType) -> WaddleExtensionFieldType {
    match field_type {
        core::ExtensionFieldType::Boolean => WaddleExtensionFieldType::Boolean,
        core::ExtensionFieldType::Fixed => WaddleExtensionFieldType::Fixed,
        core::ExtensionFieldType::Hidden => WaddleExtensionFieldType::Hidden,
        core::ExtensionFieldType::JidMulti => WaddleExtensionFieldType::JidMulti,
        core::ExtensionFieldType::JidSingle => WaddleExtensionFieldType::JidSingle,
        core::ExtensionFieldType::ListMulti => WaddleExtensionFieldType::ListMulti,
        core::ExtensionFieldType::ListSingle => WaddleExtensionFieldType::ListSingle,
        core::ExtensionFieldType::TextMulti => WaddleExtensionFieldType::TextMulti,
        core::ExtensionFieldType::TextPrivate => WaddleExtensionFieldType::TextPrivate,
        core::ExtensionFieldType::TextSingle => WaddleExtensionFieldType::TextSingle,
    }
}

fn note_type_to_ffi(note_type: core::ExtensionNoteType) -> WaddleExtensionNoteType {
    match note_type {
        core::ExtensionNoteType::Info => WaddleExtensionNoteType::Info,
        core::ExtensionNoteType::Warn => WaddleExtensionNoteType::Warn,
        core::ExtensionNoteType::Error => WaddleExtensionNoteType::Error,
    }
}

fn command_to_ffi(descriptor: core::ExtensionCommandDescriptor) -> WaddleExtensionCommand {
    WaddleExtensionCommand {
        service_jid: descriptor.service_jid,
        node: descriptor.node,
        name: descriptor.name,
        scope: scope_to_ffi(descriptor.scope),
        composer_prefix: descriptor.composer_prefix,
        inline_field: descriptor.inline_field,
        composer_execute: descriptor.composer_execute,
    }
}

fn form_to_ffi(form: core::ExtensionCommandForm) -> WaddleExtensionCommandForm {
    WaddleExtensionCommandForm {
        title: form.title,
        instructions: form.instructions,
        fields: form
            .fields
            .into_iter()
            .map(|field| WaddleExtensionCommandFormField {
                var: field.var,
                label: field.label,
                field_type: field_type_to_ffi(field.field_type),
                required: field.required,
                blocked: field.blocked,
                options: field
                    .options
                    .into_iter()
                    .map(|option| WaddleExtensionFieldOption {
                        label: option.label,
                        value: option.value,
                    })
                    .collect(),
                values: field.values,
            })
            .collect(),
    }
}

fn result_to_ffi(result: core::ExtensionCommandResult) -> WaddleExtensionCommandResult {
    WaddleExtensionCommandResult {
        status: adhoc_status_to_ffi(result.status),
        session_id: result.session_id,
        actions: result
            .actions
            .into_iter()
            .map(adhoc_action_to_ffi)
            .collect(),
        form: result.form.map(form_to_ffi),
        notes: result
            .notes
            .into_iter()
            .map(|note| WaddleExtensionCommandNote {
                note_type: note_type_to_ffi(note.note_type),
                value: note.value,
            })
            .collect(),
    }
}

fn submit_fields_from_ffi(
    fields: Vec<WaddleExtensionFormField>,
) -> Vec<core::ExtensionSubmitField> {
    fields
        .into_iter()
        .map(|field| core::ExtensionSubmitField {
            var: field.var,
            values: field.values,
        })
        .collect()
}

// ── FFI → wire stanza factories ─────────────────────────────────────────────
//
// Pure functions between the FFI argument shapes and the shared
// builders; the dedicated test suite pins their output against the
// XEP wire shapes without a live connection.

pub(crate) fn extension_invoke_stanza(
    service_jid: &BareJid,
    node: &str,
    room_jid: Option<&BareJid>,
) -> Element {
    let room = room_jid.map(BareJid::to_string);
    core::build_extension_invoke_iq(&service_jid.to_string(), node, room.as_deref())
}

pub(crate) fn extension_submit_stanza(
    service_jid: &BareJid,
    node: &str,
    session_id: Option<&str>,
    action: AdHocAction,
    fields: &[core::ExtensionSubmitField],
    room_jid: Option<&BareJid>,
) -> Element {
    let room = room_jid.map(BareJid::to_string);
    core::build_extension_submit_iq(
        &service_jid.to_string(),
        node,
        session_id,
        action,
        fields,
        room.as_deref(),
    )
}

/// Map a command reply into the typed FFI result. A reply that lacks
/// the `<command/>` payload or carries an unknown status is a
/// server-side bug surfaced as [`WaddleError::MalformedResponse`].
pub(crate) fn map_command_reply(
    reply: Result<Element, ClientError>,
) -> Result<WaddleExtensionCommandResult, WaddleError> {
    let element = reply.map_err(|e| client_error_to_waddle(&e))?;
    core::parse_extension_command_result(&element)
        .map(result_to_ffi)
        .map_err(|_| WaddleError::MalformedResponse)
}

// ── Discovery orchestration ─────────────────────────────────────────────────

/// Resolve the extension service JID: probe every candidate with
/// `disco#info` and take the first advertising both the Waddle
/// extension feature and XEP-0050 command support; fall back to the
/// conventional `extensions.<domain>` when nothing qualifies.
async fn discover_extension_service(handle: &ClientHandle, domain: &str) -> String {
    let discovered = match send_iq_with_timeout(handle, build_disco_items_iq(domain, None)).await {
        Ok(reply) => parse_disco_items_result(&reply)
            .unwrap_or_default()
            .into_iter()
            .map(|item| item.jid)
            .collect(),
        Err(_) => Vec::new(),
    };
    for candidate in core::extension_service_candidates(domain, &discovered) {
        let Ok(reply) = send_iq_with_timeout(handle, build_disco_info_iq(&candidate, None)).await
        else {
            continue;
        };
        if parse_disco_info_result(&reply, &candidate)
            .is_some_and(|info| core::is_extension_service(&info))
        {
            return candidate;
        }
    }
    core::fallback_extension_service_jid(domain)
}

/// Fetch one command's composer metadata; a failed or unparsable
/// `disco#info` degrades to the metadata-free default (global scope,
/// no composer affordances) — the wasm client's fallback.
async fn fetch_command_metadata(
    handle: &ClientHandle,
    service_jid: &str,
    node: &str,
) -> core::ExtensionCommandMetadata {
    match send_iq_with_timeout(handle, build_disco_info_iq(service_jid, Some(node))).await {
        Ok(reply) => parse_disco_info_result(&reply, service_jid)
            .map(|info| core::parse_extension_command_metadata(&info))
            .unwrap_or_default(),
        Err(_) => core::ExtensionCommandMetadata::default(),
    }
}

// ── Exported verbs ───────────────────────────────────────────────────────────

#[uniffi::export(async_runtime = "tokio")]
impl WaddleClient {
    /// Discover the extension service and its XEP-0050 command list,
    /// hydrated with per-command composer metadata (scope, slash
    /// prefix, inline field, direct-execute). Cache-free: callers
    /// cache per session.
    pub async fn discover_extension_commands(
        &self,
    ) -> Result<Vec<WaddleExtensionCommand>, WaddleError> {
        let Some(handle) = self.clone_handle().await else {
            return Err(WaddleError::NotConnected);
        };
        let domain = jid_domain(&self.config.jid).to_string();
        let service_jid = discover_extension_service(&handle, &domain).await;
        let items_iq = build_disco_items_iq(
            &service_jid,
            Some(waddle_xmpp_client::xep::xep0050::NS_COMMANDS),
        );
        let reply = send_iq_with_timeout(&handle, items_iq)
            .await
            .map_err(|e| client_error_to_waddle(&e))?;
        let items = parse_disco_items_result(&reply).unwrap_or_default();
        let mut commands = Vec::new();
        for item in core::extension_command_refs(items, &service_jid) {
            let metadata = fetch_command_metadata(&handle, &item.service_jid, &item.node).await;
            commands.push(command_to_ffi(core::extension_command_descriptor(
                item, metadata,
            )));
        }
        Ok(commands)
    }

    /// XEP-0050 §2.4: start an extension command with `execute`. In a
    /// room, `room_jid` travels as the `waddle#room_jid` submit field.
    pub async fn invoke_extension_command(
        &self,
        service_jid: String,
        node: String,
        room_jid: Option<String>,
    ) -> Result<WaddleExtensionCommandResult, WaddleError> {
        let service: BareJid = service_jid.parse().map_err(|_| WaddleError::InvalidJid)?;
        let room = room_jid
            .map(|jid| jid.parse::<BareJid>())
            .transpose()
            .map_err(|_| WaddleError::InvalidJid)?;
        if node.trim().is_empty() {
            return Err(WaddleError::InvalidArgument);
        }
        let Some(handle) = self.clone_handle().await else {
            return Err(WaddleError::NotConnected);
        };
        let iq = extension_invoke_stanza(&service, &node, room.as_ref());
        map_command_reply(send_iq_with_timeout(&handle, iq).await)
    }

    /// XEP-0050 §3: submit a stage of an extension command session.
    /// `session_id` is threaded verbatim from the previous response;
    /// `cancel`/`prev` submissions never carry a form.
    pub async fn submit_extension_command_form(
        &self,
        service_jid: String,
        node: String,
        session_id: Option<String>,
        fields: Vec<WaddleExtensionFormField>,
        action: WaddleAdhocAction,
        room_jid: Option<String>,
    ) -> Result<WaddleExtensionCommandResult, WaddleError> {
        let service: BareJid = service_jid.parse().map_err(|_| WaddleError::InvalidJid)?;
        let room = room_jid
            .map(|jid| jid.parse::<BareJid>())
            .transpose()
            .map_err(|_| WaddleError::InvalidJid)?;
        if node.trim().is_empty() {
            return Err(WaddleError::InvalidArgument);
        }
        let Some(handle) = self.clone_handle().await else {
            return Err(WaddleError::NotConnected);
        };
        let iq = extension_submit_stanza(
            &service,
            &node,
            session_id.as_deref(),
            adhoc_action_from_ffi(action),
            &submit_fields_from_ffi(fields),
            room.as_ref(),
        );
        map_command_reply(send_iq_with_timeout(&handle, iq).await)
    }
}
