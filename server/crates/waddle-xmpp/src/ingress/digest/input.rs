//! Typed semantic-digest input captured before routing enriches a message.

use std::collections::BTreeMap;

use jid::{BareJid, Jid};
use minidom::{Element, Node};
use thiserror::Error;
use xmpp_parsers::message::{Lang, Message, MessageType};

use super::limits::{
    MAX_ATTRS_PER_ELEMENT, MAX_DEPTH, MAX_ID_LEN, MAX_LANG_ENTRIES, MAX_NAME_LEN, MAX_TEXT_LEN,
    MAX_TOTAL_NODES,
};
use crate::ingress::NormalizedTarget;

const CLIENT_NS: &str = "jabber:client";
const REPLY_NS: &str = "urn:xmpp:reply:0";
const SID_NS: &str = "urn:xmpp:sid:0";
const DELAY_NS: &str = "urn:xmpp:delay";

/// Context which is unavailable after parsing or is specific to the receiving server.
#[derive(Debug, Clone)]
pub struct DigestContext {
    /// Exact addressed form before routing supplies any implicit target.
    pub target: NormalizedTarget,
    /// Authorities whose XEP-0359 stanza identifiers only this server may stamp.
    pub server_authorities: Vec<BareJid>,
    /// The `<message/>` element's own `xml:lang`, verbatim.
    ///
    /// `xmpp_parsers::message::Message` drops the stanza attribute.  The future
    /// binding must therefore read it from the raw element before `Message::try_from`.
    pub stanza_lang: Option<String>,
}

/// A thread identifier and its optional XEP-0201 parent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DigestThread {
    pub(crate) id: String,
    pub(crate) parent: Option<String>,
}

/// A strict XEP-0461 reply reference.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DigestReply {
    pub(crate) id: String,
    pub(crate) to: Option<Jid>,
}

/// Fully validated, typed material consumed by the frozen v1 canonicalizer.
#[derive(Debug, Clone)]
pub struct DigestInput {
    pub(crate) message_type: MessageType,
    pub(crate) stanza_lang: Option<String>,
    pub(crate) target: NormalizedTarget,
    pub(crate) bodies: BTreeMap<Lang, String>,
    pub(crate) subjects: BTreeMap<Lang, String>,
    pub(crate) thread: Option<DigestThread>,
    pub(crate) reply: Option<DigestReply>,
    pub(crate) extensions: Vec<Element>,
    pub(crate) preimage: Vec<u8>,
}

/// Reasons parsed message material cannot safely form a semantic digest.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum DigestInputError {
    #[error("message contains more than one origin-id")]
    DuplicateOriginId,
    #[error("origin-id is malformed")]
    MalformedOriginId,
    /// A trusted authority's stanza-id must be stripped and the input retried;
    /// this is not a whole-stanza rejection at the future binding site.
    #[error("stanza-id was forged for server authority {by}")]
    ForgedServerStanzaId { by: BareJid },
    #[error("message contains more than one thread")]
    DuplicateThread,
    #[error("message contains more than one reply")]
    DuplicateReply,
    #[error("reply payload is malformed")]
    ReplyMalformed,
    #[error("extension nesting depth exceeds the v1 bound")]
    DepthExceeded,
    #[error("extension node count exceeds the v1 bound")]
    NodeCountExceeded,
    #[error("extension attribute count exceeds the v1 bound")]
    AttrCountExceeded,
    #[error("text length exceeds the v1 bound")]
    TextLengthExceeded,
    #[error("namespace, local name, or language key length exceeds the v1 bound")]
    NameLengthExceeded,
    #[error("language-map entry count exceeds the v1 bound")]
    LangCountExceeded,
    #[error("identifier length exceeds the v1 bound")]
    IdLengthExceeded,
    #[error("canonical preimage exceeds the v1 bound")]
    PreimageTooLarge,
}

impl DigestInput {
    /// Capture the semantic fields of a parsed message before routing enrichment.
    pub fn from_parsed(
        message: &Message,
        context: &DigestContext,
    ) -> Result<Self, DigestInputError> {
        validate_langmap(&message.bodies)?;
        validate_langmap(&message.subjects)?;
        validate_name_opt(context.stanza_lang.as_deref())?;

        let mut thread = message.thread.as_ref().map(|thread| DigestThread {
            id: thread.id.clone(),
            parent: thread.parent.clone(),
        });
        if let Some(thread) = &thread {
            validate_id(&thread.id)?;
            if let Some(parent) = &thread.parent {
                validate_id(parent)?;
            }
        }

        let mut origin_count = 0usize;
        let mut reply = None;
        let mut extensions = Vec::with_capacity(message.payloads.len());
        let mut node_count = 0usize;

        for payload in &message.payloads {
            match (payload.ns().as_str(), payload.name()) {
                (SID_NS, "origin-id") => {
                    origin_count += 1;
                    if origin_count > 1 {
                        return Err(DigestInputError::DuplicateOriginId);
                    }
                    validate_origin_id(payload)?;
                }
                (SID_NS, "stanza-id") => validate_stanza_id(payload, context)?,
                (DELAY_NS, "delay") => {}
                (REPLY_NS, "reply") => {
                    if reply.is_some() {
                        return Err(DigestInputError::DuplicateReply);
                    }
                    reply = Some(parse_reply(payload)?);
                }
                (CLIENT_NS, "thread") => {
                    if thread.is_some() {
                        return Err(DigestInputError::DuplicateThread);
                    }
                    let candidate = DigestThread {
                        id: payload.text(),
                        parent: payload.attr("parent").map(str::to_owned),
                    };
                    validate_id(&candidate.id)?;
                    if let Some(parent) = &candidate.parent {
                        validate_id(parent)?;
                    }
                    thread = Some(candidate);
                }
                _ => {
                    validate_element(payload, 1, &mut node_count)?;
                    extensions.push(payload.clone());
                }
            }
        }

        let mut input = Self {
            message_type: message.type_.clone(),
            stanza_lang: context.stanza_lang.clone(),
            target: context.target.clone(),
            bodies: message.bodies.clone(),
            subjects: message.subjects.clone(),
            thread,
            reply,
            extensions,
            preimage: Vec::new(),
        };
        input.preimage = super::v1::encode(&input)?;
        Ok(input)
    }
}

fn validate_langmap(map: &BTreeMap<Lang, String>) -> Result<(), DigestInputError> {
    if map.len() > MAX_LANG_ENTRIES {
        return Err(DigestInputError::LangCountExceeded);
    }
    for (language, text) in map {
        validate_name(language)?;
        validate_text(text)?;
    }
    Ok(())
}

fn validate_origin_id(element: &Element) -> Result<(), DigestInputError> {
    let Some(id) = element.attr("id") else {
        return Err(DigestInputError::MalformedOriginId);
    };
    if id.len() > MAX_ID_LEN {
        return Err(DigestInputError::IdLengthExceeded);
    }
    if id.is_empty()
        || element.attrs().len() != 1
        || element.children().next().is_some()
        || !element.text().is_empty()
    {
        return Err(DigestInputError::MalformedOriginId);
    }
    Ok(())
}

fn validate_stanza_id(element: &Element, context: &DigestContext) -> Result<(), DigestInputError> {
    let Some(by) = element.attr("by") else {
        return Ok(());
    };
    let Ok(by) = by.parse::<BareJid>() else {
        return Ok(());
    };
    if context
        .server_authorities
        .iter()
        .any(|authority| authority == &by)
    {
        return Err(DigestInputError::ForgedServerStanzaId { by });
    }
    Ok(())
}

fn parse_reply(element: &Element) -> Result<DigestReply, DigestInputError> {
    let Some(id) = element.attr("id") else {
        return Err(DigestInputError::ReplyMalformed);
    };
    if id.len() > MAX_ID_LEN {
        return Err(DigestInputError::IdLengthExceeded);
    }
    if id.is_empty()
        || element.children().next().is_some()
        || !element.text().is_empty()
        || element.attrs().iter().any(|((namespace, name), _)| {
            (namespace.as_ref(), name.as_ref()) != ("", "id")
                && (namespace.as_ref(), name.as_ref()) != ("", "to")
        })
    {
        return Err(DigestInputError::ReplyMalformed);
    }
    let to = match element.attr("to") {
        Some(raw) => raw
            .parse::<Jid>()
            .map(Some)
            .map_err(|_| DigestInputError::ReplyMalformed)?,
        None => None,
    };
    Ok(DigestReply {
        id: id.to_owned(),
        to,
    })
}

fn validate_element(
    element: &Element,
    depth: usize,
    node_count: &mut usize,
) -> Result<(), DigestInputError> {
    if depth > MAX_DEPTH {
        return Err(DigestInputError::DepthExceeded);
    }
    increment_nodes(node_count)?;
    validate_name(&element.ns())?;
    validate_name(element.name())?;
    if element.attrs().len() > MAX_ATTRS_PER_ELEMENT {
        return Err(DigestInputError::AttrCountExceeded);
    }
    for ((namespace, name), value) in element.attrs().iter() {
        validate_name(namespace.as_ref())?;
        validate_name(name.as_ref())?;
        validate_text(value)?;
    }
    for node in element.nodes() {
        match node {
            Node::Element(child) => validate_element(child, depth + 1, node_count)?,
            Node::Text(text) => {
                increment_nodes(node_count)?;
                validate_text(text)?;
            }
        }
    }
    Ok(())
}

fn increment_nodes(node_count: &mut usize) -> Result<(), DigestInputError> {
    *node_count = node_count.saturating_add(1);
    if *node_count > MAX_TOTAL_NODES {
        return Err(DigestInputError::NodeCountExceeded);
    }
    Ok(())
}

fn validate_text(value: &str) -> Result<(), DigestInputError> {
    if value.len() > MAX_TEXT_LEN {
        return Err(DigestInputError::TextLengthExceeded);
    }
    Ok(())
}

fn validate_id(value: &str) -> Result<(), DigestInputError> {
    if value.len() > MAX_ID_LEN {
        return Err(DigestInputError::IdLengthExceeded);
    }
    Ok(())
}

fn validate_name_opt(value: Option<&str>) -> Result<(), DigestInputError> {
    if let Some(value) = value {
        validate_name(value)?;
    }
    Ok(())
}

fn validate_name(value: &str) -> Result<(), DigestInputError> {
    if value.len() > MAX_NAME_LEN {
        return Err(DigestInputError::NameLengthExceeded);
    }
    Ok(())
}
