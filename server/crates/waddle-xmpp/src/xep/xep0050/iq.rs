use minidom::Element;
use xmpp_parsers::iq::{Iq, IqType};

use crate::xep::xep0004::{FromElement, IntoElement};

use super::{Command, CommandError, NODE_COMMANDS, NS_COMMANDS};

// ---------------------------------------------------------------------------
// IQ helpers
// ---------------------------------------------------------------------------

/// Check if an IQ stanza is an ad-hoc command request (IQ set with command element).
pub fn is_command_request(iq: &Iq) -> bool {
    matches!(&iq.payload, IqType::Set(elem) if elem.name() == "command" && elem.ns() == NS_COMMANDS)
}

/// Parse an ad-hoc command from an IQ set stanza.
pub fn parse_command_from_iq(iq: &Iq) -> Result<Command, CommandError> {
    match &iq.payload {
        IqType::Set(elem) if elem.name() == "command" && elem.ns() == NS_COMMANDS => {
            Command::from_element(elem)
        }
        _ => Err(CommandError::NotACommandIq),
    }
}

/// Build an IQ result containing a command response.
pub fn build_command_result(original_iq: &Iq, command: &Command) -> Iq {
    Iq {
        from: original_iq.to.clone(),
        to: original_iq.from.clone(),
        id: original_iq.id.clone(),
        payload: IqType::Result(Some(command.into_element())),
    }
}

/// Build a command IQ error response with a specific condition.
///
/// The error element includes the command-specific condition from the
/// `http://jabber.org/protocol/commands` namespace alongside the standard
/// stanza error condition via the `other` extension field.
pub fn build_command_error(
    original_iq: &Iq,
    error_type: &str,
    stanza_condition: &str,
    command_condition: Option<&str>,
) -> Iq {
    use xmpp_parsers::stanza_error::{DefinedCondition, ErrorType, StanzaError};

    let et = match error_type {
        "modify" => ErrorType::Modify,
        "cancel" => ErrorType::Cancel,
        "auth" => ErrorType::Auth,
        "wait" => ErrorType::Wait,
        _ => ErrorType::Cancel,
    };

    let dc = match stanza_condition {
        "bad-request" => DefinedCondition::BadRequest,
        "not-allowed" => DefinedCondition::NotAllowed,
        "forbidden" => DefinedCondition::Forbidden,
        "item-not-found" => DefinedCondition::ItemNotFound,
        "feature-not-implemented" => DefinedCondition::FeatureNotImplemented,
        "not-acceptable" => DefinedCondition::NotAcceptable,
        "service-unavailable" => DefinedCondition::ServiceUnavailable,
        _ => DefinedCondition::UndefinedCondition,
    };

    let mut stanza_error = StanzaError::new(et, dc, "en", "");

    if let Some(cc) = command_condition {
        stanza_error.other = Some(Element::builder(cc, NS_COMMANDS).build());
    }

    Iq {
        from: original_iq.to.clone(),
        to: original_iq.from.clone(),
        id: original_iq.id.clone(),
        payload: IqType::Error(stanza_error),
    }
}

/// Build a `bad-request` error with an optional command-specific condition.
pub fn build_bad_request(original_iq: &Iq, command_condition: Option<&str>) -> Iq {
    build_command_error(original_iq, "modify", "bad-request", command_condition)
}

/// Build a `not-allowed` error (e.g., requester lacks permission).
pub fn build_not_allowed(original_iq: &Iq) -> Iq {
    build_command_error(original_iq, "cancel", "not-allowed", None)
}

/// Build a `forbidden` error.
pub fn build_forbidden(original_iq: &Iq) -> Iq {
    build_command_error(original_iq, "auth", "forbidden", None)
}

/// Build an `item-not-found` error (unknown command node).
pub fn build_item_not_found(original_iq: &Iq) -> Iq {
    build_command_error(original_iq, "cancel", "item-not-found", None)
}

/// Build a `bad-sessionid` error (invalid or expired session).
pub fn build_bad_session_id(original_iq: &Iq) -> Iq {
    build_command_error(original_iq, "modify", "bad-request", Some("bad-sessionid"))
}

/// Build a `session-expired` error.
pub fn build_session_expired(original_iq: &Iq) -> Iq {
    build_command_error(
        original_iq,
        "modify",
        "not-allowed",
        Some("session-expired"),
    )
}

// ---------------------------------------------------------------------------
// Disco helpers
// ---------------------------------------------------------------------------

/// Build a disco#items element for the commands node listing.
///
/// Each tuple is `(node, name)` representing an available command.
pub fn build_command_items(original_iq: &Iq, commands: &[(&str, &str)], responder_jid: &str) -> Iq {
    use crate::disco::items::DISCO_ITEMS_NS;

    let mut query = Element::builder("query", DISCO_ITEMS_NS).attr("node", NODE_COMMANDS);

    for (node, name) in commands {
        let item = Element::builder("item", DISCO_ITEMS_NS)
            .attr("jid", responder_jid)
            .attr("node", *node)
            .attr("name", *name)
            .build();
        query = query.append(item);
    }

    Iq {
        from: original_iq.to.clone(),
        to: original_iq.from.clone(),
        id: original_iq.id.clone(),
        payload: IqType::Result(Some(query.build())),
    }
}

/// Check if a disco#items query is for the ad-hoc commands node.
pub fn is_commands_disco_items(iq: &Iq) -> bool {
    use crate::disco::items::DISCO_ITEMS_NS;

    match &iq.payload {
        IqType::Get(elem) => {
            elem.name() == "query"
                && elem.ns() == DISCO_ITEMS_NS
                && elem.attr("node") == Some(NODE_COMMANDS)
        }
        _ => false,
    }
}

/// Check if a disco#info query is for the ad-hoc commands node.
pub fn is_commands_disco_info(iq: &Iq) -> bool {
    use crate::disco::info::DISCO_INFO_NS;

    match &iq.payload {
        IqType::Get(elem) => {
            elem.name() == "query"
                && elem.ns() == DISCO_INFO_NS
                && elem.attr("node") == Some(NODE_COMMANDS)
        }
        _ => false,
    }
}

/// Check if a disco#info query is for a specific command node.
pub fn is_command_node_disco_info(iq: &Iq, node: &str) -> bool {
    use crate::disco::info::DISCO_INFO_NS;

    match &iq.payload {
        IqType::Get(elem) => {
            elem.name() == "query" && elem.ns() == DISCO_INFO_NS && elem.attr("node") == Some(node)
        }
        _ => false,
    }
}
