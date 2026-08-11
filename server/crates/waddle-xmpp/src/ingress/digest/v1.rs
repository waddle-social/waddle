//! Frozen SemanticDigest v1 canonicalization.
//!
//! `digest = SHA-256(preimage)`, where all integers are fixed-width big-endian,
//! `str(s) = u32 BE UTF-8-byte-length || UTF-8 bytes`, and the preimage is:
//!
//! ```text
//! domain || msg_type || stanza_lang || target || bodies || subjects || thread || reply || extensions
//! domain     = b"waddle:semantic-digest:v1\\0"
//! stanza_lang= 00 | 01 str(lang)
//! target     = 00 | 01 str(bare-jid) | 02 str(full-jid)
//! langmap    = u32 count (str(lang) str(text))*
//! thread     = 00 | 01 str(id) (00 | 01 str(parent))
//! reply      = 00 | 01 str(id) (00 | 01 str(jid))
//! extensions = u32 count element*  (in document order)
//! element    = e0 str(namespace) str(local) u32 attr-count attr* u32 node-count node*
//! attr       = str(namespace) str(local) str(value)  (sorted by UTF-8 namespace, local)
//! node       = 01 str(text) | element
//! ```
//!
//! Prefixes are never serialized; names are expanded namespace/local pairs.
//! Retained trees carry only their local attributes: v1 intentionally does not
//! materialize inherited `xml:lang`, so inherited and explicitly repeated
//! language can conflict rather than falsely deduplicate.

use minidom::{Element, Node};
use sha2::{Digest, Sha256};
use xmpp_parsers::message::MessageType;

use super::input::{DigestInput, DigestInputError};
use super::limits::MAX_PREIMAGE_BYTES;
use super::{DigestVersion, SemanticDigest};
use crate::ingress::NormalizedTarget;

const DOMAIN: &[u8] = b"waddle:semantic-digest:v1\0";
const ELEMENT_TAG: u8 = 0xe0;
const TEXT_TAG: u8 = 0x01;

/// Calculate the frozen version-one digest for validated input.
pub fn digest(input: &DigestInput) -> SemanticDigest {
    let mut hasher = Sha256::new();
    hasher.update(&input.preimage);
    SemanticDigest::from_parts(DigestVersion::V1, hasher.finalize().into())
}

pub(crate) fn encode(input: &DigestInput) -> Result<Vec<u8>, DigestInputError> {
    let mut output = Vec::new();
    let mut writer = PreimageWriter::new(&mut output);
    writer.bytes(DOMAIN)?;
    writer.byte(message_type(input.message_type.clone()))?;
    writer.option_str(input.stanza_lang.as_deref())?;
    match &input.target {
        NormalizedTarget::Absent => writer.byte(0)?,
        NormalizedTarget::Bare(jid) => {
            writer.byte(1)?;
            writer.str(&jid.to_string())?;
        }
        NormalizedTarget::Full(jid) => {
            writer.byte(2)?;
            writer.str(&jid.to_string())?;
        }
    }
    writer.langmap(&input.bodies)?;
    writer.langmap(&input.subjects)?;
    match &input.thread {
        None => writer.byte(0)?,
        Some(thread) => {
            writer.byte(1)?;
            writer.str(&thread.id)?;
            writer.option_str(thread.parent.as_deref())?;
        }
    }
    match &input.reply {
        None => writer.byte(0)?,
        Some(reply) => {
            writer.byte(1)?;
            writer.str(&reply.id)?;
            match &reply.to {
                None => writer.byte(0)?,
                Some(to) => {
                    writer.byte(1)?;
                    writer.str(&to.to_string())?;
                }
            }
        }
    }
    writer.u32(input.extensions.len())?;
    for extension in &input.extensions {
        writer.element(extension)?;
    }
    Ok(output)
}

fn message_type(value: MessageType) -> u8 {
    match value {
        MessageType::Normal => 0,
        MessageType::Chat => 1,
        MessageType::Groupchat => 2,
        MessageType::Headline => 3,
        MessageType::Error => 4,
    }
}

struct PreimageWriter<'a> {
    output: &'a mut Vec<u8>,
}

impl<'a> PreimageWriter<'a> {
    fn new(output: &'a mut Vec<u8>) -> Self {
        Self { output }
    }

    fn bytes(&mut self, value: &[u8]) -> Result<(), DigestInputError> {
        let next_len = self
            .output
            .len()
            .checked_add(value.len())
            .ok_or(DigestInputError::PreimageTooLarge)?;
        if next_len > MAX_PREIMAGE_BYTES {
            return Err(DigestInputError::PreimageTooLarge);
        }
        self.output.extend_from_slice(value);
        Ok(())
    }

    fn byte(&mut self, value: u8) -> Result<(), DigestInputError> {
        self.bytes(&[value])
    }

    fn u32(&mut self, value: usize) -> Result<(), DigestInputError> {
        let value = u32::try_from(value).map_err(|_| DigestInputError::PreimageTooLarge)?;
        self.bytes(&value.to_be_bytes())
    }

    fn str(&mut self, value: &str) -> Result<(), DigestInputError> {
        self.u32(value.len())?;
        self.bytes(value.as_bytes())
    }

    fn option_str(&mut self, value: Option<&str>) -> Result<(), DigestInputError> {
        match value {
            None => self.byte(0),
            Some(value) => {
                self.byte(1)?;
                self.str(value)
            }
        }
    }

    fn langmap(
        &mut self,
        map: &std::collections::BTreeMap<xmpp_parsers::message::Lang, String>,
    ) -> Result<(), DigestInputError> {
        self.u32(map.len())?;
        for (language, text) in map {
            self.str(language)?;
            self.str(text)?;
        }
        Ok(())
    }

    fn element(&mut self, element: &Element) -> Result<(), DigestInputError> {
        self.byte(ELEMENT_TAG)?;
        self.str(&element.ns())?;
        self.str(element.name())?;
        let mut attributes: Vec<_> = element.attrs().iter().collect();
        attributes.sort_unstable_by(|left, right| {
            left.0
                 .0
                .cmp(right.0 .0)
                .then_with(|| left.0 .1.cmp(right.0 .1))
        });
        self.u32(attributes.len())?;
        for ((namespace, name), value) in attributes {
            self.str(namespace.as_ref())?;
            self.str(name.as_ref())?;
            self.str(value)?;
        }
        self.u32(element.nodes().count())?;
        for node in element.nodes() {
            match node {
                Node::Text(text) => {
                    self.byte(TEXT_TAG)?;
                    self.str(text)?;
                }
                Node::Element(child) => self.element(child)?,
            }
        }
        Ok(())
    }
}
