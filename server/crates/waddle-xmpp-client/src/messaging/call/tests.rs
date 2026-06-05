use super::*;

#[test]
fn parses_propose_with_audio_video() {
    let xml = r#"<message xmlns='jabber:client' from='alice@waddle.test/desktop' to='bob@waddle.test'>
            <propose xmlns='urn:xmpp:jingle-message:0' id='c1'>
              <description xmlns='urn:xmpp:jingle:apps:rtp:1' media='audio'/>
              <description xmlns='urn:xmpp:jingle:apps:rtp:1' media='video'/>
            </propose>
        </message>"#;
    let elem: Element = xml.parse().unwrap();
    let ev = parse_call_event(&elem).expect("propose parses");
    // XEP-0353 §0.6: the propose's `from` is stamped by the
    // server as the initiator's *full* JID so the responder can
    // address its proceed/reject directly at that resource.
    assert_eq!(ev.from.to_string(), "alice@waddle.test/desktop");
    assert_eq!(ev.sid.0, "c1");
    match ev.kind {
        CallEventKind::Propose { media } => assert_eq!(media, CallMedia::audio_video()),
        other => panic!("expected Propose, got {other:?}"),
    }
}

#[test]
fn parses_proceed() {
    let xml = "<message xmlns='jabber:client' from='bob@waddle.test/desktop'>
            <proceed xmlns='urn:xmpp:jingle-message:0' id='c1'/>
        </message>";
    let elem: Element = xml.parse().unwrap();
    let ev = parse_call_event(&elem).expect("proceed parses");
    assert!(matches!(ev.kind, CallEventKind::Proceed));
}

#[test]
fn parses_finish() {
    let xml = "<message xmlns='jabber:client' from='alice@waddle.test/desktop'>
            <finish xmlns='urn:xmpp:jingle-message:0' id='c1'/>
        </message>";
    let elem: Element = xml.parse().unwrap();
    let ev = parse_call_event(&elem).expect("finish parses");
    assert!(matches!(
        ev.kind,
        CallEventKind::Finish {
            reason: None,
            migrated_to: None
        }
    ));
}

#[test]
fn parses_tie_break_reject_and_retract_metadata() {
    let xml = r#"<message xmlns='jabber:client' from='alice@waddle.test/desktop'>
            <reject xmlns='urn:xmpp:jingle-message:0' id='c1'>
              <reason xmlns='urn:xmpp:jingle:1'><expired/></reason>
              <tie-break xmlns='urn:xmpp:jingle-message:0'/>
            </reject>
        </message>"#;
    let elem: Element = xml.parse().unwrap();
    let ev = parse_call_event(&elem).expect("reject parses");
    match ev.kind {
        CallEventKind::Reject { reason, tie_break } => {
            assert_eq!(reason, Some(JingleReason::Expired));
            assert!(tie_break);
        }
        other => panic!("expected Reject, got {other:?}"),
    }

    let xml = r#"<message xmlns='jabber:client' from='alice@waddle.test/desktop'>
            <retract xmlns='urn:xmpp:jingle-message:0' id='c1'>
              <reason xmlns='urn:xmpp:jingle:1'><expired/></reason>
              <tie-break xmlns='urn:xmpp:jingle-message:0'/>
            </retract>
        </message>"#;
    let elem: Element = xml.parse().unwrap();
    let ev = parse_call_event(&elem).expect("retract parses");
    match ev.kind {
        CallEventKind::Retract { reason, tie_break } => {
            assert_eq!(reason, Some(JingleReason::Expired));
            assert!(tie_break);
        }
        other => panic!("expected Retract, got {other:?}"),
    }
}

#[test]
fn parses_session_initiate_with_livekit_transport() {
    let xml = r#"<iq xmlns='jabber:client' type='set' from='alice@waddle.test/desktop' to='bob@waddle.test/desktop' id='i1'>
            <jingle xmlns='urn:xmpp:jingle:1' action='session-initiate' sid='c1' initiator='alice@waddle.test/desktop'>
              <content creator='initiator' name='audio'>
                <description xmlns='urn:xmpp:jingle:apps:rtp:1' media='audio'/>
                <transport xmlns='urn:waddle:transports:livekit:0'
                           url='wss://livekit.waddle.test'
                           room='alice@waddle.test::c1'
                           identity='bob@waddle.test/desktop'>
                  <token xmlns='urn:waddle:transports:livekit:0'>eyJhbGc.payload.sig</token>
                </transport>
              </content>
            </jingle>
        </iq>"#;
    let elem: Element = xml.parse().unwrap();
    let ev = parse_call_event(&elem).expect("session-initiate parses");
    assert_eq!(ev.sid.0, "c1");
    match ev.kind {
        CallEventKind::SessionInitiate { join, media } => {
            assert_eq!(join.url, "wss://livekit.waddle.test");
            assert_eq!(join.room, "alice@waddle.test::c1");
            assert_eq!(join.identity, "bob@waddle.test/desktop");
            assert_eq!(join.token, "eyJhbGc.payload.sig");
            assert_eq!(media, CallMedia::audio_only());
        }
        other => panic!("expected SessionInitiate, got {other:?}"),
    }
}

#[test]
fn parses_session_terminate_with_reason() {
    let xml = r#"<iq xmlns='jabber:client' type='set' from='bob@waddle.test/desktop' id='t1'>
            <jingle xmlns='urn:xmpp:jingle:1' action='session-terminate' sid='c1'>
              <reason><success/></reason>
            </jingle>
        </iq>"#;
    let elem: Element = xml.parse().unwrap();
    let ev = parse_call_event(&elem).expect("session-terminate parses");
    match ev.kind {
        CallEventKind::SessionTerminate { reason } => {
            // The wire `<success/>` parses into the typed
            // variant - not a raw string.
            assert_eq!(reason, Some(JingleReason::Success));
        }
        other => panic!("expected SessionTerminate, got {other:?}"),
    }
}

#[test]
fn jingle_reason_wire_names_match_xep_0166_spec() {
    // XEP-0166 §7.4 normative wire condition names. Every
    // typed variant must serialise to the spec-defined string;
    // every spec-defined string must round-trip back to the
    // same variant via `JingleReason::from_str`. A typo in any
    // single arm of `jingle_reason_wire_name` (the table used
    // by the wasm chat client to emit the reason to JS) would
    // be invisible at runtime until a peer rejected the
    // stanza - this table-driven test makes the failure
    // catchable at PR time instead.
    let cases: &[(JingleReason, &str)] = &[
        (
            JingleReason::AlternativeSession { sid: None },
            "alternative-session",
        ),
        (JingleReason::Busy, "busy"),
        (JingleReason::Cancel, "cancel"),
        (JingleReason::ConnectivityError, "connectivity-error"),
        (JingleReason::Decline, "decline"),
        (JingleReason::Expired, "expired"),
        (JingleReason::FailedApplication, "failed-application"),
        (JingleReason::FailedTransport, "failed-transport"),
        (JingleReason::GeneralError, "general-error"),
        (JingleReason::Gone, "gone"),
        (
            JingleReason::IncompatibleParameters,
            "incompatible-parameters",
        ),
        (JingleReason::MediaError, "media-error"),
        (JingleReason::SecurityError, "security-error"),
        (JingleReason::Success, "success"),
        (JingleReason::Timeout, "timeout"),
        (
            JingleReason::UnsupportedApplications,
            "unsupported-applications",
        ),
        (
            JingleReason::UnsupportedTransports,
            "unsupported-transports",
        ),
    ];
    for (variant, expected) in cases {
        assert_eq!(
            jingle_reason_wire_name(variant.clone()),
            *expected,
            "wire name for {variant:?} must match XEP-0166 §7.4"
        );
        let round_tripped = jingle_reason_from_wire_name(expected)
            .expect("XEP wire name parses back to JingleReason");
        assert_eq!(
            &round_tripped, variant,
            "{expected} must round-trip to {variant:?} via FromStr"
        );
    }
}

#[test]
fn session_terminate_unknown_condition_drops_to_none() {
    // Non-conforming servers MUST NOT leak unknown reason
    // names through the typed boundary. Parser surfaces None.
    let xml = r#"<iq xmlns='jabber:client' type='set' from='bob@waddle.test/d' id='t1'>
            <jingle xmlns='urn:xmpp:jingle:1' action='session-terminate' sid='c1'>
              <reason><not-a-real-condition/></reason>
            </jingle>
        </iq>"#;
    let elem: Element = xml.parse().unwrap();
    let ev = parse_call_event(&elem).expect("session-terminate parses");
    match ev.kind {
        CallEventKind::SessionTerminate { reason } => assert_eq!(reason, None),
        other => panic!("expected SessionTerminate, got {other:?}"),
    }
}

fn sid(s: &str) -> SessionId {
    SessionId(s.to_string())
}

fn full(s: &str) -> FullJid {
    s.parse().unwrap()
}

#[test]
fn returns_none_for_non_call_message() {
    let xml = "<message xmlns='jabber:client' from='alice@waddle.test'><body>hi</body></message>";
    let elem: Element = xml.parse().unwrap();
    assert!(parse_call_event(&elem).is_none());
}

#[test]
fn returns_none_for_jingle_with_unknown_action() {
    let xml = r#"<iq xmlns='jabber:client' type='set' from='alice@waddle.test/d'>
            <jingle xmlns='urn:xmpp:jingle:1' action='transport-info' sid='c1'/>
        </iq>"#;
    let elem: Element = xml.parse().unwrap();
    // transport-info isn't surfaced as a call event yet - it's
    // mid-session signalling, handled internally.
    assert!(parse_call_event(&elem).is_none());
}

// --- outbound builders --------------------------------------------

#[test]
fn build_propose_emits_one_description_per_offered_media() {
    let elem = build_propose(&sid("c1"), CallMedia::audio_video());
    assert_eq!(elem.name(), "propose");
    assert_eq!(elem.ns(), NS_JINGLE_MESSAGE);
    assert_eq!(elem.attr("id"), Some("c1"));
    let media: Vec<_> = elem
        .children()
        .filter(|c| c.name() == "description" && c.ns() == NS_JINGLE_RTP)
        .filter_map(|c| c.attr("media"))
        .collect();
    assert_eq!(media, vec!["audio", "video"]);
}

#[test]
fn build_propose_audio_only_omits_video_description() {
    let elem = build_propose(&sid("c1"), CallMedia::audio_only());
    let media: Vec<_> = elem
        .children()
        .filter(|c| c.name() == "description")
        .filter_map(|c| c.attr("media"))
        .collect();
    assert_eq!(media, vec!["audio"]);
}

#[test]
fn build_propose_descriptions_include_rtcp_mux() {
    let elem = build_propose(&sid("c1"), CallMedia::audio_video());
    for desc in elem
        .children()
        .filter(|c| c.name() == "description" && c.ns() == NS_JINGLE_RTP)
    {
        assert!(
            desc.children()
                .any(|c| c.name() == "rtcp-mux" && c.ns() == NS_JINGLE_RTP),
            "XEP-0167 §3.3: <description/> must advertise <rtcp-mux/>"
        );
    }
}

#[test]
fn build_session_initiate_descriptions_include_rtcp_mux() {
    let initiator: FullJid = "alice@waddle.test/desktop".parse().unwrap();
    let elem = build_session_initiate(&sid("c1"), &initiator, CallMedia::audio_video());
    for content in elem.children().filter(|c| c.name() == "content") {
        let desc = content
            .children()
            .find(|c| c.name() == "description" && c.ns() == NS_JINGLE_RTP)
            .expect("each content carries an RTP description");
        assert!(
            desc.children()
                .any(|c| c.name() == "rtcp-mux" && c.ns() == NS_JINGLE_RTP),
            "XEP-0167 §3.3 conformance on session-initiate"
        );
    }
}

#[test]
fn build_muji_session_initiate_descriptions_include_rtcp_mux() {
    let elem = build_muji_session_initiate(
        &sid("muji-1"),
        &full("alice@waddle.test/desktop"),
        "room@muc.waddle.test",
        true,
    );
    for content in elem.children().filter(|c| c.name() == "content") {
        let desc = content
            .children()
            .find(|c| c.name() == "description" && c.ns() == NS_JINGLE_RTP)
            .expect("each Muji content carries an RTP description");
        assert!(
            desc.children()
                .any(|c| c.name() == "rtcp-mux" && c.ns() == NS_JINGLE_RTP),
            "XEP-0167 §3.3 conformance on Muji session-initiate"
        );
    }
}

#[test]
fn build_session_accept_descriptions_include_rtcp_mux() {
    let responder: FullJid = "bob@waddle.test/desktop".parse().unwrap();
    let elem = build_session_accept(&sid("c1"), &responder, CallMedia::audio_video());
    for content in elem.children().filter(|c| c.name() == "content") {
        let desc = content
            .children()
            .find(|c| c.name() == "description" && c.ns() == NS_JINGLE_RTP)
            .expect("each content carries an RTP description");
        assert!(
            desc.children()
                .any(|c| c.name() == "rtcp-mux" && c.ns() == NS_JINGLE_RTP),
            "XEP-0167 §3.3 conformance on session-accept"
        );
    }
}

#[test]
fn build_jmi_helpers_roundtrip_through_parser() {
    // proceed: wrapping in a <message/> with a `from` makes the
    // inbound parser pick it up.
    let stanza = Element::builder("message", "jabber:client")
        .attr(
            minidom::rxml::xml_ncname!("from").to_owned(),
            "bob@waddle.test/desktop",
        )
        .append(build_proceed(&sid("c1")))
        .build();
    let ev = parse_call_event(&stanza).expect("proceed parses");
    assert!(matches!(ev.kind, CallEventKind::Proceed));

    let stanza = Element::builder("message", "jabber:client")
        .attr(
            minidom::rxml::xml_ncname!("from").to_owned(),
            "bob@waddle.test/desktop",
        )
        .append(build_reject(&sid("c1")))
        .build();
    let ev = parse_call_event(&stanza).expect("reject parses");
    assert!(matches!(
        ev.kind,
        CallEventKind::Reject {
            reason: None,
            tie_break: false
        }
    ));

    let stanza = Element::builder("message", "jabber:client")
        .attr(
            minidom::rxml::xml_ncname!("from").to_owned(),
            "alice@waddle.test/desktop",
        )
        .append(build_retract(&sid("c1")))
        .build();
    let ev = parse_call_event(&stanza).expect("retract parses");
    assert!(matches!(
        ev.kind,
        CallEventKind::Retract {
            reason: None,
            tie_break: false
        }
    ));

    let stanza = Element::builder("message", "jabber:client")
        .attr(
            minidom::rxml::xml_ncname!("from").to_owned(),
            "alice@waddle.test/desktop",
        )
        .append(build_finish(&sid("c1")))
        .build();
    let ev = parse_call_event(&stanza).expect("finish parses");
    assert!(matches!(
        ev.kind,
        CallEventKind::Finish {
            reason: None,
            migrated_to: None
        }
    ));
}

#[test]
fn build_tie_break_jmi_helpers_emit_expired_reason_and_tie_break() {
    let reject = build_reject_with_options(&sid("c1"), Some(JingleReason::Expired), true);
    assert!(
        reject.get_child("tie-break", NS_JINGLE_MESSAGE).is_some(),
        "XEP-0353 tie-break reject carries <tie-break/>"
    );
    assert!(
        reject
            .get_child("reason", NS_JINGLE)
            .and_then(|reason| reason.get_child("expired", NS_JINGLE))
            .is_some(),
        "XEP-0353 tie-break reject carries <reason><expired/></reason>"
    );

    let retract = build_retract_with_options(&sid("c1"), Some(JingleReason::Expired), true);
    assert!(retract.get_child("tie-break", NS_JINGLE_MESSAGE).is_some());
    assert!(retract
        .get_child("reason", NS_JINGLE)
        .and_then(|reason| reason.get_child("expired", NS_JINGLE))
        .is_some());
}

#[test]
fn build_finish_migrated_emits_expired_reason_and_migrated_target() {
    let finish = build_finish_migrated(&sid("old"), JingleReason::Expired, &sid("new"));
    assert!(finish
        .get_child("reason", NS_JINGLE)
        .and_then(|reason| reason.get_child("expired", NS_JINGLE))
        .is_some());
    let migrated = finish
        .get_child("migrated", NS_JINGLE_MESSAGE)
        .expect("finish carries migrated child");
    assert_eq!(migrated.attr("to"), Some("new"));

    let stanza = Element::builder("message", "jabber:client")
        .attr(
            minidom::rxml::xml_ncname!("from").to_owned(),
            "alice@waddle.test/desktop",
        )
        .append(finish)
        .build();
    let ev = parse_call_event(&stanza).expect("finish parses");
    match ev.kind {
        CallEventKind::Finish {
            reason,
            migrated_to,
        } => {
            assert_eq!(reason, Some(JingleReason::Expired));
            assert_eq!(migrated_to.map(|sid| sid.0).as_deref(), Some("new"));
        }
        other => panic!("expected Finish, got {other:?}"),
    }
}

#[test]
fn build_session_initiate_carries_empty_waddle_transport_per_content() {
    let jingle = build_session_initiate(
        &sid("c1"),
        &full("alice@waddle.test/desktop"),
        CallMedia::audio_video(),
    );
    assert_eq!(jingle.name(), "jingle");
    assert_eq!(jingle.ns(), NS_JINGLE);
    assert_eq!(jingle.attr("action"), Some("session-initiate"));
    assert_eq!(jingle.attr("sid"), Some("c1"));
    assert_eq!(jingle.attr("initiator"), Some("alice@waddle.test/desktop"));

    let contents: Vec<_> = jingle
        .children()
        .filter(|c| c.name() == "content")
        .collect();
    assert_eq!(contents.len(), 2);
    for content in contents {
        // Every content has an empty Waddle transport request -
        // the server fills in url/room/identity/token before
        // forwarding to the peer.
        let transport = content
            .children()
            .find(|c| c.name() == "transport")
            .expect("content has transport");
        assert_eq!(transport.ns(), NS_WADDLE_LIVEKIT_TRANSPORT);
        assert!(
            transport.attr("url").is_none(),
            "outbound transport must be a request"
        );
        assert!(transport.attr("room").is_none());
        assert!(transport.attr("identity").is_none());
        assert!(transport.children().next().is_none());
    }
}

#[test]
fn build_muji_session_initiate_carries_muji_room_and_requested_media() {
    let jingle = build_muji_session_initiate(
        &sid("muji-1"),
        &full("alice@waddle.test/desktop"),
        "room@muc.waddle.test",
        false,
    );
    assert_eq!(jingle.name(), "jingle");
    assert_eq!(jingle.ns(), NS_JINGLE);
    assert_eq!(jingle.attr("action"), Some("session-initiate"));
    assert_eq!(jingle.attr("sid"), Some("muji-1"));
    assert_eq!(jingle.attr("initiator"), Some("alice@waddle.test/desktop"));

    let muji = jingle
        .get_child("muji", NS_MUJI)
        .expect("Muji metadata is present");
    assert_eq!(muji.attr("room"), Some("room@muc.waddle.test"));

    let contents: Vec<_> = jingle
        .children()
        .filter(|c| c.name() == "content" && c.ns() == NS_JINGLE)
        .collect();
    assert_eq!(contents.len(), 1);
    assert_eq!(contents[0].attr("name"), Some("audio"));
    assert_eq!(contents[0].attr("senders"), Some("both"));

    let desc = contents[0]
        .get_child("description", NS_JINGLE_RTP)
        .expect("RTP description");
    assert_eq!(desc.attr("media"), Some("audio"));
    assert!(desc
        .children()
        .any(|c| c.name() == "payload-type" && c.attr("name") == Some("opus")));
    assert_eq!(
        contents[0]
            .get_child("transport", NS_WADDLE_LIVEKIT_TRANSPORT)
            .map(|t| t.name()),
        Some("transport")
    );
}

#[test]
fn build_muji_session_initiate_adds_video_when_requested() {
    let jingle = build_muji_session_initiate(
        &sid("muji-1"),
        &full("alice@waddle.test/desktop"),
        "room@muc.waddle.test",
        true,
    );
    let media: Vec<_> = jingle
        .children()
        .filter(|c| c.name() == "content" && c.ns() == NS_JINGLE)
        .filter_map(|content| {
            content
                .get_child("description", NS_JINGLE_RTP)
                .and_then(|desc| desc.attr("media"))
        })
        .collect();
    assert_eq!(media, vec!["audio", "video"]);
}

#[test]
fn build_session_accept_carries_responder_attr_and_empty_transport() {
    let jingle = build_session_accept(
        &sid("c1"),
        &full("bob@waddle.test/desktop"),
        CallMedia::audio_only(),
    );
    assert_eq!(jingle.attr("action"), Some("session-accept"));
    assert_eq!(jingle.attr("initiator"), None);
    assert_eq!(jingle.attr("responder"), Some("bob@waddle.test/desktop"));
    let contents: Vec<_> = jingle
        .children()
        .filter(|c| c.name() == "content")
        .collect();
    assert_eq!(contents.len(), 1);
}

#[test]
fn build_session_terminate_includes_reason_when_supplied() {
    let with_reason =
        build_session_terminate(&sid("c1"), Some(xmpp_parsers::jingle::Reason::Success));
    assert_eq!(with_reason.attr("initiator"), None);
    let reason_elem = with_reason
        .children()
        .find(|c| c.name() == "reason")
        .expect("reason child");
    assert!(reason_elem.children().any(|c| c.name() == "success"));

    let without = build_session_terminate(&sid("c1"), None);
    assert!(without.children().all(|c| c.name() != "reason"));
}

#[test]
fn wrap_jmi_message_stamps_type_chat_and_store_hint() {
    // XEP-0353 §3: every JMI message (propose / proceed / reject /
    // retract / finish) MUST be `type='chat'` and MUST contain a
    // XEP-0334 `<store/>` hint. Without this envelope, JMI stanzas
    // ship as `type='normal'` and skip MAM archival, breaking call
    // history reconstruction.
    let to: Jid = "bob@waddle.test".parse().unwrap();
    let stanza = wrap_jmi_message(&to, build_propose(&sid("c1"), CallMedia::audio_video()));
    assert_eq!(stanza.name(), "message");
    assert_eq!(stanza.attr("type"), Some("chat"));
    assert_eq!(stanza.attr("to"), Some("bob@waddle.test"));
    let store = stanza
        .children()
        .find(|c| c.name() == "store" && c.ns() == NS_HINTS)
        .expect("XEP-0334 <store/> hint required by XEP-0353 §3");
    assert!(store.children().next().is_none());
    // The JMI body itself rides along, so the responder's parser
    // still surfaces it via parse_jmi_message -> CallEventKind.
    let ev = parse_call_event(
        &Element::builder("message", "jabber:client")
            .attr(
                minidom::rxml::xml_ncname!("from").to_owned(),
                "alice@waddle.test/desktop",
            )
            .attr(
                minidom::rxml::xml_ncname!("type").to_owned(),
                stanza.attr("type").unwrap_or_default(),
            )
            .append_all(stanza.children().cloned())
            .build(),
    )
    .expect("wrapped JMI body still parses as a call event");
    assert!(matches!(ev.kind, CallEventKind::Propose { .. }));
}

#[test]
fn wrap_jmi_message_preserves_jmi_body_for_every_variant() {
    // Every JMI variant must survive the envelope.
    let to: Jid = "bob@waddle.test".parse().unwrap();
    let cases: Vec<(&str, Element)> = vec![
        (
            "propose",
            build_propose(&sid("c1"), CallMedia::audio_only()),
        ),
        ("proceed", build_proceed(&sid("c1"))),
        ("reject", build_reject(&sid("c1"))),
        ("retract", build_retract(&sid("c1"))),
        ("finish", build_finish(&sid("c1"))),
    ];
    for (name, body) in cases {
        let stanza = wrap_jmi_message(&to, body);
        assert_eq!(stanza.attr("type"), Some("chat"), "{name}: type=chat");
        assert!(
            stanza
                .children()
                .any(|c| c.name() == name && c.ns() == NS_JINGLE_MESSAGE),
            "{name}: JMI body preserved"
        );
        assert!(
            stanza
                .children()
                .any(|c| c.name() == "store" && c.ns() == NS_HINTS),
            "{name}: store hint attached"
        );
    }
}

#[test]
fn rejects_jingle_session_initiate_without_livekit_transport() {
    let xml = r#"<iq xmlns='jabber:client' type='set' from='alice@waddle.test/d' id='i1'>
            <jingle xmlns='urn:xmpp:jingle:1' action='session-initiate' sid='c1'>
              <content creator='initiator' name='audio'>
                <description xmlns='urn:xmpp:jingle:apps:rtp:1' media='audio'/>
                <transport xmlns='urn:xmpp:jingle:transports:ice-udp:1'/>
              </content>
            </jingle>
        </iq>"#;
    let elem: Element = xml.parse().unwrap();
    // No Waddle transport -> no LiveKit credentials -> not a
    // surfaced call event (the chat UI has nothing actionable).
    assert!(parse_call_event(&elem).is_none());
}
