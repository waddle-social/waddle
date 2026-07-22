//! Wasm bindings for the V1 admin Users panel.
//!
//! Exposes:
//!
//! - `admin_users_list(prefix?, page_size?, after_cursor?)` →
//!   `Promise<AdminUsersPage>`.
//! - `is_community_owner()` → `Promise<bool>` — probes the admin
//!   command surface with `page_size=1`; success means the caller is
//!   the community owner, `<forbidden/>` means they are not.
//!
//! The IQ builders and result parsers live in the shared
//! `waddle_xmpp_client::admin_commands` module (one implementation
//! for wasm and FFI); this file only adapts the JS boundary. The
//! return shape is a typed Serde struct projected into a JS value via
//! `serde_wasm_bindgen` — no JSON-blob strings cross the boundary
//! per the typed-payloads rule in CLAUDE.md.

use waddle_xmpp_client::admin_commands::{
    build_admin_users_list_iq, parse_admin_users_list_result, AdminUsersListArgs,
};

use super::*;

#[wasm_bindgen]
impl WaddleClient {
    /// Call the `urn:waddle:admin:users:list:0` ad-hoc command
    /// against the user-bearing server domain and return a typed
    /// page of matching users. Errors out (rejecting the returned
    /// Promise) if the server replies with a stanza error — the
    /// chat client interprets `<forbidden/>` as "not the community
    /// owner" and falls back to the empty-state screen.
    pub fn admin_users_list(
        &self,
        prefix: Option<String>,
        page_size: Option<u32>,
        after_cursor: Option<String>,
    ) -> Promise {
        let inner = self.inner.clone();
        future_to_promise(async move {
            let domain = {
                let stored = inner.borrow().config.clone();
                jid_domain(&stored.jid)
            };
            let args = AdminUsersListArgs {
                prefix,
                page_size,
                after_cursor,
            };
            let iq = build_admin_users_list_iq(&domain, &args);
            let result = send_iq_command(inner, iq).await?;
            let page =
                parse_admin_users_list_result(&result).map_err(|err| js_error(err.to_string()))?;
            to_js_value(&page)
        })
    }

    /// `true` iff the authenticated user is the community owner — i.e.
    /// the server accepts a probe of the admin Users command. Any
    /// stanza error (including `<forbidden/>`) resolves to `false`;
    /// the wasm boundary doesn't try to distinguish "not owner" from
    /// "server error" because the admin panel's empty state is the
    /// right fallback in either case.
    pub fn is_community_owner(&self) -> Promise {
        let inner = self.inner.clone();
        future_to_promise(async move {
            let domain = {
                let stored = inner.borrow().config.clone();
                jid_domain(&stored.jid)
            };
            // Probe with page_size=1 to keep the database scan trivial
            // when the answer is "yes." The body of the response is
            // discarded; only success-vs-error matters.
            let args = AdminUsersListArgs {
                prefix: None,
                page_size: Some(1),
                after_cursor: None,
            };
            let iq = build_admin_users_list_iq(&domain, &args);
            match send_iq_command(inner, iq).await {
                Ok(_) => Ok(JsValue::from_bool(true)),
                Err(_) => Ok(JsValue::from_bool(false)),
            }
        })
    }
}
