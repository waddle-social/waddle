//! XEP-0050 Ad-Hoc Commands — client-side typed IQ builders.
//!
//! xmpp-parsers 0.22 does not ship a XEP-0050 module; this module
//! mirrors the shape used server-side (`waddle-xmpp::xep::xep0050`)
//! at the client boundary. The server-side crate is not depended on
//! directly — that would invert the dependency direction (client →
//! server).

use minidom::Element;
use xmpp_parsers::data_forms::DataForm;

use crate::discovery::ids::next_id;
use crate::discovery::CLIENT_NS;

/// XEP-0050 §2 namespace.
pub const NS_COMMANDS: &str = "http://jabber.org/protocol/commands";
/// XEP-0004 data forms namespace, used for the `<x/>` payload inside
/// `<command/>`.
pub const NS_DATA_FORMS: &str = "jabber:x:data";

/// XEP-0050 §3 `action` attribute. Typed-payloads hard rule: callers
/// pass this enum, not a `&str`, so a typo can't reach the wire.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AdHocAction {
    Execute,
    Cancel,
    Next,
    Prev,
    Complete,
}

impl AdHocAction {
    pub fn as_wire_str(self) -> &'static str {
        match self {
            Self::Execute => "execute",
            Self::Cancel => "cancel",
            Self::Next => "next",
            Self::Prev => "prev",
            Self::Complete => "complete",
        }
    }
}

/// XEP-0050 §3 `status` attribute on responses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AdHocStatus {
    Executing,
    Completed,
    Canceled,
}

impl AdHocStatus {
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "executing" => Some(Self::Executing),
            "completed" => Some(Self::Completed),
            "canceled" => Some(Self::Canceled),
            _ => None,
        }
    }
}

/// Parsed `<command/>` payload extracted from an IQ result.
#[derive(Debug, Clone, PartialEq)]
pub struct CommandResponse {
    pub node: String,
    pub session_id: Option<String>,
    pub status: AdHocStatus,
    pub form: Option<DataForm>,
}

/// Build an IQ that initiates a XEP-0050 ad-hoc command with no
/// session id. Use this for the FIRST stage of the multi-step dance,
/// typically with [`AdHocAction::Execute`].
pub fn build_xep0050_command_request(
    service_jid: &str,
    node: &str,
    action: AdHocAction,
    form: Option<DataForm>,
) -> Element {
    // xmpp_parsers DataForm round-trips through minidom::Element via
    // AsXml. The conversion is infallible for serialization.
    let id = format!("adhoc-{}", next_id());
    build_command_iq(service_jid, node, &id, None, action, form.map(Element::from))
}

/// Like [`build_xep0050_command_request`], but with a caller-supplied
/// IQ id. Use this when the caller owns response correlation (the FFI
/// verbs stamp UUID ids).
pub fn build_xep0050_command_request_with_id(
    service_jid: &str,
    node: &str,
    iq_id: &str,
    action: AdHocAction,
    form: Option<DataForm>,
) -> Element {
    build_command_iq(service_jid, node, iq_id, None, action, form.map(Element::from))
}

/// Build a SUBSEQUENT-stage IQ that carries the `sessionid` returned
/// by the service in the first response. Caller is responsible for
/// propagating the session id verbatim from
/// [`CommandResponse::session_id`].
pub fn build_xep0050_command_request_with_session(
    service_jid: &str,
    node: &str,
    session_id: &str,
    action: AdHocAction,
    form: Option<DataForm>,
) -> Element {
    let id = format!("adhoc-{}", next_id());
    build_command_iq(
        service_jid,
        node,
        &id,
        Some(session_id),
        action,
        form.map(Element::from),
    )
}

/// Build a command IQ around a pre-built `jabber:x:data` form
/// element. Used where the wire form must not carry a XEP-0068
/// `FORM_TYPE` field (the extension-command surface mirrors the wasm
/// chat client, which submits bare `var`/`value` fields), which
/// [`xmpp_parsers::data_forms::DataForm`] builders cannot express
/// per-field-type faithfully.
pub fn build_xep0050_command_request_with_form_element(
    service_jid: &str,
    node: &str,
    session_id: Option<&str>,
    action: AdHocAction,
    form: Option<Element>,
) -> Element {
    let id = format!("adhoc-{}", next_id());
    build_command_iq(service_jid, node, &id, session_id, action, form)
}

fn build_command_iq(
    service_jid: &str,
    node: &str,
    id: &str,
    session_id: Option<&str>,
    action: AdHocAction,
    form: Option<Element>,
) -> Element {
    let mut command = Element::builder("command", NS_COMMANDS)
        .attr(minidom::rxml::xml_ncname!("node").to_owned(), node)
        .attr(
            minidom::rxml::xml_ncname!("action").to_owned(),
            action.as_wire_str(),
        );
    if let Some(session_id) = session_id {
        command = command.attr(
            minidom::rxml::xml_ncname!("sessionid").to_owned(),
            session_id,
        );
    }
    if let Some(form) = form {
        command = command.append(form);
    }
    Element::builder("iq", CLIENT_NS)
        .attr(minidom::rxml::xml_ncname!("type").to_owned(), "set")
        .attr(minidom::rxml::xml_ncname!("to").to_owned(), service_jid)
        .attr(minidom::rxml::xml_ncname!("id").to_owned(), id)
        .append(command.build())
        .build()
}

/// Parse a `<command/>` payload out of an IQ result. Returns `None`
/// when the IQ does not carry a `<command/>` child or the `status`
/// attribute is missing/unknown — both are server-side bugs the
/// caller surfaces as a typed protocol error.
pub fn parse_command_response(iq: &Element) -> Option<CommandResponse> {
    let command = iq.get_child("command", NS_COMMANDS)?;
    let status = AdHocStatus::parse(command.attr("status")?)?;
    let form = command
        .get_child("x", NS_DATA_FORMS)
        .cloned()
        .and_then(|elem| DataForm::try_from(elem).ok());
    Some(CommandResponse {
        node: command.attr("node")?.to_string(),
        session_id: command.attr("sessionid").map(str::to_string),
        status,
        form,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use xmpp_parsers::data_forms::{DataForm, DataFormType};

    #[test]
    fn build_command_request_execute_carries_node_and_action() {
        let iq = build_xep0050_command_request(
            "push.example.com",
            "register-device",
            AdHocAction::Execute,
            None,
        );
        assert_eq!(iq.attr("type"), Some("set"));
        assert_eq!(iq.attr("to"), Some("push.example.com"));
        let command = iq.get_child("command", NS_COMMANDS).expect("command");
        assert_eq!(command.attr("node"), Some("register-device"));
        assert_eq!(command.attr("action"), Some("execute"));
        assert_eq!(command.attr("sessionid"), None);
        assert!(command.get_child("x", NS_DATA_FORMS).is_none());
    }

    #[test]
    fn build_command_request_with_id_uses_caller_iq_id() {
        let iq = build_xep0050_command_request_with_id(
            "waddle.test",
            "urn:waddle:group-dm:create:0",
            "iq-uuid-1",
            AdHocAction::Execute,
            None,
        );
        assert_eq!(iq.attr("id"), Some("iq-uuid-1"));
        assert_eq!(iq.attr("type"), Some("set"));
        let command = iq.get_child("command", NS_COMMANDS).expect("command");
        assert_eq!(command.attr("action"), Some("execute"));
        assert_eq!(command.attr("sessionid"), None);
    }

    #[test]
    fn build_command_request_with_session_carries_sessionid_and_form() {
        let form = DataForm::new(
            DataFormType::Submit,
            "urn:waddle:push-service:commands:register-device:0",
            vec![],
        );
        let iq = build_xep0050_command_request_with_session(
            "push.example.com",
            "register-device",
            "session-1",
            AdHocAction::Complete,
            Some(form),
        );
        let command = iq.get_child("command", NS_COMMANDS).expect("command");
        assert_eq!(command.attr("sessionid"), Some("session-1"));
        assert_eq!(command.attr("action"), Some("complete"));
        let x = command
            .get_child("x", NS_DATA_FORMS)
            .expect("submitted form");
        assert_eq!(x.attr("type"), Some("submit"));
    }

    #[test]
    fn parse_command_response_extracts_typed_fields() {
        let iq = Element::builder("iq", CLIENT_NS)
            .attr(minidom::rxml::xml_ncname!("type").to_owned(), "result")
            .append(
                Element::builder("command", NS_COMMANDS)
                    .attr(
                        minidom::rxml::xml_ncname!("node").to_owned(),
                        "register-device",
                    )
                    .attr(minidom::rxml::xml_ncname!("sessionid").to_owned(), "s-1")
                    .attr(minidom::rxml::xml_ncname!("status").to_owned(), "executing")
                    .build(),
            )
            .build();
        let parsed = parse_command_response(&iq).expect("parses");
        assert_eq!(parsed.node, "register-device");
        assert_eq!(parsed.session_id.as_deref(), Some("s-1"));
        assert_eq!(parsed.status, AdHocStatus::Executing);
        assert!(parsed.form.is_none());
    }

    #[test]
    fn parse_command_response_returns_none_without_status() {
        let iq = Element::builder("iq", CLIENT_NS)
            .attr(minidom::rxml::xml_ncname!("type").to_owned(), "result")
            .append(
                Element::builder("command", NS_COMMANDS)
                    .attr(
                        minidom::rxml::xml_ncname!("node").to_owned(),
                        "register-device",
                    )
                    .build(),
            )
            .build();
        assert!(parse_command_response(&iq).is_none());
    }

    #[test]
    fn parse_command_response_returns_none_for_unknown_status() {
        let iq = Element::builder("iq", CLIENT_NS)
            .attr(minidom::rxml::xml_ncname!("type").to_owned(), "result")
            .append(
                Element::builder("command", NS_COMMANDS)
                    .attr(
                        minidom::rxml::xml_ncname!("node").to_owned(),
                        "register-device",
                    )
                    .attr(minidom::rxml::xml_ncname!("status").to_owned(), "bogus")
                    .build(),
            )
            .build();
        assert!(parse_command_response(&iq).is_none());
    }
}
