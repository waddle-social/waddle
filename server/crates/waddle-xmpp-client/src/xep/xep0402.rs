//! XEP-0402: PEP-native bookmarks.
//!
//! Typed model + IQ builders for the `urn:xmpp:bookmarks:1` PEP node
//! used to synchronise the user's bookmarked group chats — and, per
//! XEP-0402 §2, any per-chat extensions attached as `<extensions/>`
//! children of `<conference/>`.
//!
//! This module owns the wire ↔ typed translation for the bookmark
//! envelope only. Inner extensions (XEP-0492 `<notify/>`, etc.) are
//! carried as opaque [`Element`] values so this surface stays
//! XEP-0402-shaped and does not bake in any specific extension's
//! schema.

use jid::BareJid;
use minidom::Element;

use crate::pep::{NS_BOOKMARKS, NS_PUBSUB};

const NS_CLIENT: &str = "jabber:client";

/// One XEP-0402 bookmark item.
///
/// `jid` is the bookmarked room's bare JID per XEP-0402 §2.2; `name`,
/// `autojoin`, `nick`, and `password` map onto the `<conference/>`
/// schema (§2.2 + XEP-0402 XSD); `extensions` carries every direct
/// child of `<extensions/>` opaquely so foreign extensions
/// (XEP-0492 `<notify/>`, other specs) are never lost.
///
/// `nick` and `password` are round-tripped verbatim so a notification-
/// mode toggle does not silently destroy a sibling client's stored
/// nickname or password — round-7 XEP-0402 reviewer P1 pinning.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BookmarkItem {
    /// The room's bare JID (XEP-0402 §2 item id).
    pub jid: BareJid,
    /// `<conference name='…'>` attribute, when present.
    pub name: Option<String>,
    /// `<conference autojoin='true|false'>` attribute. XEP-0402 §2.2
    /// defaults this to `false` when absent.
    pub autojoin: bool,
    /// `<nick/>` child element body, when present (XEP-0402 §2.2).
    pub nick: Option<String>,
    /// `<password/>` child element body, when present (XEP-0402 §2.2).
    pub password: Option<String>,
    /// Raw children of `<extensions/>`, preserved verbatim so
    /// concurrent writers of foreign extensions (XEP-0492 advanced
    /// settings, other specs) don't lose their state when this
    /// client rewrites only the parts it knows about.
    pub extensions: Vec<Element>,
}

impl BookmarkItem {
    /// Parse an `<item id='…'><conference …>…</conference></item>`
    /// element from a XEP-0060 `<items/>` reply.
    ///
    /// Returns `None` if either the item id is not a bare JID or the
    /// expected `<conference/>` payload is missing.
    pub fn parse_item(item: &Element) -> Option<Self> {
        let id_attr = item.attr("id")?;
        let jid: BareJid = id_attr.parse().ok()?;
        let conference = item.get_child("conference", NS_BOOKMARKS)?;
        let name = conference
            .attr("name")
            .filter(|name| !name.is_empty())
            .map(str::to_string);
        let autojoin = matches!(conference.attr("autojoin"), Some("true") | Some("1"));
        let nick = conference
            .get_child("nick", NS_BOOKMARKS)
            .map(|el| el.text())
            .filter(|text| !text.is_empty());
        let password = conference
            .get_child("password", NS_BOOKMARKS)
            .map(|el| el.text())
            .filter(|text| !text.is_empty());
        // XEP-0402 XSD declares `<extensions/>` content as
        // `<xs:any namespace='##other'>` — children MUST live in a
        // non-bookmarks namespace. Filter any malformed
        // same-namespace child here so a republish doesn't
        // propagate the violation back onto the wire. Round-12
        // Copilot review.
        let extensions = conference
            .get_child("extensions", NS_BOOKMARKS)
            .map(|exts| {
                exts.children()
                    .filter(|child| child.ns() != NS_BOOKMARKS)
                    .cloned()
                    .collect()
            })
            .unwrap_or_default();
        Some(Self {
            jid,
            name,
            autojoin,
            nick,
            password,
            extensions,
        })
    }

    /// Build the `<conference/>` payload for a publish IQ.
    ///
    /// `<nick/>` and `<password/>` come *before* `<extensions/>` to
    /// match the XEP-0402 XSD child-element ordering. Omitted (no
    /// element on the wire) when the typed value is `None`.
    /// `autojoin` is only emitted when `true` — XEP-0402 §2.2 XSD
    /// defaults the attribute to `false`, and an absent attr is the
    /// canonical "no autojoin" wire shape.
    pub fn build_conference_element(&self) -> Element {
        let mut builder = Element::builder("conference", NS_BOOKMARKS);
        if self.autojoin {
            builder = builder.attr(minidom::rxml::xml_ncname!("autojoin").to_owned(), "true");
        }
        if let Some(name) = self.name.as_deref() {
            builder = builder.attr(minidom::rxml::xml_ncname!("name").to_owned(), name);
        }
        if let Some(nick) = self.nick.as_deref() {
            builder = builder.append(Element::builder("nick", NS_BOOKMARKS).append(nick).build());
        }
        if let Some(password) = self.password.as_deref() {
            builder = builder.append(
                Element::builder("password", NS_BOOKMARKS)
                    .append(password)
                    .build(),
            );
        }
        if !self.extensions.is_empty() {
            let mut exts = Element::builder("extensions", NS_BOOKMARKS);
            for child in &self.extensions {
                exts = exts.append(child.clone());
            }
            builder = builder.append(exts.build());
        }
        builder.build()
    }
}

/// Build a `get` IQ requesting the user's bookmark items from their
/// own PEP `urn:xmpp:bookmarks:1` node.
pub fn build_fetch_bookmarks_iq(request_id: &str) -> Element {
    Element::builder("iq", NS_CLIENT)
        .attr(minidom::rxml::xml_ncname!("type").to_owned(), "get")
        .attr(minidom::rxml::xml_ncname!("id").to_owned(), request_id)
        .append(
            Element::builder("pubsub", NS_PUBSUB)
                .append(
                    Element::builder("items", NS_PUBSUB)
                        .attr(minidom::rxml::xml_ncname!("node").to_owned(), NS_BOOKMARKS)
                        .build(),
                )
                .build(),
        )
        .build()
}

/// Build a `set` IQ that publishes one bookmark item to the user's
/// own PEP `urn:xmpp:bookmarks:1` node, using the canonical XEP-0402
/// access model (publish-options `whitelist`/`max_items=max`/
/// `send_last_published_item=never`).
///
/// Includes the XEP-0402 §3.1 `publish-options` so the server creates
/// the node with the correct access model on first publish.
pub fn build_publish_bookmark_iq(item: &BookmarkItem, request_id: &str) -> Element {
    let publish_options = Element::builder("publish-options", NS_PUBSUB)
        .append(
            Element::builder("x", "jabber:x:data")
                .attr(minidom::rxml::xml_ncname!("type").to_owned(), "submit")
                .append(
                    Element::builder("field", "jabber:x:data")
                        .attr(minidom::rxml::xml_ncname!("var").to_owned(), "FORM_TYPE")
                        .attr(minidom::rxml::xml_ncname!("type").to_owned(), "hidden")
                        .append(
                            Element::builder("value", "jabber:x:data")
                                .append("http://jabber.org/protocol/pubsub#publish-options")
                                .build(),
                        )
                        .build(),
                )
                .append(submit_value_field("pubsub#persist_items", "true"))
                .append(submit_value_field("pubsub#max_items", "max"))
                .append(submit_value_field(
                    "pubsub#send_last_published_item",
                    "never",
                ))
                .append(submit_value_field("pubsub#access_model", "whitelist"))
                .build(),
        )
        .build();

    Element::builder("iq", NS_CLIENT)
        .attr(minidom::rxml::xml_ncname!("type").to_owned(), "set")
        .attr(minidom::rxml::xml_ncname!("id").to_owned(), request_id)
        .append(
            Element::builder("pubsub", NS_PUBSUB)
                .append(
                    Element::builder("publish", NS_PUBSUB)
                        .attr(minidom::rxml::xml_ncname!("node").to_owned(), NS_BOOKMARKS)
                        .append(
                            Element::builder("item", NS_PUBSUB)
                                .attr(
                                    minidom::rxml::xml_ncname!("id").to_owned(),
                                    item.jid.to_string(),
                                )
                                .append(item.build_conference_element())
                                .build(),
                        )
                        .build(),
                )
                .append(publish_options)
                .build(),
        )
        .build()
}

/// Extract every `<item/>` from a XEP-0060 items response targeting
/// `urn:xmpp:bookmarks:1`, returning them as typed [`BookmarkItem`]
/// values. Items with an unparseable id or missing conference payload
/// are skipped.
pub fn parse_bookmarks_response(iq: &Element) -> Vec<BookmarkItem> {
    let Some(items) = iq
        .get_child("pubsub", NS_PUBSUB)
        .and_then(|pubsub| pubsub.get_child("items", NS_PUBSUB))
        .filter(|items| items.attr("node") == Some(NS_BOOKMARKS))
    else {
        return Vec::new();
    };
    items
        .children()
        .filter(|child| child.name() == "item" && child.ns() == NS_PUBSUB)
        .filter_map(BookmarkItem::parse_item)
        .collect()
}

fn submit_value_field(var: &str, value: &str) -> Element {
    Element::builder("field", "jabber:x:data")
        .attr(minidom::rxml::xml_ncname!("var").to_owned(), var)
        .append(
            Element::builder("value", "jabber:x:data")
                .append(value)
                .build(),
        )
        .build()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::xep::xep0492::{
        merge_notify_into_extensions, read_fallback_mode, NotifyMode, NS_NOTIFICATION_SETTINGS,
    };

    #[test]
    fn parses_bookmark_item_with_notify_extension() {
        let xml = "<item xmlns='http://jabber.org/protocol/pubsub' id='theplay@conference.example.com'>\
                    <conference xmlns='urn:xmpp:bookmarks:1' name='The Play' autojoin='true'>\
                        <extensions>\
                            <notify xmlns='urn:xmpp:notification-settings:1'><on-mention /></notify>\
                        </extensions>\
                    </conference>\
                    </item>";
        let item: Element = xml.parse().expect("valid xml");
        let parsed = BookmarkItem::parse_item(&item).expect("parsed");
        assert_eq!(parsed.jid.to_string(), "theplay@conference.example.com");
        assert_eq!(parsed.name.as_deref(), Some("The Play"));
        assert!(parsed.autojoin);
        assert_eq!(parsed.extensions.len(), 1);
        assert!(parsed.extensions[0].is("notify", NS_NOTIFICATION_SETTINGS));
        assert_eq!(
            read_fallback_mode(&parsed.extensions[0]),
            Some(NotifyMode::OnMention)
        );
    }

    #[test]
    fn parse_drops_same_namespace_extensions_children() {
        // Round-12 Copilot review: XEP-0402 XSD `<extensions/>`
        // content is `<xs:any namespace='##other'>` — a same-ns
        // child is malformed. If we kept it on parse, a subsequent
        // republish (via set_room_notification_mode's fetch-merge-
        // publish) would re-emit the violation and the server
        // could reject the publish.
        let xml =
            "<item xmlns='http://jabber.org/protocol/pubsub' id='theplay@conference.example.com'>\
                    <conference xmlns='urn:xmpp:bookmarks:1' name='The Play'>\
                        <extensions>\
                            <invalid xmlns='urn:xmpp:bookmarks:1'/>\
                            <notify xmlns='urn:xmpp:notification-settings:1'><always /></notify>\
                        </extensions>\
                    </conference>\
                    </item>";
        let item: Element = xml.parse().expect("valid xml");
        let parsed = BookmarkItem::parse_item(&item).expect("parsed");
        // The malformed same-ns child is filtered out; only the
        // foreign-namespace `<notify/>` survives.
        assert_eq!(parsed.extensions.len(), 1);
        assert!(parsed.extensions[0].is("notify", "urn:xmpp:notification-settings:1",));
    }

    #[test]
    fn parses_bookmark_item_without_extensions() {
        let xml =
            "<item xmlns='http://jabber.org/protocol/pubsub' id='theplay@conference.example.com'>\
                    <conference xmlns='urn:xmpp:bookmarks:1' name='The Play' />\
                    </item>";
        let item: Element = xml.parse().expect("valid xml");
        let parsed = BookmarkItem::parse_item(&item).expect("parsed");
        assert!(!parsed.autojoin);
        assert!(parsed.extensions.is_empty());
    }

    #[test]
    fn parse_skips_item_with_invalid_jid_id() {
        let xml = "<item xmlns='http://jabber.org/protocol/pubsub' id='not a jid'>\
                    <conference xmlns='urn:xmpp:bookmarks:1' />\
                    </item>";
        let item: Element = xml.parse().expect("valid xml");
        assert!(BookmarkItem::parse_item(&item).is_none());
    }

    #[test]
    fn parse_skips_item_missing_conference() {
        let xml =
            "<item xmlns='http://jabber.org/protocol/pubsub' id='theplay@conference.example.com'/>";
        let item: Element = xml.parse().expect("valid xml");
        assert!(BookmarkItem::parse_item(&item).is_none());
    }

    #[test]
    fn build_conference_round_trips_extension_children() {
        let merged = merge_notify_into_extensions(None, NotifyMode::Never, false);
        let item = BookmarkItem {
            jid: "theplay@conference.example.com".parse().unwrap(),
            name: Some("The Play".into()),
            autojoin: true,
            nick: None,
            password: None,
            extensions: vec![merged.children().next().expect("notify child").clone()],
        };
        let conference = item.build_conference_element();
        let extensions = conference
            .get_child("extensions", NS_BOOKMARKS)
            .expect("extensions wrapper present");
        let notify = extensions
            .get_child("notify", NS_NOTIFICATION_SETTINGS)
            .expect("notify present");
        assert_eq!(read_fallback_mode(notify), Some(NotifyMode::Never));
    }

    #[test]
    fn build_conference_omits_autojoin_attr_when_false() {
        // Round-12 reviewer P2: XEP-0402 §2.2 / XSD default for
        // autojoin is `false`. We omit the attr entirely when
        // false so the wire shape matches the canonical "no
        // autojoin" form (an absent attr) — a regression that
        // flipped to always-emit `autojoin="false"` would silently
        // change every notification-mode publish's wire shape.
        let item = BookmarkItem {
            jid: "noauto@conference.example.com".parse().unwrap(),
            name: None,
            autojoin: false,
            nick: None,
            password: None,
            extensions: vec![],
        };
        let conference = item.build_conference_element();
        assert!(
            conference.attr("autojoin").is_none(),
            "autojoin MUST be omitted when false (XEP-0402 XSD default)"
        );
    }

    #[test]
    fn build_conference_emits_autojoin_true_when_set() {
        let item = BookmarkItem {
            jid: "auto@conference.example.com".parse().unwrap(),
            name: None,
            autojoin: true,
            nick: None,
            password: None,
            extensions: vec![],
        };
        let conference = item.build_conference_element();
        assert_eq!(conference.attr("autojoin"), Some("true"));
    }

    #[test]
    fn build_publish_bookmark_iq_omits_to_attr() {
        // Round-10 XMPP-conformance reviewer P3 — XEP-0163 §3.5: a
        // PEP publish from the bare-JID account targets the account's
        // own PEP service implicitly when `to=` is absent. Setting
        // `to=` to anything else would route the publish off the
        // user's own PEP node.
        let item = BookmarkItem {
            jid: "theplay@conference.example.com".parse().unwrap(),
            name: None,
            autojoin: false,
            nick: None,
            password: None,
            extensions: vec![],
        };
        let iq = build_publish_bookmark_iq(&item, "req-no-to");
        assert!(
            iq.attr("to").is_none(),
            "publish IQ MUST omit to= so the server routes to the account's own PEP service (XEP-0163 §3.5)"
        );
    }

    #[test]
    fn build_fetch_bookmarks_iq_omits_to_attr() {
        let iq = build_fetch_bookmarks_iq("req-fetch-no-to");
        assert!(
            iq.attr("to").is_none(),
            "fetch IQ MUST omit to= so the server routes to the account's own PEP service (XEP-0163 §3.5)"
        );
    }

    #[test]
    fn build_publish_bookmark_iq_uses_canonical_publish_options() {
        // Round-8 Rust reviewer P2: pin every publish-options field
        // so a regression that drops or mistypes one of them gets
        // caught.
        let item = BookmarkItem {
            jid: "theplay@conference.example.com".parse().unwrap(),
            name: Some("The Play".into()),
            autojoin: true,
            nick: None,
            password: None,
            extensions: vec![],
        };
        let iq = build_publish_bookmark_iq(&item, "req-1");
        let publish_options = iq
            .get_child("pubsub", NS_PUBSUB)
            .and_then(|p| p.get_child("publish-options", NS_PUBSUB))
            .expect("publish-options present");
        let form = publish_options
            .get_child("x", "jabber:x:data")
            .expect("x form present");
        assert_eq!(form.attr("type"), Some("submit"));

        let value_of = |var: &str| -> Option<String> {
            form.children()
                .find(|child| child.attr("var") == Some(var))
                .and_then(|field| field.get_child("value", "jabber:x:data"))
                .map(|value| value.text())
        };

        // FORM_TYPE marker for the publish-options form.
        assert_eq!(
            value_of("FORM_TYPE").as_deref(),
            Some("http://jabber.org/protocol/pubsub#publish-options"),
        );
        // XEP-0402 §3.1 canonical fields.
        assert_eq!(value_of("pubsub#persist_items").as_deref(), Some("true"));
        assert_eq!(value_of("pubsub#max_items").as_deref(), Some("max"));
        assert_eq!(
            value_of("pubsub#send_last_published_item").as_deref(),
            Some("never"),
        );
        assert_eq!(
            value_of("pubsub#access_model").as_deref(),
            Some("whitelist")
        );
    }

    #[test]
    fn round_trips_nick_and_password() {
        // Round-7 XEP-0402 reviewer P1 — a sibling client may have
        // written `<nick/>` / `<password/>` (XEP-0402 §2.2 + XSD).
        // Our notification-mode publish path must preserve them.
        let xml =
            "<item xmlns='http://jabber.org/protocol/pubsub' id='theplay@conference.example.com'>\
                    <conference xmlns='urn:xmpp:bookmarks:1' name='The Play' autojoin='true'>\
                        <nick>JC</nick>\
                        <password>cauldronburn</password>\
                    </conference>\
                    </item>";
        let item: Element = xml.parse().expect("valid xml");
        let parsed = BookmarkItem::parse_item(&item).expect("parsed");
        assert_eq!(parsed.nick.as_deref(), Some("JC"));
        assert_eq!(parsed.password.as_deref(), Some("cauldronburn"));

        // Round-trip: build_conference_element must re-emit them so
        // a republish doesn't drop them.
        let conference = parsed.build_conference_element();
        let nick = conference
            .get_child("nick", NS_BOOKMARKS)
            .map(|el| el.text());
        let password = conference
            .get_child("password", NS_BOOKMARKS)
            .map(|el| el.text());
        assert_eq!(nick.as_deref(), Some("JC"));
        assert_eq!(password.as_deref(), Some("cauldronburn"));
    }

    #[test]
    fn parse_accepts_numeric_autojoin_true() {
        // XEP-0402 XSD types autojoin as xs:boolean — both lexical
        // forms `true` and `1` are valid on the wire. The topology
        // bookmark merge keys join behaviour off this flag, so a
        // sibling client publishing `autojoin='1'` must not silently
        // lose autojoin.
        let xml =
            "<item xmlns='http://jabber.org/protocol/pubsub' id='theplay@conference.example.com'>\
                    <conference xmlns='urn:xmpp:bookmarks:1' autojoin='1' />\
                    </item>";
        let item: Element = xml.parse().expect("valid xml");
        let parsed = BookmarkItem::parse_item(&item).expect("parsed");
        assert!(parsed.autojoin);
    }

    #[test]
    fn parse_response_ignores_items_from_a_foreign_node() {
        // A response whose <items/> is not the urn:xmpp:bookmarks:1
        // node must parse to zero bookmarks — otherwise a mis-routed
        // pubsub reply could stamp bogus autojoin state onto the
        // discovered topology.
        let xml = "<iq xmlns='jabber:client' type='result'>\
                    <pubsub xmlns='http://jabber.org/protocol/pubsub'>\
                        <items node='urn:example:other'>\
                            <item id='one@conference.example.com'>\
                                <conference xmlns='urn:xmpp:bookmarks:1' name='One' autojoin='true' />\
                            </item>\
                        </items>\
                    </pubsub>\
                    </iq>";
        let iq: Element = xml.parse().expect("valid xml");
        assert!(parse_bookmarks_response(&iq).is_empty());
    }

    #[test]
    fn parse_response_extracts_multiple_items() {
        let xml = "<iq xmlns='jabber:client' type='result'>\
                    <pubsub xmlns='http://jabber.org/protocol/pubsub'>\
                        <items node='urn:xmpp:bookmarks:1'>\
                            <item id='one@conference.example.com'>\
                                <conference xmlns='urn:xmpp:bookmarks:1' name='One' />\
                            </item>\
                            <item id='two@conference.example.com'>\
                                <conference xmlns='urn:xmpp:bookmarks:1' name='Two' autojoin='true' />\
                            </item>\
                        </items>\
                    </pubsub>\
                    </iq>";
        let iq: Element = xml.parse().expect("valid xml");
        let parsed = parse_bookmarks_response(&iq);
        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed[0].name.as_deref(), Some("One"));
        assert!(!parsed[0].autojoin);
        assert_eq!(parsed[1].name.as_deref(), Some("Two"));
        assert!(parsed[1].autojoin);
    }
}
