//! XEP-0357 notification payload building and the Waddle push context.

use super::*;

pub const WADDLE_PUSH_CONTEXT_NS: &str = "urn:waddle:push:context:0";
pub const XEP0357_SUMMARY_FORM_TYPE: &str = "urn:xmpp:push:summary";

pub(super) fn build_waddle_context(candidate: &NotificationCandidate) -> Element {
    Element::builder("context", WADDLE_PUSH_CONTEXT_NS)
        .attr(
            minidom::rxml::xml_ncname!("conversation").to_owned(),
            candidate.conversation_jid.to_string(),
        )
        .attr(
            minidom::rxml::xml_ncname!("thread").to_owned(),
            candidate.thread_id.as_str(),
        )
        .attr(
            minidom::rxml::xml_ncname!("class").to_owned(),
            candidate.class.as_db_value(),
        )
        .build()
}

/// Resolved XEP-0357 §5.4 rich summary fields, decided at T1.
///
/// The push decision evaluator resolves these from the recipient's
/// XEP-0492 `<advanced/>` rich-payload opt-in and the message-frozen
/// XEP-0334 storage hints (see [`evaluate_push_gate_at_dispatch`]):
///
/// - `sender` (`last-message-sender`) is set iff the recipient opted in;
///   it is a routing JID present in any delivery and is preserved even
///   when a storage hint strips the body.
/// - `body` (`last-message-body`) is set iff the recipient opted in AND
///   no XEP-0334 `<no-store/>`/`<no-permanent-store/>` hint applies —
///   shipping the body to a third-party push gateway is a semi-permanent
///   store, so the hint always wins over the opt-in.
///
/// The default (`None`/`None`) is the minimal summary: `message-count`
/// plus the Waddle routing context only.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RichSummary {
    pub sender: Option<Jid>,
    pub body: Option<String>,
}

impl RichSummary {
    /// The minimal default — no rich fields (opt-out).
    pub fn minimal() -> Self {
        Self::default()
    }
}

pub fn build_xep0357_notification_payload(
    message_count: u32,
    rich: &RichSummary,
    context: &Element,
) -> Element {
    Element::builder("notification", waddle_xmpp::xep::xep0357::NS_PUSH)
        .append(build_xep0357_summary_form(message_count, rich))
        .append(context.clone())
        .build()
}

fn build_xep0357_summary_form(message_count: u32, rich: &RichSummary) -> Element {
    // XEP-0357 §4 example shows `<x xmlns='jabber:x:data'>` with NO
    // `type` attribute — the form is a passively-encapsulated summary,
    // not the result of a search/query. XEP-0004 §3.2 reserves
    // `type='result'` for query-response contexts which doesn't apply
    // here; emitting it confused at least one client we tested
    // against. Match the §4 example literally.
    let mut builder = Element::builder("x", NS_DATA_FORMS)
        .append(xdata_hidden_field("FORM_TYPE", XEP0357_SUMMARY_FORM_TYPE))
        .append(xdata_field("message-count", &message_count.to_string()));
    // XEP-0357 §5.4 optional rich fields. Order matches the spec
    // example: sender before body.
    if let Some(sender) = &rich.sender {
        builder = builder.append(xdata_field("last-message-sender", &sender.to_string()));
    }
    if let Some(body) = &rich.body {
        builder = builder.append(xdata_field("last-message-body", body));
    }
    builder.build()
}

fn xdata_hidden_field(var: &str, value: &str) -> Element {
    Element::builder("field", NS_DATA_FORMS)
        .attr(minidom::rxml::xml_ncname!("var").to_owned(), var)
        .attr(minidom::rxml::xml_ncname!("type").to_owned(), "hidden")
        .append(
            Element::builder("value", NS_DATA_FORMS)
                .append(value)
                .build(),
        )
        .build()
}

fn xdata_field(var: &str, value: &str) -> Element {
    Element::builder("field", NS_DATA_FORMS)
        .attr(minidom::rxml::xml_ncname!("var").to_owned(), var)
        .append(
            Element::builder("value", NS_DATA_FORMS)
                .append(value)
                .build(),
        )
        .build()
}

pub fn target_from_subscription(
    subscription: &waddle_xmpp::push::PushSubscription,
) -> Result<Option<NotificationOutboxTarget>, NotificationOutboxError> {
    let Some(node) = subscription.node.as_ref() else {
        return Ok(None);
    };
    let push_service_jid = subscription.service_jid.parse::<BareJid>().map_err(|_| {
        NotificationOutboxError::InvalidPushServiceBareJid(subscription.service_jid.clone())
    })?;
    Ok(Some(NotificationOutboxTarget::new(
        push_service_jid,
        PushServiceNodeName::new(node.clone())?,
    )))
}

pub fn publish_options_form_type_is_xep0060(publish_options: &Element) -> bool {
    publish_options.children().any(|child| {
        child.is("field", NS_DATA_FORMS)
            && child.attr("var") == Some("FORM_TYPE")
            && child.children().any(|value| {
                value.is("value", NS_DATA_FORMS) && value.text() == NS_PUBSUB_PUBLISH_OPTIONS
            })
    })
}
