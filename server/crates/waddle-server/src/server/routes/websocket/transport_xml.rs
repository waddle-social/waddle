use super::*;

/// Convert a Stanza to XML string for WebSocket transmission
/// Serialize a stanza to XML by converting it to a `minidom::Element` via
/// `xmpp_parsers`' own `From<T> for Element` impls.  This ensures every field
/// — bodies, payloads (embeds), thread, etc. — is faithfully serialized
/// without hand-rolled format strings.
pub(crate) fn stanza_to_xml(stanza: &Stanza) -> String {
    let mut element = stanza.to_element();
    if let Stanza::Message(message) = stanza {
        // xmpp_parsers drops RFC 6121 `<thread/>` during Message -> Element.
        // Re-attach via the canonical XEP-0201 builder so the wire shape
        // — including any optional `parent` attribute — round-trips.
        // Note: post-`reattach_thread_parent` messages have
        // `message.thread = None` (the typed field is cleared and the
        // thread element lives in `msg.payloads` instead, which
        // `to_element()` already serializes faithfully). This branch
        // only re-attaches when the typed field is the only source —
        // no parent context exists in that case.
        if let Some(thread) = message.thread.as_ref() {
            // RFC 6121 §5.2.5 scopes `<thread/>` to the enclosing
            // message's namespace (`jabber:client` / `jabber:server`,
            // sometimes serialized with empty ns). Match that — any
            // `<thread>` element from a *different* namespace is an
            // unrelated extension payload that must not suppress
            // reattachment of the actual conversation thread
            // (Copilot review on PR #305).
            let stanza_ns = element.ns();
            let has_thread = element.children().any(|child| {
                child.name() == "thread" && (child.ns() == stanza_ns || child.ns().is_empty())
            });
            if !has_thread {
                // Skip the reattach if the typed `Thread` body is
                // empty: the wire shape requires a non-empty body
                // (`<thread></thread>` is malformed per RFC 6121
                // §5.2.5 / XEP-0201). `ThreadId::new` is the gate.
                if let Some(id) = waddle_xmpp::mam::ThreadId::new(thread.id.clone()) {
                    let info = waddle_xmpp::xep0201::ThreadInfo::root(id);
                    element.append_child(waddle_xmpp::xep0201::build_thread_element(
                        &info, &stanza_ns,
                    ));
                }
            }
        }
    }
    element_to_xml(element)
}

pub(crate) fn element_to_xml(element: xmpp_parsers::minidom::Element) -> String {
    let mut buf = Vec::new();
    // write_to cannot fail on a Vec<u8> in practice.
    element
        .write_to(&mut buf)
        .expect("serializing stanza to Vec<u8> should not fail");
    String::from_utf8(buf).expect("xmpp_parsers serializes valid UTF-8")
}

pub(super) fn websocket_stream_open_element(domain: &str) -> Element {
    Element::builder("open", "urn:ietf:params:xml:ns:xmpp-framing")
        .attr(minidom::rxml::xml_ncname!("from").to_owned(), domain)
        .attr(
            minidom::rxml::xml_ncname!("id").to_owned(),
            uuid::Uuid::new_v4().to_string(),
        )
        .attr(minidom::rxml::xml_ncname!("version").to_owned(), "1.0")
        .attr_ns(
            minidom::rxml::Namespace::XML,
            minidom::rxml::xml_ncname!("lang").to_owned(),
            "en",
        )
        .build()
}

#[cfg(test)]
pub(super) fn websocket_stream_open_xml(domain: &str) -> String {
    element_to_xml(websocket_stream_open_element(domain))
}

pub(super) fn websocket_stream_close_element() -> Element {
    Element::builder("close", "urn:ietf:params:xml:ns:xmpp-framing").build()
}

pub(super) fn websocket_stream_close_xml() -> String {
    element_to_xml(websocket_stream_close_element())
}

pub(crate) fn iq_to_xml(iq: xmpp_parsers::iq::Iq) -> String {
    stanza_to_xml(&Stanza::Iq(Box::new(iq)))
}

pub(crate) fn build_iq_result_xml(
    id: &str,
    from: Option<&str>,
    to: Option<&str>,
    payload: Option<xmpp_parsers::minidom::Element>,
) -> String {
    let mut iq = Element::builder("iq", waddle_xmpp::ns::JABBER_CLIENT)
        .attr(minidom::rxml::xml_ncname!("id").to_owned(), id)
        .attr(minidom::rxml::xml_ncname!("type").to_owned(), "result");
    if let Some(from) = from {
        iq = iq.attr(minidom::rxml::xml_ncname!("from").to_owned(), from);
    }
    if let Some(to) = to {
        iq = iq.attr(minidom::rxml::xml_ncname!("to").to_owned(), to);
    }

    let iq = if let Some(payload) = payload {
        iq.append(payload).build()
    } else {
        iq.build()
    };

    element_to_xml(iq)
}

/// Build an `<iq type='error'>` reply from a typed
/// [`xmpp_parsers::stanza_error::StanzaError`].
///
/// Replaces the previous stringly-typed `build_iq_error_xml*` helpers
/// per the typed-payloads hard rule (CLAUDE.md): `error_type` and
/// `condition` MUST flow as typed enum values across function
/// boundaries, never as `&str`. Serialization uses the upstream
/// `From<StanzaError> for Element` impl so the wire shape is identical
/// to the previous manual builder.
/// Like [`build_iq_error_xml_typed`], but echoing the request payload
/// ahead of the `<error/>` child — XEP-0045 §8.4/§9.7 require role/
/// affiliation denials to return the error "along with the offending
/// item(s)" (RFC 6120 §8.3.1 sender-echo form).
pub(crate) fn build_iq_error_xml_with_payload(
    id: &str,
    from: Option<&str>,
    to: Option<&str>,
    payload: xmpp_parsers::minidom::Element,
    error: xmpp_parsers::stanza_error::StanzaError,
) -> String {
    let mut iq = xmpp_parsers::minidom::Element::builder("iq", "jabber:client")
        .attr(minidom::rxml::xml_ncname!("id").to_owned(), id)
        .attr(minidom::rxml::xml_ncname!("type").to_owned(), "error");
    if let Some(from) = from {
        iq = iq.attr(minidom::rxml::xml_ncname!("from").to_owned(), from);
    }
    if let Some(to) = to {
        iq = iq.attr(minidom::rxml::xml_ncname!("to").to_owned(), to);
    }
    let iq = iq
        .append(payload)
        .append(xmpp_parsers::minidom::Element::from(error))
        .build();
    element_to_xml(iq)
}

pub(crate) fn build_iq_error_xml_typed(
    id: &str,
    from: Option<&str>,
    to: Option<&str>,
    error: xmpp_parsers::stanza_error::StanzaError,
) -> String {
    element_to_xml(build_iq_error_element_typed(id, from, to, error))
}

pub(crate) fn build_iq_error_element_typed(
    id: &str,
    from: Option<&str>,
    to: Option<&str>,
    error: xmpp_parsers::stanza_error::StanzaError,
) -> xmpp_parsers::minidom::Element {
    let mut iq = xmpp_parsers::minidom::Element::builder("iq", "jabber:client")
        .attr(minidom::rxml::xml_ncname!("id").to_owned(), id)
        .attr(minidom::rxml::xml_ncname!("type").to_owned(), "error");
    if let Some(from) = from {
        iq = iq.attr(minidom::rxml::xml_ncname!("from").to_owned(), from);
    }
    if let Some(to) = to {
        iq = iq.attr(minidom::rxml::xml_ncname!("to").to_owned(), to);
    }

    iq.append(xmpp_parsers::minidom::Element::from(error))
        .build()
}

pub(super) fn build_handled_count_too_high_stream_error(
    acknowledged: u32,
    send_count: u32,
) -> String {
    let mut writer = Writer::new(Vec::new());
    let mut stream_error = BytesStart::new("stream:error");
    stream_error.push_attribute(("xmlns:stream", waddle_xmpp::ns::STREAM));
    writer
        .write_event(Event::Start(stream_error))
        .expect("serializing stream error should not fail");

    let mut undefined = BytesStart::new("undefined-condition");
    undefined.push_attribute(("xmlns", waddle_xmpp::ns::STREAMS));
    writer
        .write_event(Event::Empty(undefined))
        .expect("serializing undefined-condition should not fail");

    let mut handled_count = BytesStart::new("handled-count-too-high");
    let acknowledged = acknowledged.to_string();
    let send_count = send_count.to_string();
    handled_count.push_attribute(("xmlns", SM_NS));
    handled_count.push_attribute(("h", acknowledged.as_str()));
    handled_count.push_attribute(("send-count", send_count.as_str()));
    writer
        .write_event(Event::Empty(handled_count))
        .expect("serializing handled-count-too-high should not fail");

    let mut text = BytesStart::new("text");
    text.push_attribute(("xmlns", waddle_xmpp::ns::STREAMS));
    text.push_attribute(("xml:lang", "en"));
    writer
        .write_event(Event::Start(text))
        .expect("serializing stream error text should not fail");
    writer
        .write_event(Event::Text(BytesText::new(&format!(
            "You acknowledged {acknowledged} stanzas, but I only sent you {send_count} so far."
        ))))
        .expect("serializing stream error text should not fail");
    writer
        .write_event(Event::End(BytesEnd::new("text")))
        .expect("serializing stream error text end should not fail");
    writer
        .write_event(Event::End(BytesEnd::new("stream:error")))
        .expect("serializing stream error end should not fail");
    String::from_utf8(writer.into_inner()).expect("quick-xml serializes valid UTF-8")
}

/// Build the RFC 6120 §4.9.3.20 `<stream:error><system-shutdown/>`
/// frame sent to every live session when graceful shutdown begins
/// (issue #1091). Pairs with `websocket_stream_close_xml()` so the
/// client sees error → close → WS close and reconnects elsewhere.
pub(super) fn build_system_shutdown_stream_error() -> String {
    let mut writer = Writer::new(Vec::new());
    let mut stream_error = BytesStart::new("stream:error");
    stream_error.push_attribute(("xmlns:stream", waddle_xmpp::ns::STREAM));
    writer
        .write_event(Event::Start(stream_error))
        .expect("serializing stream error should not fail");

    let mut shutdown = BytesStart::new("system-shutdown");
    shutdown.push_attribute(("xmlns", waddle_xmpp::ns::STREAMS));
    writer
        .write_event(Event::Empty(shutdown))
        .expect("serializing system-shutdown should not fail");

    writer
        .write_event(Event::End(BytesEnd::new("stream:error")))
        .expect("serializing stream error end should not fail");
    String::from_utf8(writer.into_inner()).expect("quick-xml serializes valid UTF-8")
}

/// Build the RFC 6120 §4.9.3.1 `<stream:error><conflict/>` frame sent to a
/// live stream that has just lost cross-node XEP-0198 resume claim-steal
/// arbitration (ADR-0017 Phase 3 Slice 6): "If the former stream is resumed
/// and the server still has the stream for the previously-identified
/// session open at this time, the server SHOULD send a 'conflict' stream
/// error and close that stream" (XEP-0198 "Resumption"). Pairs with
/// `websocket_stream_close_xml()`, identically to
/// `build_system_shutdown_stream_error()`.
pub(super) fn build_conflict_stream_error() -> String {
    let mut writer = Writer::new(Vec::new());
    let mut stream_error = BytesStart::new("stream:error");
    stream_error.push_attribute(("xmlns:stream", waddle_xmpp::ns::STREAM));
    writer
        .write_event(Event::Start(stream_error))
        .expect("serializing stream error should not fail");

    let mut conflict = BytesStart::new("conflict");
    conflict.push_attribute(("xmlns", waddle_xmpp::ns::STREAMS));
    writer
        .write_event(Event::Empty(conflict))
        .expect("serializing conflict should not fail");

    writer
        .write_event(Event::End(BytesEnd::new("stream:error")))
        .expect("serializing stream error end should not fail");
    String::from_utf8(writer.into_inner()).expect("quick-xml serializes valid UTF-8")
}

pub(super) fn build_stream_features_element(authenticated: bool) -> Element {
    let mut features = Element::builder("features", waddle_xmpp::ns::STREAM);
    if authenticated {
        features = features.append(Element::builder("bind", waddle_xmpp::ns::BIND).build());
        features = features.append(Element::builder("ver", waddle_xmpp::ns::ROSTERVER).build());
        features =
            features.append(Element::builder("sub", "urn:xmpp:features:pre-approval").build());
        // XEP-0198: advertise stream management so clients can <enable/> it
        // after bind or <resume/> a detached session.
        features = features.append(Element::builder("sm", SM_NS).build());
    } else {
        features = features.append(
            Element::builder("mechanisms", waddle_xmpp::ns::SASL)
                .append(
                    Element::builder("mechanism", waddle_xmpp::ns::SASL)
                        .append("SCRAM-SHA-256")
                        .build(),
                )
                .append(
                    Element::builder("mechanism", waddle_xmpp::ns::SASL)
                        .append("OAUTHBEARER")
                        .build(),
                )
                .build(),
        );
    }

    features.build()
}

#[cfg(test)]
pub(super) fn build_stream_features_xml(authenticated: bool) -> String {
    element_to_xml(build_stream_features_element(authenticated))
}

pub(super) fn build_stream_features_for_phase_element(phase: &ConnectionPhase) -> Element {
    build_stream_features_element(phase.is_authenticated())
}

pub(super) fn sasl_success_element() -> Element {
    Element::builder("success", waddle_xmpp::ns::SASL).build()
}

#[cfg(test)]
pub(super) fn sasl_success_xml() -> String {
    element_to_xml(sasl_success_element())
}

pub(super) fn sasl_failure_element(condition: &str) -> Element {
    Element::builder("failure", waddle_xmpp::ns::SASL)
        .append(Element::builder(condition, waddle_xmpp::ns::SASL).build())
        .build()
}

pub(super) fn sasl_failure_xml(condition: &str) -> String {
    element_to_xml(sasl_failure_element(condition))
}

#[cfg(test)]
mod stream_error_tests {
    use super::*;

    #[test]
    fn conflict_stream_error_matches_rfc6120_wire_shape() {
        assert_eq!(
            build_conflict_stream_error(),
            concat!(
                "<stream:error xmlns:stream=\"http://etherx.jabber.org/streams\">",
                "<conflict xmlns=\"urn:ietf:params:xml:ns:xmpp-streams\"/>",
                "</stream:error>"
            )
        );
    }

    #[test]
    fn system_shutdown_stream_error_matches_rfc6120_wire_shape() {
        // RFC 6120 §4.9.3.20: <system-shutdown/> in the
        // urn:ietf:params:xml:ns:xmpp-streams namespace, wrapped in
        // <stream:error> qualified by the streams prefix namespace.
        assert_eq!(
            build_system_shutdown_stream_error(),
            concat!(
                "<stream:error xmlns:stream=\"http://etherx.jabber.org/streams\">",
                "<system-shutdown xmlns=\"urn:ietf:params:xml:ns:xmpp-streams\"/>",
                "</stream:error>"
            )
        );
    }
}
