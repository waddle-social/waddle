//! Wasm bindings for the V1 admin Users panel.
//!
//! Exposes:
//!
//! - `admin_users_list(prefix?, page_size?, after_cursor?)` →
//!   `Promise<WaddleAdminUsersPage>`.
//! - `is_community_owner()` → `Promise<bool>` — probes the admin
//!   command surface with `page_size=1`; success means the caller is
//!   the community owner, `<forbidden/>` means they are not.
//!
//! Both functions issue an XEP-0050 `iq type='set'` with a
//! `<command xmlns='http://jabber.org/protocol/commands'
//! node='urn:xmpp:waddle:admin:users:list:0' action='execute'>`
//! payload to the user-bearing server domain. The return shape is a
//! typed Serde struct projected into a JS value via
//! `serde_wasm_bindgen` — no JSON-blob strings cross the boundary
//! per the typed-payloads rule in CLAUDE.md.

use super::*;

const NS_ADMIN_USERS_LIST: &str = "urn:xmpp:waddle:admin:users:list:0";

#[derive(Debug, Serialize, Deserialize)]
pub struct WaddleAdminUserEntry {
    pub jid: String,
    pub display_name: Option<String>,
    pub has_owner_hat: bool,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct WaddleAdminUsersPage {
    pub entries: Vec<WaddleAdminUserEntry>,
    pub next_cursor: Option<String>,
}

#[wasm_bindgen]
impl WaddleClient {
    /// Call the `urn:xmpp:waddle:admin:users:list:0` ad-hoc command
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
            let iq = build_users_list_iq(&domain, prefix, page_size, after_cursor);
            let result = send_iq_command(inner, iq).await?;
            let page = parse_users_list_result(&result)?;
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
            let iq = build_users_list_iq(&domain, None, Some(1), None);
            match send_iq_command(inner, iq).await {
                Ok(_) => Ok(JsValue::from_bool(true)),
                Err(_) => Ok(JsValue::from_bool(false)),
            }
        })
    }
}

fn build_users_list_iq(
    server_domain: &str,
    prefix: Option<String>,
    page_size: Option<u32>,
    after_cursor: Option<String>,
) -> Element {
    let mut form_builder = Element::builder("x", "jabber:x:data").attr("type", "submit");

    // FORM_TYPE hidden field — XEP-0004 §3.2 mandates it for any
    // typed data form. We mirror the command node so the form id
    // and the command node never drift apart.
    form_builder = form_builder.append(
        Element::builder("field", "jabber:x:data")
            .attr("var", "FORM_TYPE")
            .attr("type", "hidden")
            .append(
                Element::builder("value", "jabber:x:data")
                    .append(NS_ADMIN_USERS_LIST)
                    .build(),
            )
            .build(),
    );

    if let Some(prefix) = prefix {
        form_builder = form_builder.append(text_single_field("prefix", &prefix));
    }
    if let Some(page_size) = page_size {
        form_builder = form_builder.append(text_single_field("page_size", &page_size.to_string()));
    }
    if let Some(after_cursor) = after_cursor {
        form_builder = form_builder.append(text_single_field("after_cursor", &after_cursor));
    }

    let command = Element::builder("command", NS_ADHOC_COMMANDS)
        .attr("node", NS_ADMIN_USERS_LIST)
        .attr("action", "execute")
        .append(form_builder.build())
        .build();

    Element::builder("iq", NS_CLIENT)
        .attr("type", "set")
        .attr("id", uuid::Uuid::new_v4().to_string())
        .attr("to", server_domain)
        .append(command)
        .build()
}

fn text_single_field(var: &str, value: &str) -> Element {
    Element::builder("field", "jabber:x:data")
        .attr("var", var)
        .attr("type", "text-single")
        .append(
            Element::builder("value", "jabber:x:data")
                .append(value)
                .build(),
        )
        .build()
}

/// Parse the XEP-0004 `result` data form returned by the admin
/// users-list command into the typed [`WaddleAdminUsersPage`].
///
/// We walk the form once and project rows into entries; the optional
/// `next_cursor` top-level field is captured separately.
pub(crate) fn parse_users_list_result(iq: &Element) -> Result<WaddleAdminUsersPage, JsValue> {
    let Some(command) = iq.get_child("command", NS_ADHOC_COMMANDS) else {
        return Err(js_error("admin response missing <command/>"));
    };
    let Some(form) = command.get_child("x", "jabber:x:data") else {
        return Ok(WaddleAdminUsersPage {
            entries: Vec::new(),
            next_cursor: None,
        });
    };

    let mut entries = Vec::new();
    let mut next_cursor: Option<String> = None;

    for child in form.children() {
        if child.name() == "field" && child.attr("var") == Some("next_cursor") {
            next_cursor = child
                .get_child("value", "jabber:x:data")
                .map(|value| value.text())
                .filter(|s| !s.is_empty());
            continue;
        }
        if child.name() == "item" {
            entries.push(parse_user_item(child));
        }
    }

    Ok(WaddleAdminUsersPage {
        entries,
        next_cursor,
    })
}

fn parse_user_item(item: &Element) -> WaddleAdminUserEntry {
    let mut jid = String::new();
    let mut display_name: Option<String> = None;
    let mut has_owner_hat = false;
    for field in item.children().filter(|c| c.name() == "field") {
        let Some(var) = field.attr("var") else {
            continue;
        };
        let value_text = field
            .get_child("value", "jabber:x:data")
            .map(|v| v.text())
            .unwrap_or_default();
        match var {
            "jid" => jid = value_text,
            "display_name" => {
                display_name = if value_text.is_empty() {
                    None
                } else {
                    Some(value_text)
                }
            }
            "has_owner_hat" => has_owner_hat = matches!(value_text.as_str(), "1" | "true"),
            _ => {}
        }
    }
    WaddleAdminUserEntry {
        jid,
        display_name,
        has_owner_hat,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn build_response(items_xml: &str, next_cursor: Option<&str>) -> Element {
        let form_inner = format!(
            r#"<field type="hidden" var="FORM_TYPE"><value>{NS_ADMIN_USERS_LIST}</value></field>
            <reported>
                <field label="JID" type="jid-single" var="jid"/>
                <field label="Display name" type="text-single" var="display_name"/>
                <field label="Community owner" type="boolean" var="has_owner_hat"/>
            </reported>
            {items_xml}
            {cursor}"#,
            cursor = next_cursor
                .map(|c| format!(
                    r#"<field var="next_cursor" type="text-single"><value>{c}</value></field>"#
                ))
                .unwrap_or_default(),
        );
        let raw = format!(
            r#"<iq xmlns='jabber:client' type='result' id='test'><command xmlns='http://jabber.org/protocol/commands' node='{NS_ADMIN_USERS_LIST}' status='completed'><x xmlns='jabber:x:data' type='result'>{form_inner}</x></command></iq>"#
        );
        raw.parse().expect("parse")
    }

    #[test]
    fn parse_empty_form_returns_empty_page() {
        let iq = build_response("", None);
        let page = parse_users_list_result(&iq).expect("ok");
        assert!(page.entries.is_empty());
        assert!(page.next_cursor.is_none());
    }

    #[test]
    fn parse_single_entry_with_cursor() {
        let item = r#"<item>
            <field var="jid"><value>alice@localhost</value></field>
            <field var="display_name"><value>Alice</value></field>
            <field var="has_owner_hat"><value>0</value></field>
        </item>"#;
        let iq = build_response(item, Some("alice"));
        let page = parse_users_list_result(&iq).expect("ok");
        assert_eq!(page.entries.len(), 1);
        let entry = &page.entries[0];
        assert_eq!(entry.jid, "alice@localhost");
        assert_eq!(entry.display_name.as_deref(), Some("Alice"));
        assert!(!entry.has_owner_hat);
        assert_eq!(page.next_cursor.as_deref(), Some("alice"));
    }

    #[test]
    fn parse_owner_flag_recognizes_boolean_variants() {
        for raw in ["1", "true"] {
            let item = format!(
                r#"<item>
                    <field var="jid"><value>admin@localhost</value></field>
                    <field var="display_name"><value></value></field>
                    <field var="has_owner_hat"><value>{raw}</value></field>
                </item>"#
            );
            let iq = build_response(&item, None);
            let page = parse_users_list_result(&iq).expect("ok");
            assert!(page.entries[0].has_owner_hat, "{raw} → owner");
            assert!(page.entries[0].display_name.is_none());
        }
    }
}
