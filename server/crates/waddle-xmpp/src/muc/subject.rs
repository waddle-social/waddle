use chrono::{DateTime, Utc};
use jid::BareJid;
use serde::{Deserialize, Serialize};

/// XEP-0045 §8.1: a room subject change contains a subject and neither
/// a body nor a thread. Callers supply the shape of their typed message.
pub fn is_groupchat_subject_change(
    message_type: &xmpp_parsers::message::MessageType,
    has_subject: bool,
    has_body: bool,
    has_thread: bool,
) -> bool {
    *message_type == xmpp_parsers::message::MessageType::Groupchat
        && has_subject
        && !has_body
        && !has_thread
}

/// [`is_groupchat_subject_change`] for a live [`Message`].
///
/// Whitespace-only bodies count as absent (hostile-client guard in the
/// room subject handler), and the thread is read the payload-aware way:
/// the inbound parser moves `<thread parent='…'/>` into `payloads`, so
/// `message.thread` alone would miss parented threads.
pub fn is_groupchat_subject_change_message(message: &xmpp_parsers::message::Message) -> bool {
    is_groupchat_subject_change(
        &message.type_,
        !message.subjects.is_empty(),
        message.bodies.values().any(|body| !body.trim().is_empty()),
        waddle_xmpp_core::xep0201::thread_id_from_message(message).is_some(),
    )
}

/// Typed multi-language subject-text map carried by [`SubjectState`]
/// and the `OutboundEvent::PersistRoomSubject` / `RoomActor::SetSubject`
/// payloads.
///
/// Newtype around `BTreeMap<xml:lang, subject-text>` per the
/// typed-payloads hard rule (CLAUDE.md): a generic `BTreeMap<String, String>`
/// at a protocol boundary makes the xml:lang / subject-text
/// relationship type-indistinguishable from any other map of strings.
/// `RoomSubjectTexts` encapsulates that relationship and the
/// `xmpp_parsers::message::Message::subjects` ↔ persisted-map
/// conversion in one place so call sites don't reinvent it.
///
/// The empty-string key is the default-language entry, mirroring
/// `xmpp_parsers::message::Message::subjects`'s own
/// `BTreeMap<Lang, Subject>` shape (where `Lang = String`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RoomSubjectTexts(std::collections::BTreeMap<String, String>);

impl RoomSubjectTexts {
    /// Empty map (no subject elements).
    pub fn new() -> Self {
        Self::default()
    }

    /// Capture a `Message`'s typed `subjects` field by cloning every
    /// language-tagged subject text and pairing it with its
    /// `xml:lang` key.
    pub fn from_message_subjects(
        subjects: &std::collections::BTreeMap<xmpp_parsers::message::Lang, String>,
    ) -> Self {
        Self(
            subjects
                .iter()
                .map(|(lang, text)| (lang.0.clone(), text.clone()))
                .collect(),
        )
    }

    /// Insert one `<subject xml:lang='...'>` per persisted entry into
    /// `msg.subjects`. Used by the join-time replay builder.
    pub fn apply_to_message(&self, msg: &mut xmpp_parsers::message::Message) {
        for (lang, text) in &self.0 {
            msg.subjects
                .insert(xmpp_parsers::message::Lang(lang.clone()), text.clone());
        }
    }

    pub fn iter(&self) -> impl Iterator<Item = (&String, &String)> {
        self.0.iter()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn get(&self, lang: &str) -> Option<&str> {
        self.0.get(lang).map(String::as_str)
    }
}

impl From<std::collections::BTreeMap<String, String>> for RoomSubjectTexts {
    fn from(map: std::collections::BTreeMap<String, String>) -> Self {
        Self(map)
    }
}

impl FromIterator<(String, String)> for RoomSubjectTexts {
    fn from_iter<I: IntoIterator<Item = (String, String)>>(iter: I) -> Self {
        Self(iter.into_iter().collect())
    }
}

/// XEP-0045 §7.2.15 / §8.1 room subject state.
///
/// `texts` carries every `<subject xml:lang='...'>` variant from the
/// originating §8.1 message, keyed by `xml:lang` (the empty string is
/// the default-language entry). All entries with empty values
/// represent an **explicitly cleared** subject — XEP-0045 §7.2.15
/// distinguishes this from "never set", which is represented by
/// `MucRoom.subject == None`. Persisting every language variant rather
/// than a single canonical text avoids losing localized subjects: the
/// reflected broadcast and the join-time replay would otherwise carry
/// different `<subject>` element sets.
///
/// `setter` and `setter_nick` are the bare JID of the occupant who
/// last set the subject and the nickname they were using at that
/// moment; `setter_nick` is frozen here rather than re-resolved at
/// emission so that historical join-time emissions remain stable
/// across nick changes and after the setter has left. `set_at` powers
/// the XEP-0203 `<delay/>` stamp on the join-time emission and the
/// XEP-0421 occupant-id derivation uses `setter` as the bare-JID input.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SubjectState {
    pub texts: RoomSubjectTexts,
    pub setter: BareJid,
    pub setter_nick: String,
    pub set_at: DateTime<Utc>,
}
