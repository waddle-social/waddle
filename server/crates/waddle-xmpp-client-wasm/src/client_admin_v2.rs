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
//! - `admin_channels_kick` — XEP-0045 §8.2 role→none.
//!
//! Builders, parsers, and the serde-typed Args/Result structs live in
//! the shared `waddle_xmpp_client::admin_commands` module — one
//! implementation for the wasm chat client and the FFI clients. This
//! file only adapts the JS boundary: args cross via
//! `serde_wasm_bindgen`, and the parsed typed result is projected
//! back into a JS value. JS never sees raw XML.

use waddle_xmpp_client::admin_commands::{
    build_admin_channels_affiliations_iq, build_admin_channels_create_iq,
    build_admin_channels_delete_iq, build_admin_channels_kick_iq, build_admin_channels_list_iq,
    build_admin_channels_occupants_iq, build_admin_channels_set_affiliation_iq,
    build_admin_channels_update_iq, build_admin_spaces_create_iq, build_admin_spaces_delete_iq,
    build_admin_spaces_list_iq, build_admin_spaces_members_iq, build_admin_spaces_set_role_iq,
    build_admin_spaces_update_iq, parse_admin_channel_ref_result,
    parse_admin_channels_affiliations_result, parse_admin_channels_kick_result,
    parse_admin_channels_list_result, parse_admin_channels_occupants_result,
    parse_admin_channels_set_affiliation_result, parse_admin_space_ref_result,
    parse_admin_spaces_list_result, parse_admin_spaces_members_result,
    parse_admin_spaces_set_role_result, AdminChannelsAffiliationsArgs, AdminChannelsCreateArgs,
    AdminChannelsDeleteArgs, AdminChannelsKickArgs, AdminChannelsListArgs,
    AdminChannelsOccupantsArgs, AdminChannelsSetAffiliationArgs, AdminChannelsUpdateArgs,
    AdminSpacesCreateArgs, AdminSpacesDeleteArgs, AdminSpacesListArgs, AdminSpacesMembersArgs,
    AdminSpacesSetRoleArgs, AdminSpacesUpdateArgs,
};

use super::*;

fn caller_domain(inner: &Rc<RefCell<WaddleClientInner>>) -> String {
    let stored = inner.borrow().config.clone();
    jid_domain(&stored.jid)
}

/// Run one V2 admin command: deserialize the JS args, build the IQ
/// via the shared core builder, await the round-trip, and project the
/// parsed typed result back to JS.
macro_rules! admin_v2_command {
    ($self:ident, $args:ident, $args_ty:ty, $build:ident, $parse:ident) => {{
        let inner = $self.inner.clone();
        future_to_promise(async move {
            let parsed: $args_ty =
                serde_wasm_bindgen::from_value($args).map_err(|e| js_error(e.to_string()))?;
            let domain = caller_domain(&inner);
            let iq = $build(&domain, &parsed);
            let result = send_iq_command(inner, iq).await?;
            let payload = $parse(&result).map_err(|e| js_error(e.to_string()))?;
            to_js_value(&payload)
        })
    }};
}

/// Same as [`admin_v2_command!`] for the delete commands, whose
/// completed responses carry no payload — resolves `true`.
macro_rules! admin_v2_delete {
    ($self:ident, $args:ident, $args_ty:ty, $build:ident) => {{
        let inner = $self.inner.clone();
        future_to_promise(async move {
            let parsed: $args_ty =
                serde_wasm_bindgen::from_value($args).map_err(|e| js_error(e.to_string()))?;
            let domain = caller_domain(&inner);
            let iq = $build(&domain, &parsed);
            let _ = send_iq_command(inner, iq).await?;
            Ok(JsValue::TRUE)
        })
    }};
}

#[wasm_bindgen]
impl WaddleClient {
    pub fn admin_spaces_list(&self, args: JsValue) -> Promise {
        admin_v2_command!(
            self,
            args,
            AdminSpacesListArgs,
            build_admin_spaces_list_iq,
            parse_admin_spaces_list_result
        )
    }

    pub fn admin_spaces_create(&self, args: JsValue) -> Promise {
        admin_v2_command!(
            self,
            args,
            AdminSpacesCreateArgs,
            build_admin_spaces_create_iq,
            parse_admin_space_ref_result
        )
    }

    pub fn admin_spaces_update(&self, args: JsValue) -> Promise {
        admin_v2_command!(
            self,
            args,
            AdminSpacesUpdateArgs,
            build_admin_spaces_update_iq,
            parse_admin_space_ref_result
        )
    }

    pub fn admin_spaces_delete(&self, args: JsValue) -> Promise {
        admin_v2_delete!(
            self,
            args,
            AdminSpacesDeleteArgs,
            build_admin_spaces_delete_iq
        )
    }

    pub fn admin_spaces_members(&self, args: JsValue) -> Promise {
        admin_v2_command!(
            self,
            args,
            AdminSpacesMembersArgs,
            build_admin_spaces_members_iq,
            parse_admin_spaces_members_result
        )
    }

    pub fn admin_spaces_set_role(&self, args: JsValue) -> Promise {
        admin_v2_command!(
            self,
            args,
            AdminSpacesSetRoleArgs,
            build_admin_spaces_set_role_iq,
            parse_admin_spaces_set_role_result
        )
    }

    pub fn admin_channels_list(&self, args: JsValue) -> Promise {
        admin_v2_command!(
            self,
            args,
            AdminChannelsListArgs,
            build_admin_channels_list_iq,
            parse_admin_channels_list_result
        )
    }

    pub fn admin_channels_create(&self, args: JsValue) -> Promise {
        admin_v2_command!(
            self,
            args,
            AdminChannelsCreateArgs,
            build_admin_channels_create_iq,
            parse_admin_channel_ref_result
        )
    }

    pub fn admin_channels_update(&self, args: JsValue) -> Promise {
        admin_v2_command!(
            self,
            args,
            AdminChannelsUpdateArgs,
            build_admin_channels_update_iq,
            parse_admin_channel_ref_result
        )
    }

    pub fn admin_channels_delete(&self, args: JsValue) -> Promise {
        admin_v2_delete!(
            self,
            args,
            AdminChannelsDeleteArgs,
            build_admin_channels_delete_iq
        )
    }

    pub fn admin_channels_occupants(&self, args: JsValue) -> Promise {
        admin_v2_command!(
            self,
            args,
            AdminChannelsOccupantsArgs,
            build_admin_channels_occupants_iq,
            parse_admin_channels_occupants_result
        )
    }

    pub fn admin_channels_affiliations(&self, args: JsValue) -> Promise {
        admin_v2_command!(
            self,
            args,
            AdminChannelsAffiliationsArgs,
            build_admin_channels_affiliations_iq,
            parse_admin_channels_affiliations_result
        )
    }

    pub fn admin_channels_set_affiliation(&self, args: JsValue) -> Promise {
        admin_v2_command!(
            self,
            args,
            AdminChannelsSetAffiliationArgs,
            build_admin_channels_set_affiliation_iq,
            parse_admin_channels_set_affiliation_result
        )
    }

    pub fn admin_channels_kick(&self, args: JsValue) -> Promise {
        admin_v2_command!(
            self,
            args,
            AdminChannelsKickArgs,
            build_admin_channels_kick_iq,
            parse_admin_channels_kick_result
        )
    }
}
