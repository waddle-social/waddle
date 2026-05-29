use minidom::Element;
use xmpp_parsers::iq::Iq;

use crate::xep::xep0004::{FromElement, IntoElement};

use super::{Command, CommandError, NODE_COMMANDS, NS_COMMANDS};

// ---------------------------------------------------------------------------
// IQ helpers
// ---------------------------------------------------------------------------

/// Check if an IQ stanza is an ad-hoc command request (IQ set with command element).
pub fn is_command_request(iq: &Iq) -> bool {
    matches!(iq, Iq::Set { payload, .. } if payload.name() == "command" && payload.ns() == NS_COMMANDS)
}

/// Parse an ad-hoc command from an IQ set stanza.
pub fn parse_command_from_iq(iq: &Iq) -> Result<Command, CommandError> {
    match iq {
        Iq::Set { payload, .. } if payload.name() == "command" && payload.ns() == NS_COMMANDS => {
            Command::from_element(payload)
        }
        _ => Err(CommandError::NotACommandIq),
    }
}

/// Build an IQ result containing a command response.
pub fn build_command_result(original_iq: &Iq, command: &Command) -> Iq {
    Iq::Result {
        from: original_iq.to().cloned(),
        to: original_iq.from().cloned(),
        id: original_iq.id().to_string(),
        payload: Some(command.into_element()),
    }
}

// Command §4.4 error responses are built typed from `XmppError::AdHocCommand`
// at the dispatch boundary (see `build_xmpp_error_response`), so this module
// no longer hand-rolls per-condition `Iq::Error` builders.

// ---------------------------------------------------------------------------
// Disco helpers
// ---------------------------------------------------------------------------

/// Build a disco#items element for the commands node listing.
///
/// Each tuple is `(node, name)` representing an available command.
pub fn build_command_items(original_iq: &Iq, commands: &[(&str, &str)], responder_jid: &str) -> Iq {
    use crate::disco::items::DISCO_ITEMS_NS;

    let mut query = Element::builder("query", DISCO_ITEMS_NS)
        .attr(minidom::rxml::xml_ncname!("node").to_owned(), NODE_COMMANDS);

    for (node, name) in commands {
        let item = Element::builder("item", DISCO_ITEMS_NS)
            .attr(minidom::rxml::xml_ncname!("jid").to_owned(), responder_jid)
            .attr(minidom::rxml::xml_ncname!("node").to_owned(), *node)
            .attr(minidom::rxml::xml_ncname!("name").to_owned(), *name)
            .build();
        query = query.append(item);
    }

    Iq::Result {
        from: original_iq.to().cloned(),
        to: original_iq.from().cloned(),
        id: original_iq.id().to_string(),
        payload: Some(query.build()),
    }
}

/// Check if a disco#items query is for the ad-hoc commands node.
pub fn is_commands_disco_items(iq: &Iq) -> bool {
    use crate::disco::items::DISCO_ITEMS_NS;

    match iq {
        Iq::Get { payload, .. } => {
            payload.name() == "query"
                && payload.ns() == DISCO_ITEMS_NS
                && payload.attr("node") == Some(NODE_COMMANDS)
        }
        _ => false,
    }
}

/// Check if a disco#info query is for the ad-hoc commands node.
pub fn is_commands_disco_info(iq: &Iq) -> bool {
    use crate::disco::info::DISCO_INFO_NS;

    match iq {
        Iq::Get { payload, .. } => {
            payload.name() == "query"
                && payload.ns() == DISCO_INFO_NS
                && payload.attr("node") == Some(NODE_COMMANDS)
        }
        _ => false,
    }
}

/// Check if a disco#info query is for a specific command node.
pub fn is_command_node_disco_info(iq: &Iq, node: &str) -> bool {
    use crate::disco::info::DISCO_INFO_NS;

    match iq {
        Iq::Get { payload, .. } => {
            payload.name() == "query"
                && payload.ns() == DISCO_INFO_NS
                && payload.attr("node") == Some(node)
        }
        _ => false,
    }
}
