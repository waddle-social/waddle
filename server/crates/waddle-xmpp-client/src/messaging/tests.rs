use super::namespaces::*;
use super::*;
use crate::error::ClientError;
use crate::request::StanzaId;
use minidom::Element;
use waddle_xmpp_core::CoreError;

fn el(xml: &str) -> Element {
    xml.parse().expect("invalid XML")
}

fn assert_origin_id_matches_message_id(stanza: &Element) {
    let stanza_id = stanza.attr("id").expect("message id");
    let origin_id = stanza
        .get_child("origin-id", NS_ORIGIN_ID)
        .expect("origin-id");
    assert_eq!(origin_id.attr("id"), Some(stanza_id));
}

#[test]
fn parse_message_with_body() {
    let e = el("<message xmlns='jabber:client' \
         from='room@conf.example/alice' \
         to='bob@example.com' \
         type='groupchat' \
         id='msg-1'>\
         <body>Hello world</body>\
         </message>");
    let ev = parse(&e).unwrap();
    let MessagingEvent::Message(msg) = ev else {
        panic!("expected Message");
    };
    assert_eq!(msg.id.as_deref(), Some("msg-1"));
    assert_eq!(msg.from.as_deref(), Some("room@conf.example/alice"));
    assert_eq!(msg.body.as_deref(), Some("Hello world"));
    assert_eq!(msg.message_type, "groupchat");
}

#[test]
fn parse_message_with_in_call_reaction_payload() {
    let e = el("<message xmlns='jabber:client' \
         from='room@conf.example/alice' \
         to='bob@example.com' \
         type='groupchat' \
         id='in-call-1'>\
         <in-call xmlns='urn:waddle:in-call:0' sid='call-1'>\
           <reaction emoji='🔥'/>\
         </in-call>\
         <no-store xmlns='urn:xmpp:hints'/>\
         <no-copy xmlns='urn:xmpp:hints'/>\
         </message>");
    let MessagingEvent::Message(msg) = parse(&e).unwrap() else {
        panic!("expected Message");
    };
    let Some(InCallSignal::Reaction(payload)) = msg.in_call else {
        panic!("expected in-call reaction signal");
    };
    assert_eq!(payload.sid.as_str(), "call-1");
    assert_eq!(payload.emoji.as_str(), "🔥");
}

#[test]
fn parse_message_with_waddle_extension_envelope() {
    let e = el("<message xmlns='jabber:client' \
         from='room@conf.example/github' \
         to='bob@example.com' \
         type='groupchat' \
         id='msg-extension'>\
         <body>GitHub waddle-social/waddle: ci completed with failure</body>\
         <fallback xmlns='urn:xmpp:fallback:0' for='urn:waddle:extension:1'>\
           <body/>\
         </fallback>\
         <extensions xmlns='urn:waddle:extension:1' version='1'>\
           <enrichment id='github-delivery-1' plugin='github' capability='message.enrich' payload-ns='urn:waddle:web-integration:1' created='2026-05-12T00:00:00Z'>\
             <source stanza-id='room-stanza-1'/>\
             <payload>\
               <view id='github-event' title='GitHub'>\
                 <text style='body'>GitHub waddle-social/waddle: ci completed with failure</text>\
               </view>\
               <github-event xmlns='urn:waddle:web-integration:1' event-type='workflow_run' repository='waddle-social/waddle' conclusion='failure' name='ci'>\
                 <detail xmlns='urn:example:webhook:detail' id='nested'/>\
               </github-event>\
             </payload>\
             <launch id='retry-1' plugin='github' action='retry' command-node='urn:waddle:extension:1:invoke' label='Retry' expires-at='2026-05-12T00:05:00Z' token='signed-token'>\
               <context waddle-id='github-delivery-1' room='room@conf.example' stanza-id='room-stanza-1'/>\
             </launch>\
           </enrichment>\
         </extensions>\
         </message>");
    let MessagingEvent::Message(msg) = parse(&e).unwrap() else {
        panic!("expected Message");
    };

    let envelope = msg.extension_envelope.expect("extension envelope");
    assert_eq!(envelope.version, 1);
    let enrichment = envelope.enrichments.first().expect("enrichment");
    assert_eq!(enrichment.plugin.as_str(), "github");
    assert_eq!(enrichment.capability.as_str(), "message.enrich");
    assert_eq!(enrichment.created.as_str(), "2026-05-12T00:00:00Z");
    assert_eq!(
        enrichment.title.as_ref().map(|title| title.as_str()),
        Some("GitHub")
    );
    assert_eq!(
        enrichment.payload_namespace.as_str(),
        "urn:waddle:web-integration:1"
    );
    assert_eq!(
        enrichment
            .source
            .as_ref()
            .expect("source")
            .stanza_id
            .as_str(),
        "room-stanza-1"
    );
    assert_eq!(enrichment.payloads.len(), 1);
    let payload = &enrichment.payloads[0];
    assert_eq!(payload.name.as_str(), "github-event");
    assert!(payload
        .attributes
        .iter()
        .any(|attribute| attribute.name.as_str() == "repository"
            && attribute.value == "waddle-social/waddle"));
    let nested = payload.children.first().expect("nested payload child");
    assert_eq!(nested.name.as_str(), "detail");
    assert_eq!(nested.namespace.as_str(), "urn:example:webhook:detail");
    assert!(nested
        .attributes
        .iter()
        .any(|attribute| attribute.name.as_str() == "id" && attribute.value == "nested"));
    assert_eq!(enrichment.launches.len(), 1);
    assert_eq!(enrichment.launches[0].id.as_str(), "retry-1");
    assert_eq!(
        enrichment.launches[0].command_node.as_str(),
        "urn:waddle:extension:1:invoke"
    );
    assert_eq!(enrichment.launches[0].label.as_str(), "Retry");
    assert_eq!(
        enrichment.launches[0]
            .expires_at
            .as_ref()
            .map(|timestamp| timestamp.as_str()),
        Some("2026-05-12T00:05:00Z")
    );
    assert_eq!(
        enrichment.launches[0]
            .context
            .room
            .as_ref()
            .map(|room| room.as_str()),
        Some("room@conf.example")
    );
    assert!(msg.extension_body_fallback);
}

#[test]
fn parse_message_keeps_body_when_extension_fallback_has_no_renderable_envelope() {
    let e = el("<message xmlns='jabber:client' \
         from='room@conf.example/github' \
         to='bob@example.com' \
         type='groupchat' \
         id='msg-extension'>\
         <body>GitHub waddle-social/waddle: ci completed with failure</body>\
         <fallback xmlns='urn:xmpp:fallback:0' for='urn:waddle:extension:1'>\
           <body/>\
         </fallback>\
         <extensions xmlns='urn:waddle:extension:1' version='1'>\
           <enrichment id='github-delivery-1' plugin='github' capability='message.observe' payload-ns='urn:waddle:web-integration:1' created='2026-05-12T00:00:00Z'/>\
         </extensions>\
         </message>");
    let MessagingEvent::Message(msg) = parse(&e).unwrap() else {
        panic!("expected Message");
    };

    assert!(msg.extension_envelope.is_none());
    assert!(!msg.extension_body_fallback);
}

#[test]
fn parse_message_with_delay() {
    use chrono::Datelike;
    let e = el("<message xmlns='jabber:client' type='groupchat' id='m2'>\
         <body>delayed</body>\
         <delay xmlns='urn:xmpp:delay' stamp='2024-01-15T10:00:00Z'/>\
         </message>");
    let MessagingEvent::Message(msg) = parse(&e).unwrap() else {
        panic!("expected Message");
    };
    let ts = msg.timestamp.unwrap();
    assert_eq!(ts.year(), 2024);
    assert_eq!(ts.month(), 1);
    assert_eq!(ts.day(), 15);
}

#[test]
fn parse_message_with_stanza_id() {
    let e = el("<message xmlns='jabber:client' type='groupchat' id='m3'>\
         <body>hi</body>\
         <stanza-id xmlns='urn:xmpp:sid:0' id='server-sid-42' by='room@conf.example'/>\
         </message>");
    let MessagingEvent::Message(msg) = parse(&e).unwrap() else {
        panic!("expected Message");
    };
    let stanza_id = msg.stanza_id.expect("stanza-id");
    assert_eq!(stanza_id.as_str(), "server-sid-42");
    assert_eq!(stanza_id.by.to_string(), "room@conf.example");
}

#[test]
fn parse_message_preserves_multiple_typed_stanza_ids() {
    let e = el("<message xmlns='jabber:client' type='groupchat' id='m3'>\
         <body>hi</body>\
         <stanza-id xmlns='urn:xmpp:sid:0' id='foreign-sid' by='archive.example'/>\
         <stanza-id xmlns='urn:xmpp:sid:0' id='room-sid' by='room@conf.example'/>\
         <stanza-id xmlns='urn:xmpp:sid:0' id='bad-sid' by=''/>\
         </message>");
    let MessagingEvent::Message(msg) = parse(&e).unwrap() else {
        panic!("expected Message");
    };
    let stanza_ids: Vec<(String, String)> = msg
        .stanza_ids
        .iter()
        .map(|stanza_id| (stanza_id.id.clone(), stanza_id.by.to_string()))
        .collect();
    assert_eq!(
        stanza_ids,
        vec![
            ("foreign-sid".to_string(), "archive.example".to_string()),
            ("room-sid".to_string(), "room@conf.example".to_string()),
        ]
    );
    assert_eq!(
        msg.stanza_id.as_ref().map(|id| id.as_str()),
        Some("foreign-sid")
    );
}

#[test]
fn parse_message_with_origin_id() {
    let e = Element::builder("message", NS_CLIENT)
        .attr(minidom::rxml::xml_ncname!("type").to_owned(), "chat")
        .attr(minidom::rxml::xml_ncname!("id").to_owned(), "m4")
        .append(Element::builder("body", NS_CLIENT).append("hi").build())
        .append(
            Element::builder("origin-id", NS_ORIGIN_ID)
                .attr(
                    minidom::rxml::xml_ncname!("id").to_owned(),
                    "client-origin-42",
                )
                .build(),
        )
        .build();
    let MessagingEvent::Message(msg) = parse(&e).unwrap() else {
        panic!("expected Message");
    };
    assert_eq!(msg.origin_id.as_deref(), Some("client-origin-42"));
}

#[test]
fn parse_message_with_chat_state() {
    let e = el(
        "<message xmlns='jabber:client' type='chat' to='alice@example.com'>\
         <composing xmlns='http://jabber.org/protocol/chatstates'/>\
         </message>",
    );
    let MessagingEvent::Message(msg) = parse(&e).unwrap() else {
        panic!("expected Message");
    };
    assert_eq!(msg.chat_state.as_deref(), Some("composing"));
}

#[test]
fn parse_message_with_displayed_marker() {
    let e = el("<message xmlns='jabber:client' type='groupchat'>\
         <displayed xmlns='urn:xmpp:chat-markers:0' id='orig-msg-id'/>\
         </message>");
    let MessagingEvent::Message(msg) = parse(&e).unwrap() else {
        panic!("expected Message");
    };
    assert_eq!(msg.displayed_marker_id.as_deref(), Some("orig-msg-id"));
}

#[test]
fn parse_message_with_markable_marker_request() {
    let e = el("<message xmlns='jabber:client' type='chat' id='msg-1'>\
         <body>hello</body>\
         <markable xmlns='urn:xmpp:chat-markers:0'/>\
         </message>");
    let MessagingEvent::Message(msg) = parse(&e).unwrap() else {
        panic!("expected Message");
    };
    assert!(msg.displayed_marker_requested);
    assert_eq!(msg.displayed_marker_id, None);
}

#[test]
fn parse_markable_marker_without_message_id_is_ignored() {
    let e = el("<message xmlns='jabber:client' type='chat'>\
         <body>hello</body>\
         <markable xmlns='urn:xmpp:chat-markers:0'/>\
         </message>");
    let MessagingEvent::Message(msg) = parse(&e).unwrap() else {
        panic!("expected Message");
    };
    assert!(!msg.displayed_marker_requested);
}

#[test]
fn parse_message_with_reply() {
    let e = el("<message xmlns='jabber:client' type='groupchat'>\
         <body>&gt; [quoted]\nresponse</body>\
         <reply xmlns='urn:xmpp:reply:0' id='target-id' to='sender@example.com'/>\
         </message>");
    let MessagingEvent::Message(msg) = parse(&e).unwrap() else {
        panic!("expected Message");
    };
    assert_eq!(msg.reply_to_id.as_deref(), Some("target-id"));
    assert_eq!(msg.reply_to_sender.as_deref(), Some("sender@example.com"));
    assert!(msg.reply_fallback.is_none());
}

#[test]
fn parse_message_with_reply_and_fallback() {
    let e = el("<message xmlns='jabber:client' type='groupchat'>\
         <body>&gt; alice wrote:\n&gt; original message\n\nmy response</body>\
         <reply xmlns='urn:xmpp:reply:0' id='orig-id' to='alice@example.com'/>\
         <fallback xmlns='urn:xmpp:fallback:0' for='urn:xmpp:reply:0'>\
             <body start='0' end='37'/>\
         </fallback>\
         </message>");
    let MessagingEvent::Message(msg) = parse(&e).unwrap() else {
        panic!("expected Message");
    };
    assert_eq!(msg.reply_to_id.as_deref(), Some("orig-id"));
    assert_eq!(msg.reply_to_sender.as_deref(), Some("alice@example.com"));
    assert_eq!(msg.reply_fallback, Some((0, 37)));
}

#[test]
fn parse_message_with_nested_thread() {
    let e = el("<message xmlns='jabber:client' type='groupchat'>\
         <body>nested reply</body>\
         <thread parent='root-abc'>child-xyz</thread>\
         </message>");
    let MessagingEvent::Message(msg) = parse(&e).unwrap() else {
        panic!("expected Message");
    };
    assert_eq!(msg.thread_id.as_deref(), Some("child-xyz"));
    assert_eq!(msg.parent_thread_id.as_deref(), Some("root-abc"));
}

#[test]
fn parse_message_with_call_thread_anchor() {
    let e = el("<message xmlns='jabber:client' from='room@muc.example' type='groupchat'>\
         <body>Alice started a call</body>\
         <thread>call-thread-uuid</thread>\
         <call-thread xmlns='urn:waddle:call-thread:0' kind='muc' sid='session-uuid' media='audio video' initiator='alice@example.com' started='2026-06-07T14:30:00Z'/>\
         </message>");
    let MessagingEvent::Message(msg) = parse(&e).unwrap() else {
        panic!("expected Message");
    };
    let call_thread = msg.call_thread.expect("call-thread marker");

    assert_eq!(msg.thread_id.as_deref(), Some("call-thread-uuid"));
    assert_eq!(
        call_thread.kind,
        crate::xep::call_thread::CallThreadKind::Muc
    );
    assert_eq!(call_thread.sid.0, "session-uuid");
    assert!(call_thread.media.audio);
    assert!(call_thread.media.video);
    assert_eq!(call_thread.initiator.to_string(), "alice@example.com");
    assert_eq!(
        call_thread.started.to_rfc3339(),
        "2026-06-07T14:30:00+00:00"
    );
}

#[test]
fn parse_message_with_call_thread_ended_fastening() {
    let e = el("<message xmlns='jabber:client' from='room@muc.example' type='groupchat'>\
         <apply-to xmlns='urn:xmpp:fasten:0' id='anchor-stanza-id'>\
           <call-thread-ended xmlns='urn:waddle:call-thread:0' ended='2026-06-07T14:35:00Z' duration='PT5M'/>\
         </apply-to>\
         <store xmlns='urn:xmpp:hints'/>\
         </message>");
    let MessagingEvent::Message(msg) = parse(&e).unwrap() else {
        panic!("expected Message");
    };
    let ended = msg.call_thread_ended.expect("call-thread-ended marker");

    assert_eq!(ended.anchor_id, "anchor-stanza-id");
    assert_eq!(ended.ended.to_rfc3339(), "2026-06-07T14:35:00+00:00");
    assert_eq!(ended.duration.as_str(), "PT5M");
}

#[test]
fn parse_message_with_retract() {
    let e = el("<message xmlns='jabber:client' type='groupchat'>\
         <retract xmlns='urn:xmpp:message-retract:1' id='old-msg-id'/>\
         </message>");
    let MessagingEvent::Message(msg) = parse(&e).unwrap() else {
        panic!("expected Message");
    };
    assert_eq!(msg.retracts_id.as_deref(), Some("old-msg-id"));
    assert!(!msg.is_retracted);
}

#[test]
fn parse_message_with_retracted_tombstone() {
    let e = el(
        "<message xmlns='jabber:client' type='groupchat' id='old-msg-id'>\
         <retracted xmlns='urn:xmpp:message-retract:1' id='retract-msg-id'/>\
         </message>",
    );
    let MessagingEvent::Message(msg) = parse(&e).unwrap() else {
        panic!("expected Message");
    };
    assert_eq!(msg.retracts_id, None);
    assert_eq!(msg.retraction_id.as_deref(), Some("retract-msg-id"));
    assert!(msg.is_retracted);
}

#[test]
fn parse_message_keeps_tombstone_with_malformed_moderated_by_jid() {
    let e = el(
        "<message xmlns='jabber:client' type='groupchat' id='old-msg-id'>\
         <retracted xmlns='urn:xmpp:message-retract:1' id='retract-msg-id'>\
             <moderated xmlns='urn:xmpp:message-moderate:1' by=''/>\
             <reason>cleanup</reason>\
         </retracted>\
         </message>",
    );
    let MessagingEvent::Message(msg) = parse(&e).unwrap() else {
        panic!("expected Message");
    };
    assert_eq!(msg.retracts_id, None);
    assert_eq!(msg.retraction_id.as_deref(), Some("retract-msg-id"));
    assert!(msg.is_retracted);
    assert_eq!(msg.moderated_by, None);
    assert_eq!(msg.moderation_reason.as_deref(), Some("cleanup"));
}

#[test]
fn parse_message_with_correction() {
    let e = el("<message xmlns='jabber:client' type='chat'>\
         <body>fixed</body>\
         <replace xmlns='urn:xmpp:message-correct:0' id='old-msg-id'/>\
         </message>");
    let MessagingEvent::Message(msg) = parse(&e).unwrap() else {
        panic!("expected Message");
    };
    assert_eq!(msg.replaces_id.as_deref(), Some("old-msg-id"));
}

#[test]
fn parse_message_with_moderation() {
    let e = el(
        "<message xmlns='jabber:client' type='groupchat' from='room@muc.example'>\
         <retract xmlns='urn:xmpp:message-retract:1' id='old-msg-id'>\
             <moderated xmlns='urn:xmpp:message-moderate:1' by='room@muc.example/moderator'/>\
             <reason>cleanup</reason>\
         </retract>\
         </message>",
    );
    let MessagingEvent::Message(msg) = parse(&e).unwrap() else {
        panic!("expected Message");
    };
    assert_eq!(msg.retracts_id.as_deref(), Some("old-msg-id"));
    assert_eq!(msg.moderation_target_id.as_deref(), Some("old-msg-id"));
    assert_eq!(
        msg.moderated_by.as_ref().map(|jid| jid.to_string()),
        Some("room@muc.example/moderator".to_string())
    );
    assert_eq!(msg.moderation_reason.as_deref(), Some("cleanup"));
}

#[test]
fn parse_message_discards_spoofed_moderation_from_occupant() {
    let e = el(
        "<message xmlns='jabber:client' type='groupchat' from='room@muc.example/spoofer'>\
         <body>/me attempted moderation</body>\
         <retract xmlns='urn:xmpp:message-retract:1' id='old-msg-id'>\
             <moderated xmlns='urn:xmpp:message-moderate:1' by='room@muc.example/spoofer'/>\
         </retract>\
         </message>",
    );
    assert!(parse(&e).is_none());
}

#[test]
fn parse_message_keeps_moderation_with_malformed_by_jid() {
    let e = el(
        "<message xmlns='jabber:client' type='groupchat' from='room@muc.example'>\
         <retract xmlns='urn:xmpp:message-retract:1' id='old-msg-id'>\
             <moderated xmlns='urn:xmpp:message-moderate:1' by=''/>\
         </retract>\
         </message>",
    );
    let MessagingEvent::Message(msg) = parse(&e).unwrap() else {
        panic!("expected Message");
    };
    assert_eq!(msg.retracts_id.as_deref(), Some("old-msg-id"));
    assert_eq!(msg.moderation_target_id.as_deref(), Some("old-msg-id"));
    assert_eq!(msg.moderated_by, None);
}

#[test]
fn parse_message_with_reactions() {
    let e = el("<message xmlns='jabber:client' type='groupchat'>\
         <reactions xmlns='urn:xmpp:reactions:0' id='target-msg'>\
         <reaction>👍</reaction>\
         <reaction>❤️</reaction>\
         </reactions>\
         </message>");
    let MessagingEvent::Message(msg) = parse(&e).unwrap() else {
        panic!("expected Message");
    };
    assert_eq!(msg.reaction_target_id.as_deref(), Some("target-msg"));
    assert_eq!(msg.reaction_emojis, vec!["👍", "❤️"]);
}

#[test]
fn parse_message_extracts_references_for_data_type() {
    let e = el(
        "<message xmlns='jabber:client' type='groupchat' id='m-data'>\
           <body>see https://example.com</body>\
           <reference xmlns='urn:xmpp:reference:0' type='data' \
              uri='https://example.com' begin='4' end='23' \
              anchor='https://example.com'/>\
         </message>",
    );
    let MessagingEvent::Message(msg) = parse(&e).unwrap() else {
        panic!("expected Message");
    };
    assert_eq!(
        msg.references,
        vec![ReferenceData {
            ref_type: "data".to_string(),
            uri: "https://example.com".to_string(),
            begin: 4,
            end: 23,
            anchor: Some("https://example.com".to_string()),
        }]
    );
    // `type=data` references that don't carry XEP-0446/0447 file metadata
    // must not pollute the shared_files projection.
    assert!(msg.shared_files.is_empty());
    // `type=data` references must not appear as mentions.
    assert!(msg.mention_uris.is_empty());
}

#[test]
fn parse_message_extracts_xep0511_link_preview() {
    let e = el(
        "<message xmlns='jabber:client' type='groupchat' id='m-link'>\
           <body>see https://the.link.example.com/what-was-linked-to</body>\
           <rdf:Description xmlns:rdf='http://www.w3.org/1999/02/22-rdf-syntax-ns#' xmlns:og='https://ogp.me/ns#' xmlns:ogi='https://ogp.me/ns#image:' rdf:about='https://the.link.example.com/what-was-linked-to'>\
             <og:title>The Best Webpage</og:title>\
             <og:description>This is a great webpage and you will really like it</og:description>\
             <og:url>https://example.com/canonical-url/for/what-was-linked-to</og:url>\
             <og:image>https://waddle.example/api/files/11111111-1111-4111-8111-111111111111/link-preview-86610c40efe63f0a46c58c4b605c164b4ffa3a3ad3f1dcf13e6ba4c59cb3ce16.png</og:image>\
             <ogi:type>image/png</ogi:type>\
             <ogi:width>640</ogi:width>\
             <ogi:height>360</ogi:height>\
             <ogi:alt>Article screenshot</ogi:alt>\
           </rdf:Description>\
         </message>",
    );
    let MessagingEvent::Message(msg) = parse(&e).unwrap() else {
        panic!("expected Message");
    };
    assert_eq!(msg.link_previews.len(), 1);
    assert_eq!(
        msg.link_previews[0].original_url.as_str(),
        "https://the.link.example.com/what-was-linked-to"
    );
    assert_eq!(
        msg.link_previews[0]
            .normalized_url
            .as_ref()
            .map(url::Url::as_str),
        Some("https://example.com/canonical-url/for/what-was-linked-to")
    );
    assert_eq!(
        msg.link_previews[0].title.as_deref(),
        Some("The Best Webpage")
    );
    let image = msg.link_previews[0].image.as_ref().expect("cached image");
    assert_eq!(
        image.url.as_str(),
        "https://waddle.example/api/files/11111111-1111-4111-8111-111111111111/link-preview-86610c40efe63f0a46c58c4b605c164b4ffa3a3ad3f1dcf13e6ba4c59cb3ce16.png"
    );
    assert_eq!(
        image.media_type,
        waddle_xmpp_core::PreviewImageMediaType::Png
    );
    assert_eq!(image.width, Some(640));
    assert_eq!(image.height, Some(360));
    assert_eq!(image.alt.as_deref(), Some("Article screenshot"));
}

#[test]
fn parses_og_video_player_embed_from_xep0511() {
    let e = el(
        "<message xmlns='jabber:client' type='groupchat' id='m-link'>\
           <body>see https://www.youtube.com/watch?v=429A_VugWW0</body>\
           <rdf:Description xmlns:rdf='http://www.w3.org/1999/02/22-rdf-syntax-ns#' xmlns:og='https://ogp.me/ns#' xmlns:ogv='https://ogp.me/ns#video:' rdf:about='https://www.youtube.com/watch?v=429A_VugWW0'>\
             <og:title>A video</og:title>\
             <og:video>https://www.youtube-nocookie.com/embed/429A_VugWW0</og:video>\
             <ogv:type>text/html</ogv:type>\
             <ogv:width>1280</ogv:width>\
             <ogv:height>720</ogv:height>\
           </rdf:Description>\
         </message>",
    );
    let MessagingEvent::Message(msg) = parse(&e).unwrap() else {
        panic!("expected Message");
    };
    assert_eq!(msg.link_previews.len(), 1);
    let preview = &msg.link_previews[0];
    let player = preview.player_embed.as_ref().expect("player embed");
    assert_eq!(
        player.url.as_str(),
        "https://www.youtube-nocookie.com/embed/429A_VugWW0"
    );
    assert_eq!(player.width, Some(1280));
    assert_eq!(player.height, Some(720));
}

#[test]
fn parses_og_video_native_media_from_xep0511() {
    let e = el(
        "<message xmlns='jabber:client' type='groupchat' id='m-link'>\
           <body>watch https://rawkode.academy/watch/yoke</body>\
           <rdf:Description xmlns:rdf='http://www.w3.org/1999/02/22-rdf-syntax-ns#' xmlns:og='https://ogp.me/ns#' xmlns:ogv='https://ogp.me/ns#video:' rdf:about='https://rawkode.academy/watch/yoke'>\
             <og:title>Hands-on Yoke</og:title>\
             <og:video>https://content.rawkode.academy/v/clip.mp4</og:video>\
             <ogv:secure_url>https://content.rawkode.academy/v/clip.mp4</ogv:secure_url>\
             <ogv:type>video/mp4</ogv:type>\
           </rdf:Description>\
         </message>",
    );
    let MessagingEvent::Message(msg) = parse(&e).unwrap() else {
        panic!("expected Message");
    };
    assert_eq!(msg.link_previews.len(), 1);
    let preview = &msg.link_previews[0];
    let video = preview.video.as_ref().expect("native video");
    assert_eq!(
        video.url.as_str(),
        "https://content.rawkode.academy/v/clip.mp4"
    );
    assert_eq!(
        video.media_type,
        waddle_xmpp_core::DirectVideoMediaType::Mp4
    );
    assert!(preview.player_embed.is_none());
}

#[test]
fn parses_native_video_type_with_codec_parameters() {
    // A conformant (e.g. federated) sender may carry codec parameters in
    // og:video:type; the parser must match on the MIME essence.
    let e = el(
        "<message xmlns='jabber:client' type='groupchat' id='m-link'>\
           <body>watch https://rawkode.academy/watch/yoke</body>\
           <rdf:Description xmlns:rdf='http://www.w3.org/1999/02/22-rdf-syntax-ns#' xmlns:og='https://ogp.me/ns#' xmlns:ogv='https://ogp.me/ns#video:' rdf:about='https://rawkode.academy/watch/yoke'>\
             <og:video>https://content.rawkode.academy/v/clip.mp4</og:video>\
             <ogv:type>video/mp4; codecs=\"avc1.42E01E\"</ogv:type>\
           </rdf:Description>\
         </message>",
    );
    let MessagingEvent::Message(msg) = parse(&e).unwrap() else {
        panic!("expected Message");
    };
    let video = msg.link_previews[0].video.as_ref().expect("native video");
    assert_eq!(
        video.media_type,
        waddle_xmpp_core::DirectVideoMediaType::Mp4
    );
}

#[test]
fn parses_native_video_prefers_secure_url() {
    // The server emits both og:video and ogv:secure_url; the parser must prefer
    // secure_url (distinct URL here so the preference is observable).
    let e = el(
        "<message xmlns='jabber:client' type='groupchat' id='m-link'>\
           <body>watch https://rawkode.academy/watch/yoke</body>\
           <rdf:Description xmlns:rdf='http://www.w3.org/1999/02/22-rdf-syntax-ns#' xmlns:og='https://ogp.me/ns#' xmlns:ogv='https://ogp.me/ns#video:' rdf:about='https://rawkode.academy/watch/yoke'>\
             <og:video>https://content.rawkode.academy/v/BASE.mp4</og:video>\
             <ogv:secure_url>https://content.rawkode.academy/v/SECURE.mp4</ogv:secure_url>\
             <ogv:type>video/mp4</ogv:type>\
           </rdf:Description>\
         </message>",
    );
    let MessagingEvent::Message(msg) = parse(&e).unwrap() else {
        panic!("expected Message");
    };
    let video = msg.link_previews[0].video.as_ref().expect("native video");
    assert_eq!(
        video.url.as_str(),
        "https://content.rawkode.academy/v/SECURE.mp4"
    );
}

#[test]
fn ignores_native_video_with_text_html_type() {
    // text/html stays a player embed, never a native video.
    let e = el(
        "<message xmlns='jabber:client' type='groupchat' id='m-link'>\
           <body>see https://www.youtube.com/watch?v=429A_VugWW0</body>\
           <rdf:Description xmlns:rdf='http://www.w3.org/1999/02/22-rdf-syntax-ns#' xmlns:og='https://ogp.me/ns#' xmlns:ogv='https://ogp.me/ns#video:' rdf:about='https://www.youtube.com/watch?v=429A_VugWW0'>\
             <og:video>https://www.youtube-nocookie.com/embed/429A_VugWW0</og:video>\
             <ogv:type>text/html</ogv:type>\
           </rdf:Description>\
         </message>",
    );
    let MessagingEvent::Message(msg) = parse(&e).unwrap() else {
        panic!("expected Message");
    };
    assert!(msg.link_previews[0].video.is_none());
    assert!(msg.link_previews[0].player_embed.is_some());
}

#[test]
fn parses_og_video_player_embed_prefers_secure_url() {
    // The server's `build_link_metadata_element` always emits BOTH `og:video`
    // and `ogv:secure_url`. This covers that primary path and proves the parser
    // prefers `secure_url` (distinct URL here so the preference is observable).
    let e = el(
        "<message xmlns='jabber:client' type='groupchat' id='m-link'>\
           <body>see https://www.youtube.com/watch?v=429A_VugWW0</body>\
           <rdf:Description xmlns:rdf='http://www.w3.org/1999/02/22-rdf-syntax-ns#' xmlns:og='https://ogp.me/ns#' xmlns:ogv='https://ogp.me/ns#video:' rdf:about='https://www.youtube.com/watch?v=429A_VugWW0'>\
             <og:title>A video</og:title>\
             <og:video>https://www.youtube-nocookie.com/embed/BASEURL</og:video>\
             <ogv:secure_url>https://www.youtube-nocookie.com/embed/SECURE</ogv:secure_url>\
             <ogv:type>text/html</ogv:type>\
           </rdf:Description>\
         </message>",
    );
    let MessagingEvent::Message(msg) = parse(&e).unwrap() else {
        panic!("expected Message");
    };
    let player = msg.link_previews[0]
        .player_embed
        .as_ref()
        .expect("player embed");
    assert_eq!(
        player.url.as_str(),
        "https://www.youtube-nocookie.com/embed/SECURE"
    );
}

#[test]
fn parse_message_accepts_loopback_http_xep0511_link_preview_image() {
    for cached_url in [
        "http://localhost:3000/api/files/11111111-1111-4111-8111-111111111111/link-preview-86610c40efe63f0a46c58c4b605c164b4ffa3a3ad3f1dcf13e6ba4c59cb3ce16.png",
        "http://127.0.0.1:3000/api/files/11111111-1111-4111-8111-111111111111/link-preview-86610c40efe63f0a46c58c4b605c164b4ffa3a3ad3f1dcf13e6ba4c59cb3ce16.png",
    ] {
        let e = el(&format!(
            "<message xmlns='jabber:client' type='groupchat' id='m-link'>\
               <body>see https://the.link.example.com/what-was-linked-to</body>\
               <rdf:Description xmlns:rdf='http://www.w3.org/1999/02/22-rdf-syntax-ns#' xmlns:og='https://ogp.me/ns#' xmlns:ogi='https://ogp.me/ns#image:' rdf:about='https://the.link.example.com/what-was-linked-to'>\
                 <og:title>The Best Webpage</og:title>\
                 <og:image>{cached_url}</og:image>\
                 <ogi:type>image/png</ogi:type>\
               </rdf:Description>\
             </message>"
        ));
        let MessagingEvent::Message(msg) = parse(&e).unwrap() else {
            panic!("expected Message");
        };
        assert_eq!(msg.link_previews.len(), 1);
        assert_eq!(
            msg.link_previews[0]
                .image
                .as_ref()
                .map(|image| image.url.as_str()),
            Some(cached_url)
        );
    }
}

#[test]
fn parse_message_rejects_non_loopback_http_xep0511_link_preview_image() {
    let e = el(
        "<message xmlns='jabber:client' type='groupchat' id='m-link'>\
           <body>see https://the.link.example.com/what-was-linked-to</body>\
           <rdf:Description xmlns:rdf='http://www.w3.org/1999/02/22-rdf-syntax-ns#' xmlns:og='https://ogp.me/ns#' xmlns:ogi='https://ogp.me/ns#image:' rdf:about='https://the.link.example.com/what-was-linked-to'>\
             <og:title>The Best Webpage</og:title>\
             <og:image>http://waddle.example/api/files/11111111-1111-4111-8111-111111111111/link-preview-86610c40efe63f0a46c58c4b605c164b4ffa3a3ad3f1dcf13e6ba4c59cb3ce16.png</og:image>\
             <ogi:type>image/png</ogi:type>\
           </rdf:Description>\
         </message>",
    );
    let MessagingEvent::Message(msg) = parse(&e).unwrap() else {
        panic!("expected Message");
    };
    assert_eq!(msg.link_previews.len(), 1);
    assert!(msg.link_previews[0].image.is_none());
}

#[test]
fn parse_message_ignores_xep0511_link_preview_image_with_unsafe_media_type() {
    let e = el(
        "<message xmlns='jabber:client' type='groupchat' id='m-link'>\
           <body>see https://the.link.example.com/what-was-linked-to</body>\
           <rdf:Description xmlns:rdf='http://www.w3.org/1999/02/22-rdf-syntax-ns#' xmlns:og='https://ogp.me/ns#' xmlns:ogi='https://ogp.me/ns#image:' rdf:about='https://the.link.example.com/what-was-linked-to'>\
             <og:title>The Best Webpage</og:title>\
             <og:image>https://waddle.example/api/files/11111111-1111-4111-8111-111111111111/link-preview-86610c40efe63f0a46c58c4b605c164b4ffa3a3ad3f1dcf13e6ba4c59cb3ce16.png</og:image>\
             <ogi:type>image/svg+xml</ogi:type>\
           </rdf:Description>\
         </message>",
    );
    let MessagingEvent::Message(msg) = parse(&e).unwrap() else {
        panic!("expected Message");
    };
    assert_eq!(msg.link_previews.len(), 1);
    assert!(msg.link_previews[0].image.is_none());
    assert!(msg.link_previews[0].remote_media_unavailable);
}

#[test]
fn parse_message_ignores_xep0511_link_preview_image_without_cached_media_path() {
    let e = el(
        "<message xmlns='jabber:client' type='groupchat' id='m-link'>\
           <body>see https://the.link.example.com/what-was-linked-to</body>\
           <rdf:Description xmlns:rdf='http://www.w3.org/1999/02/22-rdf-syntax-ns#' xmlns:og='https://ogp.me/ns#' xmlns:ogi='https://ogp.me/ns#image:' rdf:about='https://the.link.example.com/what-was-linked-to'>\
             <og:title>The Best Webpage</og:title>\
             <og:image>https://attacker.example/not-the-cache.png</og:image>\
             <ogi:type>image/png</ogi:type>\
           </rdf:Description>\
         </message>",
    );
    let MessagingEvent::Message(msg) = parse(&e).unwrap() else {
        panic!("expected Message");
    };
    assert_eq!(msg.link_previews.len(), 1);
    assert!(msg.link_previews[0].image.is_none());
    assert!(msg.link_previews[0].remote_media_unavailable);
}

#[test]
fn parse_message_marks_remote_xep0511_link_preview_image_unavailable() {
    let e = el(
        "<message xmlns='jabber:client' type='groupchat' id='m-link'>\
           <body>see https://the.link.example.com/what-was-linked-to</body>\
           <rdf:Description xmlns:rdf='http://www.w3.org/1999/02/22-rdf-syntax-ns#' xmlns:og='https://ogp.me/ns#' xmlns:ogi='https://ogp.me/ns#image:' rdf:about='https://the.link.example.com/what-was-linked-to'>\
             <og:title>The Best Webpage</og:title>\
             <og:image>https://remote.example/preview.png</og:image>\
             <ogi:type>image/png</ogi:type>\
           </rdf:Description>\
         </message>",
    );
    let MessagingEvent::Message(msg) = parse(&e).unwrap() else {
        panic!("expected Message");
    };
    assert_eq!(msg.link_previews.len(), 1);
    assert!(msg.link_previews[0].image.is_none());
    assert!(msg.link_previews[0].remote_media_unavailable);
}

#[test]
fn parse_message_rejects_xep0511_link_preview_with_unsafe_scheme() {
    let e = el(
        "<message xmlns='jabber:client' type='groupchat' id='m-link'>\
           <body>see this</body>\
           <rdf:Description xmlns:rdf='http://www.w3.org/1999/02/22-rdf-syntax-ns#' xmlns:og='https://ogp.me/ns#' rdf:about='javascript:alert(1)'>\
             <og:title>Bad</og:title>\
           </rdf:Description>\
         </message>",
    );
    let MessagingEvent::Message(msg) = parse(&e).unwrap() else {
        panic!("expected Message");
    };
    assert!(msg.link_previews.is_empty());
}

#[test]
fn parse_message_rejects_xep0511_link_preview_for_non_first_body_url() {
    let e = el(
        "<message xmlns='jabber:client' type='groupchat' id='m-link'>\
           <body>first https://first.example.com/ then https://second.example.com/</body>\
           <rdf:Description xmlns:rdf='http://www.w3.org/1999/02/22-rdf-syntax-ns#' xmlns:og='https://ogp.me/ns#' rdf:about='https://second.example.com/'>\
             <og:title>Second</og:title>\
           </rdf:Description>\
         </message>",
    );
    let MessagingEvent::Message(msg) = parse(&e).unwrap() else {
        panic!("expected Message");
    };
    assert!(msg.link_previews.is_empty());
}

#[test]
fn parse_message_ignores_reply_fallback_url_when_matching_xep0511_preview() {
    let fallback_prefix = "&gt; quoted \u{1f642} https://quoted.example.com/\n";
    let fallback_text = "> quoted \u{1f642} https://quoted.example.com/\n";
    let e = el(&format!(
        "<message xmlns='jabber:client' type='groupchat' id='m-link'>\
           <body>{fallback_prefix}see https://current.example.com/</body>\
           <reply xmlns='urn:xmpp:reply:0' id='orig-id' to='alice@example.com'/>\
           <fallback xmlns='urn:xmpp:fallback:0' for='urn:xmpp:reply:0'>\
             <body start='0' end='{}'/>\
           </fallback>\
           <rdf:Description xmlns:rdf='http://www.w3.org/1999/02/22-rdf-syntax-ns#' xmlns:og='https://ogp.me/ns#' rdf:about='https://current.example.com/'>\
             <og:title>Current</og:title>\
           </rdf:Description>\
         </message>",
        fallback_text.encode_utf16().count()
    ));
    let MessagingEvent::Message(msg) = parse(&e).unwrap() else {
        panic!("expected Message");
    };
    assert_eq!(msg.link_previews.len(), 1);
    assert_eq!(
        msg.link_previews[0].original_url.as_str(),
        "https://current.example.com/"
    );
}

#[test]
fn parse_message_strips_multiple_reply_fallback_ranges_before_matching_xep0511_preview() {
    let fallback_prefix = "&gt; quoted https://quoted.example.com/\n";
    let visible_body = "see https://current.example.com/";
    let fallback_suffix = "\n&gt; more https://later.example.com/";
    let body = format!("{fallback_prefix}{visible_body}{fallback_suffix}");
    let first_end = "> quoted https://quoted.example.com/\n"
        .encode_utf16()
        .count();
    let second_start = first_end + visible_body.encode_utf16().count();
    let second_end = second_start + "\n> more https://later.example.com/".encode_utf16().count();
    let e = el(&format!(
        "<message xmlns='jabber:client' type='groupchat' id='m-link'>\
           <body>{body}</body>\
           <reply xmlns='urn:xmpp:reply:0' id='orig-id' to='alice@example.com'/>\
           <fallback xmlns='urn:xmpp:fallback:0' for='urn:xmpp:reply:0'>\
             <body start='0' end='{first_end}'/>\
             <body start='{second_start}' end='{second_end}'/>\
           </fallback>\
           <rdf:Description xmlns:rdf='http://www.w3.org/1999/02/22-rdf-syntax-ns#' xmlns:og='https://ogp.me/ns#' rdf:about='https://current.example.com/'>\
             <og:title>Current</og:title>\
           </rdf:Description>\
         </message>"
    ));
    let MessagingEvent::Message(msg) = parse(&e).unwrap() else {
        panic!("expected Message");
    };

    assert_eq!(msg.link_previews.len(), 1);
    assert_eq!(
        msg.link_previews[0].original_url.as_str(),
        "https://current.example.com/"
    );
}

#[test]
fn parse_message_rejects_xep0511_preview_when_only_body_url_is_reply_fallback() {
    let e = el(
        "<message xmlns='jabber:client' type='groupchat' id='m-link'>\
           <body>https://quoted.example.com/</body>\
           <reply xmlns='urn:xmpp:reply:0' id='orig-id' to='alice@example.com'/>\
           <fallback xmlns='urn:xmpp:fallback:0' for='urn:xmpp:reply:0'>\
             <body/>\
           </fallback>\
           <rdf:Description xmlns:rdf='http://www.w3.org/1999/02/22-rdf-syntax-ns#' xmlns:og='https://ogp.me/ns#' rdf:about='https://quoted.example.com/'>\
             <og:title>Quoted</og:title>\
           </rdf:Description>\
         </message>",
    );
    let MessagingEvent::Message(msg) = parse(&e).unwrap() else {
        panic!("expected Message");
    };

    assert!(msg.link_previews.is_empty());
}

#[test]
fn parse_message_extracts_xep0511_link_preview_for_wrapped_first_body_url() {
    let e = el(
        "<message xmlns='jabber:client' type='groupchat' id='m-link'>\
           <body>see (https://the.link.example.com/what-was-linked-to).</body>\
           <rdf:Description xmlns:rdf='http://www.w3.org/1999/02/22-rdf-syntax-ns#' xmlns:og='https://ogp.me/ns#' rdf:about='https://the.link.example.com/what-was-linked-to'>\
             <og:title>The Best Webpage</og:title>\
           </rdf:Description>\
         </message>",
    );
    let MessagingEvent::Message(msg) = parse(&e).unwrap() else {
        panic!("expected Message");
    };
    assert_eq!(msg.link_previews.len(), 1);
    assert_eq!(
        msg.link_previews[0].original_url.as_str(),
        "https://the.link.example.com/what-was-linked-to"
    );
}

#[test]
fn parse_message_extracts_xep0511_link_preview_for_inline_punctuation_prefixed_body_url() {
    let e = el(
        "<message xmlns='jabber:client' type='groupchat' id='m-link'>\
           <body>see:https://the.link.example.com/what-was-linked-to</body>\
           <rdf:Description xmlns:rdf='http://www.w3.org/1999/02/22-rdf-syntax-ns#' xmlns:og='https://ogp.me/ns#' rdf:about='https://the.link.example.com/what-was-linked-to'>\
             <og:title>The Best Webpage</og:title>\
           </rdf:Description>\
         </message>",
    );
    let MessagingEvent::Message(msg) = parse(&e).unwrap() else {
        panic!("expected Message");
    };
    assert_eq!(msg.link_previews.len(), 1);
    assert_eq!(
        msg.link_previews[0].original_url.as_str(),
        "https://the.link.example.com/what-was-linked-to"
    );
}

#[test]
fn parse_message_extracts_xep0511_link_preview_for_quoted_first_body_url() {
    let e = el(
        "<message xmlns='jabber:client' type='groupchat' id='m-link'>\
           <body>see &quot;https://example.com&quot;</body>\
           <rdf:Description xmlns:rdf='http://www.w3.org/1999/02/22-rdf-syntax-ns#' xmlns:og='https://ogp.me/ns#' rdf:about='https://example.com/'>\
             <og:title>Example</og:title>\
           </rdf:Description>\
         </message>",
    );
    let MessagingEvent::Message(msg) = parse(&e).unwrap() else {
        panic!("expected Message");
    };
    assert_eq!(msg.link_previews.len(), 1);
    assert_eq!(
        msg.link_previews[0].original_url.as_str(),
        "https://example.com/"
    );
}

#[test]
fn parse_message_extracts_bodyless_xep0511_link_preview() {
    let e = el(
        "<message xmlns='jabber:client' type='groupchat' id='m-link'>\
           <rdf:Description xmlns:rdf='http://www.w3.org/1999/02/22-rdf-syntax-ns#' xmlns:og='https://ogp.me/ns#' rdf:about='https://example.com/article'>\
             <og:title>Example</og:title>\
           </rdf:Description>\
         </message>",
    );
    let MessagingEvent::Message(msg) = parse(&e).unwrap() else {
        panic!("expected Message");
    };
    assert_eq!(msg.link_previews.len(), 1);
    assert_eq!(
        msg.link_previews[0].original_url.as_str(),
        "https://example.com/article"
    );
}

#[test]
fn parse_message_rejects_bodyless_xep0511_link_preview_for_host_without_dot() {
    let e = el(
        "<message xmlns='jabber:client' type='groupchat' id='m-link'>\
           <rdf:Description xmlns:rdf='http://www.w3.org/1999/02/22-rdf-syntax-ns#' xmlns:og='https://ogp.me/ns#' rdf:about='https://localhost/article'>\
             <og:title>Localhost</og:title>\
           </rdf:Description>\
         </message>",
    );
    let MessagingEvent::Message(msg) = parse(&e).unwrap() else {
        panic!("expected Message");
    };
    assert!(msg.link_previews.is_empty());
}

#[test]
fn parse_message_rejects_xep0511_link_preview_with_bare_about_attribute() {
    let e = el(
        "<message xmlns='jabber:client' type='groupchat' id='m-link'>\
           <body>see https://example.com/</body>\
           <rdf:Description xmlns:rdf='http://www.w3.org/1999/02/22-rdf-syntax-ns#' xmlns:og='https://ogp.me/ns#' about='https://example.com/'>\
             <og:title>Example</og:title>\
           </rdf:Description>\
         </message>",
    );
    let MessagingEvent::Message(msg) = parse(&e).unwrap() else {
        panic!("expected Message");
    };
    assert!(msg.link_previews.is_empty());
}

#[test]
fn parse_message_rejects_xep0511_link_preview_without_rdf_about_attribute() {
    let e = el(
        "<message xmlns='jabber:client' type='groupchat' id='m-link'>\
           <body>see https://example.com/</body>\
           <rdf:Description xmlns:rdf='http://www.w3.org/1999/02/22-rdf-syntax-ns#' xmlns:og='https://ogp.me/ns#'>\
             <og:title>Example</og:title>\
           </rdf:Description>\
         </message>",
    );
    let MessagingEvent::Message(msg) = parse(&e).unwrap() else {
        panic!("expected Message");
    };
    assert!(msg.link_previews.is_empty());
}

#[test]
fn parse_message_extracts_references_for_mention_type() {
    let e = el(
        "<message xmlns='jabber:client' type='groupchat' id='m-mention'>\
           <body>hi @bob</body>\
           <reference xmlns='urn:xmpp:reference:0' type='mention' \
              uri='xmpp:bob@example.com' begin='3' end='7'/>\
         </message>",
    );
    let MessagingEvent::Message(msg) = parse(&e).unwrap() else {
        panic!("expected Message");
    };
    assert_eq!(
        msg.references,
        vec![ReferenceData {
            ref_type: "mention".to_string(),
            uri: "xmpp:bob@example.com".to_string(),
            begin: 3,
            end: 7,
            anchor: None,
        }]
    );
    // Mentions still also flow through the flat helper view.
    assert_eq!(msg.mention_uris, vec!["xmpp:bob@example.com".to_string()]);
}

#[test]
fn parse_message_extracts_references_for_unknown_type() {
    let e = el(
        "<message xmlns='jabber:client' type='groupchat' id='m-unknown'>\
           <body>see https://example.com</body>\
           <reference xmlns='urn:xmpp:reference:0' type='quote' \
              uri='xmpp:room@conf.example?message;id=abc' begin='0' end='3'/>\
         </message>",
    );
    let MessagingEvent::Message(msg) = parse(&e).unwrap() else {
        panic!("expected Message");
    };
    assert_eq!(msg.references.len(), 1);
    assert_eq!(msg.references[0].ref_type, "quote");
    assert!(msg.mention_uris.is_empty());
    assert!(msg.shared_files.is_empty());
}

#[test]
fn parse_message_reference_handles_missing_optional_attrs() {
    let e = el("<message xmlns='jabber:client' type='chat' id='m-min'>\
           <body>https://example.com</body>\
           <reference xmlns='urn:xmpp:reference:0' type='data' \
              uri='https://example.com'/>\
         </message>");
    let MessagingEvent::Message(msg) = parse(&e).unwrap() else {
        panic!("expected Message");
    };
    assert_eq!(
        msg.references,
        vec![ReferenceData {
            ref_type: "data".to_string(),
            uri: "https://example.com".to_string(),
            begin: 0,
            end: 0,
            anchor: None,
        }]
    );
}

#[test]
fn parse_message_reference_skipped_when_begin_or_end_unparseable() {
    // XEP-0372 says begin/end are optional, but if present they MUST be
    // numeric. Silently coercing "abc" to 0 would mis-position the
    // highlighted span — drop the reference instead.
    let e = el(
        "<message xmlns='jabber:client' type='chat' id='m-bad-offsets'>\
           <body>https://example.com</body>\
           <reference xmlns='urn:xmpp:reference:0' type='data' \
              uri='https://example.com' begin='abc' end='5'/>\
           <reference xmlns='urn:xmpp:reference:0' type='data' \
              uri='https://other.example' begin='0' end='not-a-number'/>\
         </message>",
    );
    let MessagingEvent::Message(msg) = parse(&e).unwrap() else {
        panic!("expected Message");
    };
    assert!(msg.references.is_empty());
}

#[test]
fn parse_message_reference_skipped_when_only_one_of_begin_end_is_present() {
    // begin/end form an all-or-nothing pair under XEP-0372: either both
    // are present (the reference points at a body substring) or both are
    // absent (anchor-only). A half-specified `begin="3"` with no `end` is
    // meaningless and would silently coerce `end` to 0, putting `end`
    // before `begin` — drop the reference instead.
    let e = el("<message xmlns='jabber:client' type='chat' id='m-half'>\
           <body>see https://example.com</body>\
           <reference xmlns='urn:xmpp:reference:0' type='data' \
              uri='https://example.com' begin='4'/>\
           <reference xmlns='urn:xmpp:reference:0' type='data' \
              uri='https://other.example' end='10'/>\
         </message>");
    let MessagingEvent::Message(msg) = parse(&e).unwrap() else {
        panic!("expected Message");
    };
    assert!(msg.references.is_empty());
}

#[test]
fn parse_message_reference_skipped_when_required_attr_missing() {
    // XEP-0372 requires both `type` and `uri`. References missing either
    // must be silently dropped — never returned with placeholder values.
    let e = el("<message xmlns='jabber:client' type='chat' id='m-bad'>\
           <body>https://example.com</body>\
           <reference xmlns='urn:xmpp:reference:0' type='data'/>\
           <reference xmlns='urn:xmpp:reference:0' uri='https://example.com'/>\
         </message>");
    let MessagingEvent::Message(msg) = parse(&e).unwrap() else {
        panic!("expected Message");
    };
    assert!(msg.references.is_empty());
}

#[test]
fn build_outbound_message_omits_begin_end_when_no_position() {
    // XEP-0372 says begin/end are optional. Anchor-only references that
    // do not point at a body substring must NOT carry begin="0" end="0"
    // — that would be interpreted as a real 0-length annotation at
    // offset 0 by a conformant receiver.
    let options = SendMessageOptions {
        references: vec![ReferenceData {
            ref_type: "data".to_string(),
            uri: "xmpp:room@conf.example?message;id=earlier-msg".to_string(),
            begin: 0,
            end: 0,
            anchor: Some("xmpp:alice@example.com".to_string()),
        }],
        ..Default::default()
    };

    let (_, stanza) =
        build_outbound_message("room@muc.example", "groupchat", "see above", &options).unwrap();

    let reference = stanza
        .get_child("reference", NS_REFERENCES)
        .expect("reference child");
    assert_eq!(reference.attr("type"), Some("data"));
    assert_eq!(
        reference.attr("uri"),
        Some("xmpp:room@conf.example?message;id=earlier-msg")
    );
    assert_eq!(reference.attr("anchor"), Some("xmpp:alice@example.com"));
    assert_eq!(reference.attr("begin"), None);
    assert_eq!(reference.attr("end"), None);
}

#[test]
fn build_outbound_message_emits_anchor_attribute_when_present() {
    let options = SendMessageOptions {
        references: vec![ReferenceData {
            ref_type: "data".to_string(),
            uri: "https://example.com".to_string(),
            begin: 4,
            end: 23,
            anchor: Some("example.com".to_string()),
        }],
        ..Default::default()
    };

    let (_, stanza) = build_outbound_message(
        "room@muc.example",
        "groupchat",
        "see https://example.com",
        &options,
    )
    .unwrap();

    let reference = stanza
        .get_child("reference", NS_REFERENCES)
        .expect("reference child");
    assert_eq!(reference.attr("anchor"), Some("example.com"));
    assert_eq!(reference.attr("type"), Some("data"));
    assert_eq!(reference.attr("uri"), Some("https://example.com"));
}

#[test]
fn build_outbound_chat_message_can_request_displayed_markers() {
    let options = SendMessageOptions {
        request_displayed_marker: true,
        ..Default::default()
    };

    let (_, stanza) =
        build_outbound_message("peer@example.com", "chat", "hello", &options).unwrap();

    assert!(stanza.attr("id").is_some());
    assert_origin_id_matches_message_id(&stanza);
    assert_eq!(
        stanza
            .children()
            .filter(|child| child.name() == "markable" && child.ns() == NS_CHAT_MARKERS)
            .count(),
        1
    );
}

#[test]
fn build_outbound_groupchat_message_can_request_displayed_markers() {
    let options = SendMessageOptions {
        request_displayed_marker: true,
        ..Default::default()
    };

    let (_, stanza) =
        build_outbound_message("room@muc.example", "groupchat", "hello", &options).unwrap();

    assert!(stanza.attr("id").is_some());
    assert_origin_id_matches_message_id(&stanza);
    assert_eq!(
        stanza
            .children()
            .filter(|child| child.name() == "markable" && child.ns() == NS_CHAT_MARKERS)
            .count(),
        1
    );
}

#[test]
fn build_outbound_message_omits_markable_without_request() {
    let (_, stanza) = build_outbound_message(
        "peer@example.com",
        "chat",
        "hello",
        &SendMessageOptions::default(),
    )
    .unwrap();

    assert!(stanza.get_child("markable", NS_CHAT_MARKERS).is_none());
}

#[test]
fn parse_message_with_direct_file_sharing() {
    let e = el("<message xmlns='jabber:client' type='chat'>\
           <body>https://files.example.com/report.pdf</body>\
           <file-sharing xmlns='urn:xmpp:sfs:0' disposition='inline'>\
             <file xmlns='urn:xmpp:file:metadata:0'>\
               <media-type>application/pdf</media-type>\
               <name>report.pdf</name>\
               <size>4096</size>\
             </file>\
             <sources>\
               <url-data xmlns='http://jabber.org/protocol/url-data' target='https://files.example.com/report.pdf'/>\
             </sources>\
           </file-sharing>\
         </message>");
    let MessagingEvent::Message(msg) = parse(&e).unwrap() else {
        panic!("expected Message");
    };
    assert_eq!(msg.shared_files.len(), 1);
    assert_eq!(
        msg.shared_files[0].url,
        "https://files.example.com/report.pdf"
    );
    assert_eq!(msg.shared_files[0].name.as_deref(), Some("report.pdf"));
    assert_eq!(
        msg.shared_files[0].media_type.as_deref(),
        Some("application/pdf")
    );
    assert_eq!(msg.shared_files[0].size, Some(4096));
    assert_eq!(
        msg.shared_files[0].disposition,
        SharedFileDisposition::Inline
    );
}

#[test]
fn parse_presence_available() {
    let e = el("<presence xmlns='jabber:client' from='alice@example.com'>\
         <show>away</show>\
         <status>Out for lunch</status>\
         </presence>");
    let MessagingEvent::Presence(p) = parse(&e).unwrap() else {
        panic!("expected Presence");
    };
    assert_eq!(p.show.as_deref(), Some("away"));
    assert_eq!(p.status.as_deref(), Some("Out for lunch"));
    assert!(p.presence_type.is_none());
}

#[test]
fn parse_presence_unavailable() {
    let e = el("<presence xmlns='jabber:client' \
         from='alice@example.com' \
         type='unavailable'/>");
    let MessagingEvent::Presence(p) = parse(&e).unwrap() else {
        panic!("expected Presence");
    };
    assert_eq!(p.presence_type.as_deref(), Some("unavailable"));
}

#[test]
fn parse_presence_with_hats() {
    let e = el("<presence xmlns='jabber:client' from='admin@room.example'>\
         <hats xmlns='urn:xmpp:hats:0'>\
         <hat uri='urn:example:hats:admin' title='Administrator'/>\
         <hat uri='urn:example:hats:mod' title='Moderator'/>\
         </hats>\
         </presence>");
    let MessagingEvent::Presence(p) = parse(&e).unwrap() else {
        panic!("expected Presence");
    };
    assert_eq!(p.hats.len(), 2);
    assert_eq!(p.hats[0].uri, "urn:example:hats:admin");
    assert_eq!(p.hats[0].title, "Administrator");
    assert_eq!(p.hats[1].title, "Moderator");
}

#[test]
fn parse_presence_with_muc_affiliation_and_role() {
    let e = el(
        "<presence xmlns='jabber:client' from='room@muc.example/alice'>\
         <x xmlns='http://jabber.org/protocol/muc#user'>\
         <item affiliation='owner' role='moderator' jid='alice@example.com/phone'/>\
         </x>\
         <x xmlns='vcard-temp:x:update'>\
         <photo>room-avatar-hash</photo>\
         </x>\
         </presence>",
    );
    let MessagingEvent::Presence(p) = parse(&e).unwrap() else {
        panic!("expected Presence");
    };
    assert_eq!(p.muc_affiliation, Some(MucAffiliation::Owner));
    assert_eq!(p.muc_role, Some(MucRole::Moderator));
    assert_eq!(p.muc_jid.as_deref(), Some("alice@example.com/phone"));
    assert_eq!(p.vcard_avatar.as_deref(), Some("room-avatar-hash"));
}

#[test]
fn build_outbound_message_with_shared_files() {
    let options = SendMessageOptions {
        shared_files: vec![SharedFile {
            url: "https://files.example.com/song.ogg".to_string(),
            name: Some("song.ogg".to_string()),
            media_type: Some("audio/ogg".to_string()),
            size: Some(1234),
            width: None,
            height: None,
            desc: None,
            hashes: Vec::new(),
            disposition: SharedFileDisposition::Inline,
            encrypted: None,
        }],
        ..Default::default()
    };
    let (stanza_id, stanza) =
        build_outbound_message("alice@example.com", "chat", "listen", &options).unwrap();
    assert_eq!(stanza.attr("id"), Some(stanza_id.as_str()));
    let file_sharing = stanza
        .get_child("file-sharing", NS_SFS)
        .expect("file-sharing child");
    assert_eq!(file_sharing.attr("disposition"), Some("inline"));
    let file = file_sharing
        .get_child("file", NS_FILE_METADATA)
        .expect("metadata child");
    assert_eq!(
        file.get_child("media-type", NS_FILE_METADATA)
            .map(|e| e.text()),
        Some("audio/ogg".to_string())
    );
    assert_eq!(
        file_sharing
            .get_child("sources", NS_SFS)
            .and_then(|sources| sources.get_child("url-data", NS_URL_DATA))
            .and_then(|url_data| url_data.attr("target")),
        Some("https://files.example.com/song.ogg")
    );
}

#[test]
fn build_outbound_message_with_sticker_marker_and_file_desc_hash() {
    // XEP-0449 "Sending a sticker from a sticker pack": the message
    // carries the fallback body, the XEP-0447 file-sharing payload
    // whose <file/> includes the lang-less <desc/> and the XEP-0300
    // hash from the pack item, plus <sticker pack='…'/>.
    use crate::stickers::{HashAlgo, PackId, PackRef, StickerHash, StickerMarker, NS_HASHES};
    const NS_STICKERS: &str = "urn:xmpp:stickers:0";
    let options = SendMessageOptions {
        shared_files: vec![SharedFile {
            url: "https://download.montague.lit/sticker_marsey_kiss.png".to_string(),
            name: None,
            media_type: Some("image/png".to_string()),
            size: Some(67016),
            width: Some(512),
            height: Some(512),
            desc: Some("😘".to_string()),
            hashes: vec![StickerHash {
                algo: HashAlgo::Sha256,
                value_b64: "gw+6xdCgOcvCYSKuQNrXH33lV9NMzuDf/s0huByCDsY=".to_string(),
            }],
            disposition: SharedFileDisposition::Inline,
            encrypted: None,
        }],
        sticker: Some(StickerMarker {
            pack: Some(PackRef {
                id: PackId::new("EpRv28DHHzFrE4zd+xaNpVb4").expect("pack id"),
                jid: None,
                node: None,
            }),
        }),
        ..Default::default()
    };
    let (_, stanza) =
        build_outbound_message("juliet@shakespeare.lit", "chat", "😘", &options).expect("builds");

    let sticker = stanza
        .get_child("sticker", NS_STICKERS)
        .expect("sticker marker present");
    assert_eq!(sticker.attr("pack"), Some("EpRv28DHHzFrE4zd+xaNpVb4"));
    assert_eq!(sticker.attr("jid"), None);
    assert_eq!(sticker.attr("node"), None);

    let file = stanza
        .get_child("file-sharing", NS_SFS)
        .and_then(|sharing| sharing.get_child("file", NS_FILE_METADATA))
        .expect("file metadata");
    assert_eq!(
        file.get_child("desc", NS_FILE_METADATA).map(|e| e.text()),
        Some("😘".to_string())
    );
    let hash = file.get_child("hash", NS_HASHES).expect("XEP-0300 hash");
    assert_eq!(hash.attr("algo"), Some("sha-256"));
    assert_eq!(hash.text(), "gw+6xdCgOcvCYSKuQNrXH33lV9NMzuDf/s0huByCDsY=");
}

#[test]
fn build_outbound_message_without_sticker_option_has_no_marker() {
    let options = SendMessageOptions::default();
    let (_, stanza) =
        build_outbound_message("juliet@shakespeare.lit", "chat", "hi", &options).expect("builds");
    assert!(stanza.get_child("sticker", "urn:xmpp:stickers:0").is_none());
}

#[test]
fn parse_message_reads_file_desc_and_hashes_into_shared_file() {
    // Inbound XEP-0449 "Sending a sticker" wire shape (xep-0449.xml
    // §send): the receiver surfaces the <desc/> fallback and the
    // content hash on the typed SharedFile; is_sticker keys on the
    // <sticker/> element exactly as before.
    use crate::stickers::HashAlgo;
    let xml = "<message xmlns='jabber:client' type='chat' from='romeo@montague.lit/pda' id='sharing-a-file'>\
        <body>😘</body>\
        <sticker xmlns='urn:xmpp:stickers:0'/>\
        <file-sharing xmlns='urn:xmpp:sfs:0'>\
            <file xmlns='urn:xmpp:file:metadata:0'>\
                <media-type>image/png</media-type>\
                <desc>😘</desc>\
                <desc xml:lang='en'>kissy face</desc>\
                <size>67016</size>\
                <hash xmlns='urn:xmpp:hashes:2' algo='sha-256'>gw+6xdCgOcvCYSKuQNrXH33lV9NMzuDf/s0huByCDsY=</hash>\
                <hash xmlns='urn:xmpp:hashes:2' algo='sha-1'>ignored</hash>\
            </file>\
            <sources>\
                <url-data xmlns='http://jabber.org/protocol/url-data' target='https://download.montague.lit/sticker_marsey_kiss.png'/>\
            </sources>\
        </file-sharing>\
    </message>";
    let element: Element = xml.parse().expect("valid xml");
    let MessagingEvent::Message(parsed) = parse(&element).expect("parses") else {
        panic!("expected message event");
    };
    assert!(parsed.is_sticker, "sticker classifier unchanged");
    assert_eq!(parsed.shared_files.len(), 1);
    let file = &parsed.shared_files[0];
    // Only the lang-less desc is the mandated fallback.
    assert_eq!(file.desc.as_deref(), Some("😘"));
    // Unknown hash algorithms are dropped at the typed boundary.
    assert_eq!(file.hashes.len(), 1);
    assert_eq!(file.hashes[0].algo, HashAlgo::Sha256);
    assert_eq!(
        file.hashes[0].value_b64,
        "gw+6xdCgOcvCYSKuQNrXH33lV9NMzuDf/s0huByCDsY="
    );
}

#[test]
fn build_outbound_message_with_link_preview_token_request() {
    let options = SendMessageOptions {
        link_preview_token: Some(LinkPreviewToken::new("preview-token-1").expect("token")),
        ..Default::default()
    };

    let (_stanza_id, stanza) = build_outbound_message(
        "room@muc.example",
        "groupchat",
        "see https://example.com",
        &options,
    )
    .unwrap();

    let request = stanza
        .get_child("preview-request", NS_WADDLE_LINK_PREVIEW)
        .expect("preview request");
    assert_eq!(request.attr("token"), Some("preview-token-1"));
}

#[test]
fn build_outbound_message_emits_xep0448_when_encrypted() {
    use crate::stickers::{HashAlgo, StickerHash, NS_HASHES};
    use crate::xep::encrypted_file::{Cipher, EncryptedFile, EncryptedHash, NS_ESFS};
    let url = "https://files.example.com/photo.jpg.enc";
    let options = SendMessageOptions {
        shared_files: vec![SharedFile {
            url: url.to_string(),
            name: Some("photo.jpg".to_string()),
            media_type: Some("image/jpeg".to_string()),
            size: Some(2048),
            width: Some(800),
            height: Some(600),
            desc: None,
            hashes: vec![StickerHash {
                algo: HashAlgo::Sha256,
                value_b64: "cGxhaW4taGFzaA==".to_string(),
            }],
            disposition: SharedFileDisposition::Inline,
            encrypted: Some(EncryptedFile {
                cipher: Cipher::Aes256GcmNoPadding,
                key_b64: "a2V5".to_string(),
                iv_b64: "aXY=".to_string(),
                hashes: vec![EncryptedHash {
                    algo: "sha-256".to_string(),
                    value_b64: "aGFzaA==".to_string(),
                }],
                sources: vec![url.to_string()],
            }),
        }],
        ..Default::default()
    };
    let (_, stanza) =
        build_outbound_message("bob@example.com", "chat", "see attached", &options).unwrap();

    // The plaintext metadata still rides on `<file-sharing/>` so peers
    // without encryption support see the filename and disposition.
    let file_sharing = stanza
        .get_child("file-sharing", NS_SFS)
        .expect("file-sharing child present");
    assert_eq!(file_sharing.attr("disposition"), Some("inline"));

    // XEP-0448 §2.1: the `<file/>` metadata of an encrypted send MUST
    // carry the plaintext content hash (distinct from the ciphertext
    // hash nested inside `<encrypted/>`).
    let plaintext_hash = file_sharing
        .get_child("file", NS_FILE_METADATA)
        .and_then(|file| file.get_child("hash", NS_HASHES))
        .expect("plaintext <hash/> inside <file/>");
    assert_eq!(plaintext_hash.attr("algo"), Some("sha-256"));
    assert_eq!(plaintext_hash.text(), "cGxhaW4taGFzaA==");

    let sources = file_sharing
        .get_child("sources", NS_SFS)
        .expect("file-sharing sources");
    assert!(
        sources.get_child("url-data", NS_URL_DATA).is_none(),
        "encrypted file-sharing must not expose a direct outer url-data source"
    );
    // The XEP-0448 envelope is nested in the XEP-0447 source list.
    let encrypted = sources
        .get_child("encrypted", NS_ESFS)
        .expect("encrypted source present");
    assert_eq!(
        encrypted.attr("cipher"),
        Some("urn:xmpp:ciphers:aes-256-gcm-nopadding:0")
    );
    assert_eq!(
        encrypted.get_child("key", NS_ESFS).map(|e| e.text()),
        Some("a2V5".to_string())
    );
    assert_eq!(
        encrypted.get_child("iv", NS_ESFS).map(|e| e.text()),
        Some("aXY=".to_string())
    );
    let encrypted_sources = encrypted
        .get_child("sources", NS_SFS)
        .expect("encrypted sources");
    let url_data = encrypted_sources
        .get_child("url-data", NS_URL_DATA)
        .expect("url-data inside encrypted sources");
    assert_eq!(url_data.attr("target"), Some(url));
    assert!(
        stanza.get_child("encrypted", NS_ESFS).is_none(),
        "encrypted must not be a direct message child"
    );
}

#[test]
fn build_outbound_message_uses_file_url_as_nested_encrypted_source_fallback() {
    use crate::stickers::{HashAlgo, StickerHash};
    use crate::xep::encrypted_file::{Cipher, EncryptedFile, NS_ESFS};
    let url = "https://files.example.com/photo.jpg.enc";
    let options = SendMessageOptions {
        shared_files: vec![SharedFile {
            url: url.to_string(),
            name: Some("photo.jpg".to_string()),
            media_type: Some("image/jpeg".to_string()),
            size: Some(2048),
            width: None,
            height: None,
            desc: None,
            hashes: vec![StickerHash {
                algo: HashAlgo::Sha256,
                value_b64: "cGxhaW4taGFzaA==".to_string(),
            }],
            disposition: SharedFileDisposition::Inline,
            encrypted: Some(EncryptedFile {
                cipher: Cipher::Aes256GcmNoPadding,
                key_b64: "a2V5".to_string(),
                iv_b64: "aXY=".to_string(),
                hashes: Vec::new(),
                sources: Vec::new(),
            }),
        }],
        ..Default::default()
    };
    let (_, stanza) =
        build_outbound_message("bob@example.com", "chat", "see attached", &options).unwrap();
    let sources = stanza
        .get_child("file-sharing", NS_SFS)
        .and_then(|file_sharing| file_sharing.get_child("sources", NS_SFS))
        .expect("file-sharing sources");

    assert!(
        sources.get_child("url-data", NS_URL_DATA).is_none(),
        "fallback ciphertext URL must still not be exposed as an outer source"
    );
    let nested_url = sources
        .get_child("encrypted", NS_ESFS)
        .and_then(|encrypted| encrypted.get_child("sources", NS_SFS))
        .and_then(|encrypted_sources| encrypted_sources.get_child("url-data", NS_URL_DATA))
        .and_then(|url_data| url_data.attr("target"));
    assert_eq!(nested_url, Some(url));
}

#[test]
fn build_outbound_message_uses_file_url_as_primary_nested_encrypted_source() {
    use crate::stickers::{HashAlgo, StickerHash};
    use crate::xep::encrypted_file::{Cipher, EncryptedFile, NS_ESFS};
    let url = "https://files.example.com/photo.jpg.enc";
    let mirror_url = "https://mirror.example.com/photo.jpg.enc";
    let options = SendMessageOptions {
        shared_files: vec![SharedFile {
            url: url.to_string(),
            name: Some("photo.jpg".to_string()),
            media_type: Some("image/jpeg".to_string()),
            size: Some(2048),
            width: None,
            height: None,
            desc: None,
            hashes: vec![StickerHash {
                algo: HashAlgo::Sha256,
                value_b64: "cGxhaW4taGFzaA==".to_string(),
            }],
            disposition: SharedFileDisposition::Inline,
            encrypted: Some(EncryptedFile {
                cipher: Cipher::Aes256GcmNoPadding,
                key_b64: "a2V5".to_string(),
                iv_b64: "aXY=".to_string(),
                hashes: Vec::new(),
                sources: vec![mirror_url.to_string()],
            }),
        }],
        ..Default::default()
    };
    let (_, stanza) =
        build_outbound_message("bob@example.com", "chat", "see attached", &options).unwrap();
    let encrypted_sources = stanza
        .get_child("file-sharing", NS_SFS)
        .and_then(|file_sharing| file_sharing.get_child("sources", NS_SFS))
        .and_then(|sources| sources.get_child("encrypted", NS_ESFS))
        .and_then(|encrypted| encrypted.get_child("sources", NS_SFS))
        .expect("encrypted sources");

    let targets: Vec<_> = encrypted_sources
        .children()
        .filter(|child| child.is("url-data", NS_URL_DATA))
        .filter_map(|url_data| url_data.attr("target"))
        .collect();
    assert_eq!(targets, vec![url, mirror_url]);
}

#[test]
fn build_outbound_message_rejects_encrypted_file_without_usable_source() {
    use crate::stickers::{HashAlgo, StickerHash};
    use crate::xep::encrypted_file::{Cipher, EncryptedFile};
    let options = SendMessageOptions {
        shared_files: vec![SharedFile {
            url: "  ".to_string(),
            name: Some("photo.jpg".to_string()),
            media_type: Some("image/jpeg".to_string()),
            size: Some(2048),
            width: None,
            height: None,
            desc: None,
            hashes: vec![StickerHash {
                algo: HashAlgo::Sha256,
                value_b64: "cGxhaW4taGFzaA==".to_string(),
            }],
            disposition: SharedFileDisposition::Inline,
            encrypted: Some(EncryptedFile {
                cipher: Cipher::Aes256GcmNoPadding,
                key_b64: "a2V5".to_string(),
                iv_b64: "aXY=".to_string(),
                hashes: Vec::new(),
                sources: vec![" \t ".to_string()],
            }),
        }],
        ..Default::default()
    };
    let error = build_outbound_message("bob@example.com", "chat", "see attached", &options)
        .expect_err("encrypted file without a ciphertext source must fail");
    assert!(
        matches!(
            error,
            ClientError::Core(CoreError::BadRequest(Some(ref text)))
                if text == "encrypted shared file requires a ciphertext source URL"
        ),
        "unexpected error: {error:?}"
    );
}

#[test]
fn build_outbound_message_rejects_encrypted_file_without_plaintext_hash() {
    // XEP-0448 §2.1: the `<file/>` metadata MUST contain at least one
    // plaintext `<hash/>` for encrypted transfers.
    use crate::xep::encrypted_file::{Cipher, EncryptedFile, EncryptedHash};
    let url = "https://files.example.com/photo.jpg.enc";
    let options = SendMessageOptions {
        shared_files: vec![SharedFile {
            url: url.to_string(),
            name: Some("photo.jpg".to_string()),
            media_type: Some("image/jpeg".to_string()),
            size: Some(2048),
            width: None,
            height: None,
            desc: None,
            hashes: Vec::new(),
            disposition: SharedFileDisposition::Inline,
            encrypted: Some(EncryptedFile {
                cipher: Cipher::Aes256GcmNoPadding,
                key_b64: "a2V5".to_string(),
                iv_b64: "aXY=".to_string(),
                hashes: vec![EncryptedHash {
                    algo: "sha-256".to_string(),
                    value_b64: "aGFzaA==".to_string(),
                }],
                sources: vec![url.to_string()],
            }),
        }],
        ..Default::default()
    };
    let error = build_outbound_message("bob@example.com", "chat", "see attached", &options)
        .expect_err("encrypted file without a plaintext hash must fail");
    assert!(
        matches!(
            error,
            ClientError::Core(CoreError::BadRequest(Some(ref text)))
                if text == "encrypted shared file requires a plaintext <hash/> in its file metadata"
        ),
        "unexpected error: {error:?}"
    );
}

#[test]
fn parse_message_collects_encrypted_metadata_into_shared_file() {
    let url = "https://files.example.com/photo.jpg.enc";
    let xml = format!(
        "<message xmlns='jabber:client' type='chat' from='bob@example.com' id='m1'>\
           <body>see attached</body>\
           <file-sharing xmlns='urn:xmpp:sfs:0' disposition='inline'>\
             <file xmlns='urn:xmpp:file:metadata:0'>\
               <media-type>image/jpeg</media-type>\
               <name>photo.jpg</name>\
               <size>2048</size>\
               <hash xmlns='urn:xmpp:hashes:2' algo='sha-256'>cGxhaW4taGFzaA==</hash>\
             </file>\
             <sources xmlns='urn:xmpp:sfs:0'>\
               <url-data xmlns='http://jabber.org/protocol/url-data' target='{url}'/>\
               <encrypted xmlns='urn:xmpp:esfs:0' \
                          cipher='urn:xmpp:ciphers:aes-256-gcm-nopadding:0'>\
                 <key>a2V5</key>\
                 <iv>aXY=</iv>\
                 <hash xmlns='urn:xmpp:hashes:2' algo='sha-256'>aGFzaA==</hash>\
                 <sources xmlns='urn:xmpp:sfs:0'>\
                   <url-data xmlns='http://jabber.org/protocol/url-data' target='{url}'/>\
                 </sources>\
               </encrypted>\
             </sources>\
           </file-sharing>\
         </message>"
    );
    let el: Element = xml.parse().expect("valid xml");
    let MessagingEvent::Message(parsed) = parse(&el).expect("parses to event") else {
        panic!("expected message event");
    };
    assert_eq!(parsed.shared_files.len(), 1);
    let file = &parsed.shared_files[0];
    assert_eq!(file.url, url);
    assert_eq!(file.media_type.as_deref(), Some("image/jpeg"));
    // The XEP-0448-mandated plaintext hash inside <file/> parses into the
    // typed file-level hashes, distinct from the ciphertext hash below.
    assert_eq!(file.hashes.len(), 1);
    assert_eq!(file.hashes[0].algo, crate::stickers::HashAlgo::Sha256);
    assert_eq!(file.hashes[0].value_b64, "cGxhaW4taGFzaA==");
    let encrypted = file
        .encrypted
        .as_ref()
        .expect("encrypted metadata attached");
    assert_eq!(
        encrypted.cipher,
        crate::xep::encrypted_file::Cipher::Aes256GcmNoPadding
    );
    assert_eq!(encrypted.key_b64, "a2V5");
    assert_eq!(encrypted.iv_b64, "aXY=");
    assert_eq!(encrypted.hashes.len(), 1);
    assert_eq!(encrypted.hashes[0].algo, "sha-256");
    assert_eq!(encrypted.sources, vec![url.to_string()]);
}

#[test]
fn parse_message_leaves_encrypted_none_for_plaintext_share() {
    let xml = "<message xmlns='jabber:client' type='chat' from='bob@example.com' id='m1'>\
           <body>plain</body>\
           <file-sharing xmlns='urn:xmpp:sfs:0' disposition='attachment'>\
             <file xmlns='urn:xmpp:file:metadata:0'>\
               <name>doc.pdf</name>\
             </file>\
             <sources xmlns='urn:xmpp:sfs:0'>\
               <url-data xmlns='http://jabber.org/protocol/url-data' \
                         target='https://files.example.com/doc.pdf'/>\
             </sources>\
           </file-sharing>\
         </message>";
    let el: Element = xml.parse().expect("valid xml");
    let MessagingEvent::Message(parsed) = parse(&el).expect("parses to event") else {
        panic!("expected message event");
    };
    assert!(parsed.shared_files[0].encrypted.is_none());
}

#[test]
fn parse_message_ignores_top_level_encrypted_for_sims_share() {
    // XEP-0448 is scoped to XEP-0447 file-sharing sources. A top-level
    // encrypted sibling must not annotate legacy SIMS shares.
    let url = "https://files.example.com/sims-blob.enc";
    let xml = format!(
        "<message xmlns='jabber:client' type='chat' from='bob@example.com' id='m1'>\
           <body>see attached</body>\
           <reference xmlns='urn:xmpp:reference:0' type='data' \
                      uri='{url}' begin='0' end='0'>\
             <media-sharing xmlns='urn:xmpp:sims:1'>\
               <sources>\
                 <reference xmlns='urn:xmpp:reference:0' type='data' url='{url}'/>\
               </sources>\
             </media-sharing>\
           </reference>\
           <encrypted xmlns='urn:xmpp:esfs:0' \
                      cipher='urn:xmpp:ciphers:aes-256-gcm-nopadding:0'>\
             <key>a2V5</key>\
             <iv>aXY=</iv>\
             <sources xmlns='urn:xmpp:sfs:0'>\
               <url-data xmlns='http://jabber.org/protocol/url-data' target='{url}'/>\
             </sources>\
           </encrypted>\
         </message>"
    );
    let el: Element = xml.parse().expect("valid xml");
    let MessagingEvent::Message(parsed) = parse(&el).expect("parses to event") else {
        panic!("expected message event");
    };
    assert!(
        parsed
            .shared_files
            .iter()
            .all(|file| file.url != url && file.encrypted.is_none()),
        "top-level encrypted payload must not synthesize or annotate a SIMS share"
    );
}

#[test]
fn parse_message_ignores_orphan_encrypted_sibling() {
    // XEP-0448 encrypted metadata is only valid inside
    // `<file-sharing><sources/>`.
    let url = "https://files.example.com/orphan.enc";
    let xml = format!(
        "<message xmlns='jabber:client' type='chat' from='bob@example.com' id='m1'>\
           <body>orphan</body>\
           <encrypted xmlns='urn:xmpp:esfs:0' \
                      cipher='urn:xmpp:ciphers:aes-128-gcm-nopadding:0'>\
             <key>a2V5</key>\
             <iv>aXY=</iv>\
             <sources xmlns='urn:xmpp:sfs:0'>\
               <url-data xmlns='http://jabber.org/protocol/url-data' target='{url}'/>\
             </sources>\
           </encrypted>\
         </message>"
    );
    let el: Element = xml.parse().expect("valid xml");
    let MessagingEvent::Message(parsed) = parse(&el).expect("parses to event") else {
        panic!("expected message event");
    };
    assert!(parsed.shared_files.is_empty());
}

#[test]
fn build_then_parse_roundtrip_preserves_encrypted_metadata() {
    use crate::stickers::{HashAlgo, StickerHash};
    use crate::xep::encrypted_file::{Cipher, EncryptedFile, EncryptedHash};
    let url = "https://files.example.com/blob.enc";
    let original = SharedFile {
        url: url.to_string(),
        name: Some("blob.bin".to_string()),
        media_type: Some("application/octet-stream".to_string()),
        size: Some(99),
        width: None,
        height: None,
        desc: None,
        hashes: vec![StickerHash {
            algo: HashAlgo::Sha256,
            value_b64: "cGxhaW4taGFzaA==".to_string(),
        }],
        disposition: SharedFileDisposition::Attachment,
        encrypted: Some(EncryptedFile {
            cipher: Cipher::Aes128GcmNoPadding,
            key_b64: "k1".to_string(),
            iv_b64: "i1".to_string(),
            hashes: vec![EncryptedHash {
                algo: "sha-256".to_string(),
                value_b64: "h1".to_string(),
            }],
            sources: vec![url.to_string()],
        }),
    };
    let options = SendMessageOptions {
        shared_files: vec![original.clone()],
        ..Default::default()
    };
    let (_, stanza) = build_outbound_message("bob@example.com", "chat", "x", &options).unwrap();
    let MessagingEvent::Message(parsed) = parse(&stanza).expect("parses to event") else {
        panic!("expected message event");
    };
    assert_eq!(parsed.shared_files.len(), 1);
    let round_tripped = &parsed.shared_files[0];
    assert_eq!(round_tripped.url, original.url);
    assert_eq!(round_tripped.hashes, original.hashes);
    assert_eq!(round_tripped.encrypted, original.encrypted);
}

#[test]
fn build_outbound_message_uses_caller_stanza_id() {
    let stanza_id = StanzaId::new("client-visible-1").unwrap();
    let options = SendMessageOptions {
        stanza_id: Some(stanza_id.clone()),
        ..Default::default()
    };

    let (returned_id, stanza) =
        build_outbound_message("room@muc.example", "groupchat", "hello", &options).unwrap();

    assert_eq!(returned_id, stanza_id);
    assert_eq!(stanza.attr("id"), Some("client-visible-1"));
}

#[test]
fn build_outbound_chat_messages_stamp_origin_id_matching_stanza_id() {
    for message_type in ["chat", "groupchat"] {
        let stanza_id = StanzaId::new(format!("{message_type}-client-id")).unwrap();
        let options = SendMessageOptions {
            stanza_id: Some(stanza_id.clone()),
            ..Default::default()
        };

        let (_, stanza) =
            build_outbound_message("room@muc.example", message_type, "hello", &options).unwrap();
        let origin_id = stanza
            .get_child("origin-id", NS_ORIGIN_ID)
            .expect("origin-id");

        assert_eq!(origin_id.attr("id"), Some(stanza_id.as_str()));
        assert_eq!(origin_id.children().count(), 0);
        assert!(origin_id.text().is_empty());
    }
}

#[test]
fn build_outbound_message_with_markup_spans_and_references() {
    let options = SendMessageOptions {
        markup_spans: vec![
            MarkupSpanData {
                span_type: "bold".to_string(),
                start: 0,
                end: 5,
                uri: None,
            },
            // XEP-0394: links use <span uri="..."/> attribute, no child element
            MarkupSpanData {
                span_type: "link".to_string(),
                start: 6,
                end: 10,
                uri: Some("xmpp:bob@example.com".to_string()),
            },
        ],
        references: vec![ReferenceData {
            ref_type: "mention".to_string(),
            uri: "xmpp:bob@example.com".to_string(),
            begin: 6,
            end: 10,
            anchor: None,
        }],
        ..Default::default()
    };

    let (_, stanza) =
        build_outbound_message("room@muc.example", "groupchat", "hello @bob", &options).unwrap();

    let markups = stanza
        .get_child("markup", NS_MARKUP)
        .expect("markups child");
    let spans = markups
        .children()
        .filter(|child| child.name() == "span")
        .collect::<Vec<_>>();
    assert_eq!(spans.len(), 2);
    // Bold: <span start="0" end="5"><strong/></span>
    assert!(spans[0].get_child("strong", NS_MARKUP).is_some());
    // Link: <span start="6" end="10" uri="xmpp:bob@example.com"/> in urn:waddle:markup:0
    assert_eq!(spans[1].ns(), NS_WADDLE_MARKUP);
    assert_eq!(spans[1].attr("uri"), Some("xmpp:bob@example.com"));
    assert!(spans[1].get_child("link", NS_MARKUP).is_none());
    let reference = stanza
        .get_child("reference", NS_REFERENCES)
        .expect("reference child");
    assert_eq!(reference.attr("type"), Some("mention"));
    assert_eq!(reference.attr("begin"), Some("6"));
    assert_eq!(reference.attr("end"), Some("10"));
    assert_eq!(reference.attr("uri"), Some("xmpp:bob@example.com"));
}

#[test]
fn build_outbound_message_block_markup_uses_xep0394_elements() {
    // XEP-0394: code blocks use <bcode/> and blockquotes use <bquote/> as
    // siblings of <span>, not wrapped inside a <span> element.
    let options = SendMessageOptions {
        markup_spans: vec![
            MarkupSpanData {
                span_type: "code_block".to_string(),
                start: 0,
                end: 20,
                uri: None,
            },
            MarkupSpanData {
                span_type: "blockquote".to_string(),
                start: 21,
                end: 45,
                uri: None,
            },
        ],
        ..Default::default()
    };

    let (_, stanza) = build_outbound_message(
        "room@muc.example",
        "groupchat",
        "```code``` > quote",
        &options,
    )
    .unwrap();

    let markups = stanza
        .get_child("markup", NS_MARKUP)
        .expect("markups child");
    // Block elements must NOT be wrapped in <span>
    assert_eq!(
        markups.children().filter(|c| c.name() == "span").count(),
        0,
        "code_block and blockquote must not be wrapped in <span>"
    );
    // <bcode start="0" end="20"/>
    let bcode = markups
        .get_child("bcode", NS_MARKUP)
        .expect("bcode element");
    assert_eq!(bcode.attr("start"), Some("0"));
    assert_eq!(bcode.attr("end"), Some("20"));
    // <bquote start="21" end="45"/>
    let bquote = markups
        .get_child("bquote", NS_MARKUP)
        .expect("bquote element");
    assert_eq!(bquote.attr("start"), Some("21"));
    assert_eq!(bquote.attr("end"), Some("45"));
}

#[test]
fn xep0394_markup_roundtrip_parse_and_build() {
    // Build a message with all markup types, then parse the resulting stanza
    // and verify the roundtrip produces the same MarkupSpan values.
    let options = SendMessageOptions {
        markup_spans: vec![
            MarkupSpanData {
                span_type: "bold".to_string(),
                start: 0,
                end: 4,
                uri: None,
            },
            MarkupSpanData {
                span_type: "italic".to_string(),
                start: 5,
                end: 9,
                uri: None,
            },
            MarkupSpanData {
                span_type: "strikethrough".to_string(),
                start: 10,
                end: 14,
                uri: None,
            },
            MarkupSpanData {
                span_type: "code".to_string(),
                start: 15,
                end: 19,
                uri: None,
            },
            MarkupSpanData {
                span_type: "code_block".to_string(),
                start: 20,
                end: 39,
                uri: None,
            },
            MarkupSpanData {
                span_type: "blockquote".to_string(),
                start: 40,
                end: 59,
                uri: None,
            },
            MarkupSpanData {
                span_type: "link".to_string(),
                start: 60,
                end: 64,
                uri: Some("https://example.com".to_string()),
            },
        ],
        ..Default::default()
    };

    let (_, stanza) =
        build_outbound_message("alice@example.com", "chat", "hello world", &options).unwrap();

    let MessagingEvent::Message(parsed_message) = parse(&stanza).expect("parses to event") else {
        panic!("expected message event");
    };
    let parsed = parsed_message.markup_spans;

    assert_eq!(parsed.len(), 7);
    assert!(matches!(parsed[0].span_type, MarkupSpanType::Bold));
    assert_eq!((parsed[0].start, parsed[0].end), (0, 4));
    assert!(matches!(parsed[1].span_type, MarkupSpanType::Italic));
    assert_eq!((parsed[1].start, parsed[1].end), (5, 9));
    assert!(matches!(parsed[2].span_type, MarkupSpanType::Strikethrough));
    assert_eq!((parsed[2].start, parsed[2].end), (10, 14));
    assert!(matches!(parsed[3].span_type, MarkupSpanType::Code));
    assert_eq!((parsed[3].start, parsed[3].end), (15, 19));
    assert!(matches!(parsed[4].span_type, MarkupSpanType::CodeBlock));
    assert_eq!((parsed[4].start, parsed[4].end), (20, 39));
    assert!(matches!(parsed[5].span_type, MarkupSpanType::Blockquote));
    assert_eq!((parsed[5].start, parsed[5].end), (40, 59));
    assert!(matches!(parsed[6].span_type, MarkupSpanType::Link));
    assert_eq!(parsed[6].uri.as_deref(), Some("https://example.com"));
    assert_eq!((parsed[6].start, parsed[6].end), (60, 64));
}

#[test]
fn build_chat_state_message_validates_state() {
    let stanza = build_chat_state_message("alice@example.com", "composing", "chat", None)
        .expect("valid chat state");
    assert_eq!(stanza.attr("to"), Some("alice@example.com"));
    assert_eq!(stanza.attr("type"), Some("chat"));
    assert!(!stanza.attr("id").expect("delivery id").is_empty());
    assert!(stanza.get_child("origin-id", NS_ORIGIN_ID).is_none());
    assert!(stanza.get_child("composing", NS_CHAT_STATES).is_some());
    assert_eq!(stanza.children().count(), 1);
    assert!(build_chat_state_message("alice@example.com", "typing", "chat", None).is_err());
}

#[test]
fn build_chat_state_message_includes_thread_when_present() {
    // XEP-0201 §3 — chat-state notifications related to a threaded
    // conversation SHOULD echo the same `<thread/>`. Without it, peers
    // see typing as channel-wide instead of "in thread X".
    let thread = crate::xep::thread::ThreadRef {
        id: "thread-42".to_string(),
        parent: None,
    };
    let stanza =
        build_chat_state_message("room@muc.example", "composing", "groupchat", Some(&thread))
            .expect("valid chat state");
    let thread_el = stanza
        .get_child("thread", "jabber:client")
        .expect("thread child present");
    assert_eq!(thread_el.text(), "thread-42");
    assert!(thread_el.attr("parent").is_none());
}

#[test]
fn build_chat_state_message_carries_parent_for_nested_threads() {
    let thread = crate::xep::thread::ThreadRef {
        id: "child".to_string(),
        parent: Some("root".to_string()),
    };
    let stanza =
        build_chat_state_message("room@muc.example", "composing", "groupchat", Some(&thread))
            .expect("valid chat state");
    let thread_el = stanza
        .get_child("thread", "jabber:client")
        .expect("thread child present");
    assert_eq!(thread_el.text(), "child");
    assert_eq!(thread_el.attr("parent"), Some("root"));
}

#[test]
fn build_displayed_message_has_expected_shape() {
    let stanza = build_displayed_message("room@muc.example", "msg-1", "groupchat", None);
    assert_eq!(stanza.attr("to"), Some("room@muc.example"));
    assert_origin_id_matches_message_id(&stanza);
    assert!(stanza.get_child("displayed", NS_CHAT_MARKERS).is_some());
    assert_eq!(
        stanza
            .get_child("displayed", NS_CHAT_MARKERS)
            .and_then(|child| child.attr("id")),
        Some("msg-1")
    );
    assert!(stanza.get_child("no-store", NS_HINTS).is_some());
    assert!(stanza.get_child("no-permanent-store", NS_HINTS).is_some());
    assert!(stanza.get_child("thread", "jabber:client").is_none());
}

#[test]
fn build_displayed_message_includes_thread_when_present() {
    // XEP-0201 §3 — read markers for a threaded message SHOULD carry the
    // same `<thread/>` so other clients can scope the marker to the right
    // thread instead of treating it as a channel-wide displayed.
    let thread = crate::xep::thread::ThreadRef {
        id: "thread-99".to_string(),
        parent: None,
    };
    let stanza = build_displayed_message("room@muc.example", "msg-1", "groupchat", Some(&thread));
    let thread_el = stanza
        .get_child("thread", "jabber:client")
        .expect("thread child present");
    assert_eq!(thread_el.text(), "thread-99");
}

#[test]
fn build_correction_message_with_markup_spans() {
    let options = SendMessageOptions {
        markup_spans: vec![MarkupSpanData {
            span_type: "bold".to_string(),
            start: 0,
            end: 5,
            uri: None,
        }],
        references: vec![ReferenceData {
            ref_type: "mention".to_string(),
            uri: "xmpp:bob@example.com".to_string(),
            begin: 6,
            end: 10,
            anchor: None,
        }],
        ..Default::default()
    };

    let (_, stanza) = build_correction_message(
        "room@muc.example",
        "groupchat",
        "hello @bob",
        "orig-1",
        &options,
    )
    .expect("correction stanza");

    assert!(stanza.get_child("replace", NS_MESSAGE_CORRECT).is_some());
    assert!(stanza.get_child("markup", NS_MARKUP).is_some());
    assert!(stanza.get_child("reference", NS_REFERENCES).is_some());
}

#[test]
fn build_reaction_message_has_expected_shape() {
    let emojis = vec!["👍".to_string(), "❤️".to_string()];
    let stanza = build_reaction_message("room@muc.example", "groupchat", "msg-1", &emojis, None);
    assert_origin_id_matches_message_id(&stanza);
    let reactions = stanza
        .get_child("reactions", NS_REACTIONS)
        .expect("reactions child");
    assert_eq!(reactions.attr("id"), Some("msg-1"));
    let values: Vec<String> = reactions
        .children()
        .filter(|child| child.name() == "reaction" && child.ns() == NS_REACTIONS)
        .map(|child| child.text())
        .collect();
    assert_eq!(values, vec!["👍", "❤️"]);
    assert!(stanza.get_child("store", NS_HINTS).is_some());
    assert!(stanza.get_child("thread", "jabber:client").is_none());
}

#[test]
fn build_reaction_message_includes_thread_when_present() {
    // XEP-0201 §3 + XEP-0444 — reactions targeting a threaded message
    // SHOULD repeat the message's `<thread/>` so the reaction routes to
    // the same conversation context on receiving clients.
    let emojis = vec!["🎉".to_string()];
    let thread = crate::xep::thread::ThreadRef {
        id: "thread-react".to_string(),
        parent: None,
    };
    let stanza = build_reaction_message(
        "room@muc.example",
        "groupchat",
        "msg-1",
        &emojis,
        Some(&thread),
    );
    let thread_el = stanza
        .get_child("thread", "jabber:client")
        .expect("thread child present");
    assert_eq!(thread_el.text(), "thread-react");
}

#[test]
fn build_in_call_reaction_message_has_transient_carrier_shape() {
    let recipient =
        InCallReactionRecipient::Direct("bob@example.com/phone".parse().expect("full jid"));
    let sid = InCallSessionId::new("call-1").expect("valid sid");
    let emoji = InCallReactionEmoji::new("👍").expect("valid emoji");
    let stanza = build_in_call_reaction_message(&recipient, &sid, &emoji);
    assert_eq!(stanza.attr("to"), Some("bob@example.com/phone"));
    assert_eq!(stanza.attr("type"), Some("chat"));
    assert!(stanza.get_child("body", NS_CLIENT).is_none());
    assert!(stanza.get_child("origin-id", NS_ORIGIN_ID).is_none());

    let carrier = stanza
        .get_child("in-call", NS_WADDLE_IN_CALL)
        .expect("in-call child");
    assert_eq!(carrier.attr("sid"), Some("call-1"));
    let reaction = carrier
        .get_child("reaction", NS_WADDLE_IN_CALL)
        .expect("reaction child");
    assert_eq!(reaction.attr("emoji"), Some("👍"));

    assert!(stanza.get_child("no-store", NS_HINTS).is_some());
    assert!(stanza.get_child("no-copy", NS_HINTS).is_some());
    assert!(stanza.get_child("store", NS_HINTS).is_none());
}

#[test]
fn in_call_reaction_values_reject_empty_wire_fields() {
    assert!(InCallSessionId::new("").is_err());
    assert!(InCallSessionId::new("   ").is_err());
    assert!(InCallReactionEmoji::new("").is_err());
    assert!(InCallReactionEmoji::new("   ").is_err());
}

#[test]
fn in_call_reaction_values_trim_before_serialization() {
    let sid = InCallSessionId::new(" call-1 ").expect("valid sid");
    let emoji = InCallReactionEmoji::new(" 👍 ").expect("valid emoji");
    assert_eq!(sid.as_str(), "call-1");
    assert_eq!(emoji.as_str(), "👍");
}

#[test]
fn build_retraction_message_has_expected_shape() {
    let stanza = build_retraction_message("room@muc.example", "groupchat", "msg-1", None);
    assert_origin_id_matches_message_id(&stanza);
    assert_eq!(
        stanza
            .get_child("retract", NS_MESSAGE_RETRACT)
            .and_then(|child| child.attr("id")),
        Some("msg-1")
    );
    assert_eq!(
        stanza
            .get_child("body", NS_CLIENT)
            .map(|child| child.text()),
        Some("This person attempted to retract a previous message.".to_string())
    );
    assert!(stanza.get_child("store", NS_HINTS).is_some());
    assert!(stanza.get_child("thread", "jabber:client").is_none());
}

/// XEP-0424 §Use Case (#1267 item 1): the retraction's fallback body
/// SHOULD be marked with a XEP-0428 `<fallback/>` whose `for` attribute
/// names the retraction namespace, so supporting clients never render
/// the generic body as real content.
#[test]
fn build_retraction_message_marks_body_as_xep0428_fallback() {
    let stanza = build_retraction_message("room@muc.example", "groupchat", "msg-1", None);
    let fallback = stanza
        .get_child("fallback", crate::xep::fallback::NS_FALLBACK)
        .expect("XEP-0428 fallback marker present");
    assert_eq!(fallback.attr("for"), Some(NS_MESSAGE_RETRACT));
}

/// XEP-0045 §7.5: a MUC private message send carries the empty
/// `<x xmlns='http://jabber.org/protocol/muc#user'/>` marker so the
/// sender's other clients can classify the sent-carbon copy; normal DMs
/// and groupchat messages never carry it.
#[test]
fn build_outbound_message_marks_muc_pm_with_muc_user_x() {
    let options = SendMessageOptions {
        muc_pm: true,
        ..SendMessageOptions::default()
    };
    let (_, stanza) = build_outbound_message("room@muc.example/juliet", "chat", "psst", &options)
        .expect("builds");
    assert!(stanza.get_child("x", NS_MUC_USER).is_some());

    let (_, plain_dm) = build_outbound_message(
        "bob@example.com",
        "chat",
        "hi",
        &SendMessageOptions::default(),
    )
    .expect("builds");
    assert!(plain_dm.get_child("x", NS_MUC_USER).is_none());

    let (_, groupchat) = build_outbound_message(
        "room@muc.example",
        "groupchat",
        "hi all",
        &SendMessageOptions {
            muc_pm: true,
            ..SendMessageOptions::default()
        },
    )
    .expect("builds");
    assert!(
        groupchat.get_child("x", NS_MUC_USER).is_none(),
        "muc#user marker is a chat-typed PM shape only"
    );
}

/// XEP-0045 §7.2.15 (#1255): the join presence always requests zero
/// discussion history — MAM catch-up is the authoritative history
/// source, so accepting default join history would double-deliver.
#[test]
fn build_muc_join_presence_requests_no_history() {
    let room: jid::BareJid = "room@muc.example".parse().expect("valid room JID");
    let presence = build_muc_join_presence(&room, "alice").expect("valid nick");
    assert_eq!(presence.name(), "presence");
    assert_eq!(presence.attr("to"), Some("room@muc.example/alice"));
    let x = presence
        .get_child("x", "http://jabber.org/protocol/muc")
        .expect("muc <x/> child present");
    let history = x
        .get_child("history", "http://jabber.org/protocol/muc")
        .expect("<history/> child present");
    assert_eq!(history.attr("maxstanzas"), Some("0"));
}

#[test]
fn build_retraction_message_includes_thread_when_present() {
    // XEP-0201 §3 + XEP-0424 — retracting a threaded message SHOULD
    // repeat the message's `<thread/>` so the tombstone routes to the
    // same thread on receiving clients.
    let thread = crate::xep::thread::ThreadRef {
        id: "thread-retract".to_string(),
        parent: None,
    };
    let stanza = build_retraction_message("room@muc.example", "groupchat", "msg-1", Some(&thread));
    let thread_el = stanza
        .get_child("thread", "jabber:client")
        .expect("thread child present");
    assert_eq!(thread_el.text(), "thread-retract");
}

#[test]
fn build_pin_messages_stamp_origin_id_matching_message_id() {
    let pinned = build_pinned_message("room@muc.example", "room-stanza-1");
    assert_origin_id_matches_message_id(&pinned);
    assert!(pinned.get_child("pinned", NS_WADDLE_PIN_V0).is_some());

    let unpinned = build_unpinned_message("room@muc.example", "room-stanza-1");
    assert_origin_id_matches_message_id(&unpinned);
    assert!(unpinned.get_child("unpinned", NS_WADDLE_PIN_V0).is_some());
}

#[test]
fn build_moderation_message_has_expected_shape() {
    let stanza =
        build_moderation_message("room@muc.example", "groupchat", "msg-1", Some("cleanup"));
    assert_eq!(stanza.attr("to"), Some("room@muc.example"));
    assert_eq!(stanza.name(), "iq");
    assert_eq!(stanza.attr("type"), Some("set"));
    let moderate = stanza
        .get_child("moderate", NS_MESSAGE_MODERATE)
        .expect("moderate child");
    assert_eq!(moderate.attr("id"), Some("msg-1"));
    assert!(moderate.get_child("retract", NS_MESSAGE_RETRACT).is_some());
    assert_eq!(
        moderate
            .get_child("reason", NS_MESSAGE_MODERATE)
            .map(|child| child.text()),
        Some("cleanup".to_string())
    );
}

#[test]
fn build_correction_message_has_expected_shape() {
    let (message_id, stanza) = build_correction_message(
        "alice@example.com",
        "chat",
        "fixed",
        "msg-1",
        &SendMessageOptions::default(),
    )
    .expect("correction stanza");
    assert_eq!(stanza.attr("to"), Some("alice@example.com"));
    assert_eq!(stanza.attr("id"), Some(message_id.as_str()));
    assert_eq!(
        stanza
            .get_child("body", NS_CLIENT)
            .map(minidom::Element::text),
        Some("fixed".to_string())
    );
    assert_eq!(
        stanza
            .get_child("replace", NS_MESSAGE_CORRECT)
            .and_then(|child: &minidom::Element| child.attr("id")),
        Some("msg-1")
    );
}

#[test]
fn parse_ignores_iq() {
    let e = el("<iq xmlns='jabber:client' type='get' id='iq-1'/>");
    assert!(parse(&e).is_none());
}
