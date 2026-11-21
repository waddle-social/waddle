//! Authorization context of a retained C2S Space discovery reply.

use crate::identity::CanonicalBareJid;
use crate::protocol::frame::MAX_FRAME_SIZE;
use crate::pubsub::SubId;
use jid::{BareJid, FullJid};
use std::collections::HashSet;
use uuid::Uuid;

const HEADER: &[u8; 4] = b"SPR\x01";
const TYPED_HEADER: &[u8; 4] = b"SPR\x02";
const MAX_JID_BYTES: usize = 3071;
const UUID_BYTES: usize = 16;
const MAX_SUBSCRIPTION_ID_BYTES: usize = 256;
const FIXED_BYTES: usize = HEADER.len() + 4 * size_of::<u32>() + 2 * UUID_BYTES + size_of::<i64>();

/// Maximum protected room rows in one content result.
pub const MAX_PROTECTED_SPACE_ROOM_DEPENDENCIES: usize = 256;

/// Maximum size of either version of the serialized descriptor.
pub const MAX_PROTECTED_SPACE_REPLY_BYTES: usize = FIXED_BYTES
    + 1
    + 2 * MAX_JID_BYTES
    + MAX_FRAME_SIZE
    + size_of::<u16>()
    + MAX_PROTECTED_SPACE_ROOM_DEPENDENCIES
        * (size_of::<u32>() + MAX_JID_BYTES + 2 * UUID_BYTES + size_of::<i64>());

/// Exact XMPP stanza payload admitted by the stored Space decision.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProtectedSpaceReplyKind {
    DiscoInfo,
    DiscoItems,
    PubSubItems,
    Subscribe,
    BookmarkEvent,
    OwnerAffiliations,
    OwnerAffiliationRefusals,
    OwnerConfiguration,
    DeleteEvent,
}

/// Identity of the original access decision, without a replica lease.
///
/// Decoding establishes structural validity. The resource host must recheck the
/// original decision against current authority before disclosing the reply.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProtectedSpaceReplyDescriptor {
    kind: ProtectedSpaceReplyKind,
    room_dependencies: Option<Vec<ProtectedSpaceRoomDependency>>,
    subscription_id: Option<SubId>,
    bookmark_event: Option<ProtectedSpaceBookmarkEvent>,
    service: BareJid,
    node: String,
    recipient: FullJid,
    id: String,
    lifecycle: Uuid,
    incarnation: Uuid,
    generation: i64,
}

/// A missing, malformed, or unsupported protected reply descriptor is unusable.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
#[error("invalid protected Space reply descriptor")]
pub struct ProtectedSpaceReplyCodecError;

/// The committed bookmark mutation an event represents.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProtectedSpaceBookmarkEvent {
    operation: Uuid,
    present: bool,
}

impl ProtectedSpaceBookmarkEvent {
    pub fn operation(&self) -> Uuid {
        self.operation
    }

    pub fn present(&self) -> bool {
        self.present
    }
}

/// One managed room named by an exact protected Space content reply.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProtectedSpaceRoomDependency {
    room: BareJid,
    lifecycle: Uuid,
    incarnation: Uuid,
    generation: i64,
}

impl ProtectedSpaceRoomDependency {
    pub fn new(
        room: BareJid,
        lifecycle: Uuid,
        incarnation: Uuid,
        generation: i64,
    ) -> Result<Self, ProtectedSpaceReplyCodecError> {
        if room.node().is_none() || room.as_str().len() > MAX_JID_BYTES || generation <= 0 {
            return Err(ProtectedSpaceReplyCodecError);
        }
        let room = CanonicalBareJid::new(room)
            .map_err(|_| ProtectedSpaceReplyCodecError)?
            .into_bare_jid();
        Ok(Self {
            room,
            lifecycle,
            incarnation,
            generation,
        })
    }

    pub fn room(&self) -> &BareJid {
        &self.room
    }

    pub fn lifecycle(&self) -> Uuid {
        self.lifecycle
    }

    pub fn incarnation(&self) -> Uuid {
        self.incarnation
    }

    pub fn generation(&self) -> i64 {
        self.generation
    }
}

impl ProtectedSpaceReplyDescriptor {
    pub fn new(
        service: BareJid,
        node: String,
        recipient: FullJid,
        id: String,
        lifecycle: Uuid,
        incarnation: Uuid,
        generation: i64,
    ) -> Result<Self, ProtectedSpaceReplyCodecError> {
        let valid_length = node
            .len()
            .checked_add(id.len())
            .is_some_and(|total| total <= MAX_FRAME_SIZE);
        if service.as_str().len() > MAX_JID_BYTES
            || recipient.as_str().len() > MAX_JID_BYTES
            || recipient.node().is_none()
            || node.is_empty()
            || !valid_length
            || generation <= 0
        {
            return Err(ProtectedSpaceReplyCodecError);
        }
        Ok(Self {
            kind: ProtectedSpaceReplyKind::DiscoInfo,
            room_dependencies: None,
            subscription_id: None,
            bookmark_event: None,
            service,
            node,
            recipient,
            id,
            lifecycle,
            incarnation,
            generation,
        })
    }

    /// Bind the descriptor to the exact response shape of the request.
    pub fn with_kind(mut self, kind: ProtectedSpaceReplyKind) -> Self {
        if self.kind != kind {
            self.kind = kind;
            self.room_dependencies = None;
            self.subscription_id = None;
            self.bookmark_event = None;
        }
        self
    }

    pub fn kind(&self) -> ProtectedSpaceReplyKind {
        self.kind
    }

    /// Set the complete managed-room coverage of a content result.
    /// An empty list is explicit and can only cover an empty room list.
    pub fn with_room_dependencies(
        mut self,
        dependencies: Vec<ProtectedSpaceRoomDependency>,
    ) -> Result<Self, ProtectedSpaceReplyCodecError> {
        if !matches!(
            self.kind,
            ProtectedSpaceReplyKind::DiscoItems | ProtectedSpaceReplyKind::PubSubItems
        ) || dependencies.len() > MAX_PROTECTED_SPACE_ROOM_DEPENDENCIES
        {
            return Err(ProtectedSpaceReplyCodecError);
        }
        let mut rooms = HashSet::with_capacity(dependencies.len());
        if !dependencies
            .iter()
            .all(|dependency| rooms.insert(dependency.room()))
        {
            return Err(ProtectedSpaceReplyCodecError);
        }
        self.room_dependencies = Some(dependencies);
        Ok(self)
    }

    pub fn room_dependencies(&self) -> &[ProtectedSpaceRoomDependency] {
        self.room_dependencies.as_deref().unwrap_or_default()
    }

    pub(super) fn content_dependencies(&self) -> Option<&[ProtectedSpaceRoomDependency]> {
        self.room_dependencies.as_deref()
    }

    /// Bind a subscribe reply to the ID of its exact stored preference.
    pub fn with_subscription(
        mut self,
        subid: SubId,
    ) -> Result<Self, ProtectedSpaceReplyCodecError> {
        let id = subid.as_str();
        if id.is_empty() || id.len() > MAX_SUBSCRIPTION_ID_BYTES || id.chars().any(char::is_control)
        {
            return Err(ProtectedSpaceReplyCodecError);
        }
        self.kind = ProtectedSpaceReplyKind::Subscribe;
        self.room_dependencies = None;
        self.subscription_id = Some(subid);
        self.bookmark_event = None;
        Ok(self)
    }

    pub fn subscription_id(&self) -> Option<&SubId> {
        self.subscription_id.as_ref()
    }

    /// Bind an event to its exact subscription, mutation, and room decision.
    /// An empty base ID represents a headline that has no stanza ID.
    pub fn with_bookmark_event(
        self,
        subid: SubId,
        operation: Uuid,
        room: ProtectedSpaceRoomDependency,
        present: bool,
    ) -> Result<Self, ProtectedSpaceReplyCodecError> {
        let mut event = self.with_subscription(subid)?;
        event.kind = ProtectedSpaceReplyKind::BookmarkEvent;
        event.room_dependencies = Some(vec![room]);
        event.bookmark_event = Some(ProtectedSpaceBookmarkEvent { operation, present });
        Ok(event)
    }

    pub fn bookmark_event(&self) -> Option<&ProtectedSpaceBookmarkEvent> {
        self.bookmark_event.as_ref()
    }

    /// Identify the deletion by its canonical stanza ID.
    pub fn deletion_operation(&self) -> Option<Uuid> {
        if self.kind != ProtectedSpaceReplyKind::DeleteEvent {
            return None;
        }
        let operation = Uuid::parse_str(&self.id).ok()?;
        (operation.simple().to_string() == self.id).then_some(operation)
    }

    pub fn service(&self) -> &BareJid {
        &self.service
    }

    pub fn node(&self) -> &str {
        &self.node
    }

    pub fn recipient(&self) -> &FullJid {
        &self.recipient
    }

    pub fn id(&self) -> &str {
        &self.id
    }

    pub fn lifecycle(&self) -> Uuid {
        self.lifecycle
    }

    pub fn incarnation(&self) -> Uuid {
        self.incarnation
    }

    pub fn generation(&self) -> i64 {
        self.generation
    }

    /// Encode the versioned, length-prefixed storage representation.
    pub fn encode(&self) -> Vec<u8> {
        let fields = [
            self.service.as_str(),
            self.node(),
            self.recipient.as_str(),
            self.id(),
        ];
        let length = FIXED_BYTES + fields.iter().map(|field| field.len()).sum::<usize>();
        let mut bytes = Vec::with_capacity(length + 1);
        match self.kind {
            ProtectedSpaceReplyKind::DiscoInfo => bytes.extend_from_slice(HEADER),
            ProtectedSpaceReplyKind::DiscoItems => {
                bytes.extend_from_slice(TYPED_HEADER);
                bytes.push(1);
            }
            ProtectedSpaceReplyKind::PubSubItems => {
                bytes.extend_from_slice(TYPED_HEADER);
                bytes.push(2);
            }
            ProtectedSpaceReplyKind::Subscribe => {
                bytes.extend_from_slice(TYPED_HEADER);
                bytes.push(3);
            }
            ProtectedSpaceReplyKind::BookmarkEvent => {
                bytes.extend_from_slice(TYPED_HEADER);
                bytes.push(4);
            }
            ProtectedSpaceReplyKind::OwnerAffiliations => {
                bytes.extend_from_slice(TYPED_HEADER);
                bytes.push(5);
            }
            ProtectedSpaceReplyKind::OwnerAffiliationRefusals => {
                bytes.extend_from_slice(TYPED_HEADER);
                bytes.push(6);
            }
            ProtectedSpaceReplyKind::OwnerConfiguration => {
                bytes.extend_from_slice(TYPED_HEADER);
                bytes.push(7);
            }
            ProtectedSpaceReplyKind::DeleteEvent => {
                bytes.extend_from_slice(TYPED_HEADER);
                bytes.push(8);
            }
        }
        for field in fields {
            // The constructor limits every field below u32::MAX.
            bytes.extend_from_slice(&(field.len() as u32).to_be_bytes());
            bytes.extend_from_slice(field.as_bytes());
        }
        bytes.extend_from_slice(self.lifecycle.as_bytes());
        bytes.extend_from_slice(self.incarnation.as_bytes());
        bytes.extend_from_slice(&self.generation.to_be_bytes());
        if let Some(dependencies) = &self.room_dependencies {
            bytes.extend_from_slice(&(dependencies.len() as u16).to_be_bytes());
            for dependency in dependencies {
                let room = dependency.room.as_str().as_bytes();
                bytes.extend_from_slice(&(room.len() as u32).to_be_bytes());
                bytes.extend_from_slice(room);
                bytes.extend_from_slice(dependency.lifecycle.as_bytes());
                bytes.extend_from_slice(dependency.incarnation.as_bytes());
                bytes.extend_from_slice(&dependency.generation.to_be_bytes());
            }
        }
        if let Some(subid) = &self.subscription_id {
            let id = subid.as_str().as_bytes();
            bytes.extend_from_slice(&(id.len() as u32).to_be_bytes());
            bytes.extend_from_slice(id);
        }
        if let Some(event) = &self.bookmark_event {
            bytes.extend_from_slice(event.operation.as_bytes());
            bytes.push(u8::from(event.present));
        }
        bytes
    }

    /// Reject unknown versions, noncanonical JID spellings, and trailing data.
    pub fn decode(bytes: &[u8]) -> Result<Self, ProtectedSpaceReplyCodecError> {
        if bytes.len() > MAX_PROTECTED_SPACE_REPLY_BYTES {
            return Err(ProtectedSpaceReplyCodecError);
        }
        let (mut input, kind) = if let Some(input) = bytes.strip_prefix(HEADER) {
            (input, ProtectedSpaceReplyKind::DiscoInfo)
        } else {
            let input = bytes
                .strip_prefix(TYPED_HEADER)
                .ok_or(ProtectedSpaceReplyCodecError)?;
            match input.split_first() {
                Some((1, rest)) => (rest, ProtectedSpaceReplyKind::DiscoItems),
                Some((2, rest)) => (rest, ProtectedSpaceReplyKind::PubSubItems),
                Some((3, rest)) => (rest, ProtectedSpaceReplyKind::Subscribe),
                Some((4, rest)) => (rest, ProtectedSpaceReplyKind::BookmarkEvent),
                Some((5, rest)) => (rest, ProtectedSpaceReplyKind::OwnerAffiliations),
                Some((6, rest)) => (rest, ProtectedSpaceReplyKind::OwnerAffiliationRefusals),
                Some((7, rest)) => (rest, ProtectedSpaceReplyKind::OwnerConfiguration),
                Some((8, rest)) => (rest, ProtectedSpaceReplyKind::DeleteEvent),
                _ => return Err(ProtectedSpaceReplyCodecError),
            }
        };
        let service_text = read_string(&mut input, MAX_JID_BYTES)?;
        let node = read_string(&mut input, MAX_FRAME_SIZE)?;
        let recipient_text = read_string(&mut input, MAX_JID_BYTES)?;
        let id = read_string(&mut input, MAX_FRAME_SIZE - node.len())?;
        let service: BareJid = service_text
            .parse()
            .map_err(|_| ProtectedSpaceReplyCodecError)?;
        let recipient: FullJid = recipient_text
            .parse()
            .map_err(|_| ProtectedSpaceReplyCodecError)?;
        if service.as_str() != service_text || recipient.as_str() != recipient_text {
            return Err(ProtectedSpaceReplyCodecError);
        }
        let lifecycle = Uuid::from_bytes(read_array(&mut input)?);
        let incarnation = Uuid::from_bytes(read_array(&mut input)?);
        let generation = i64::from_be_bytes(read_array(&mut input)?);
        let mut descriptor = Self::new(
            service,
            node.into(),
            recipient,
            id.into(),
            lifecycle,
            incarnation,
            generation,
        )?
        .with_kind(kind);
        if matches!(
            kind,
            ProtectedSpaceReplyKind::DiscoItems | ProtectedSpaceReplyKind::PubSubItems
        ) {
            let count = usize::from(u16::from_be_bytes(read_array(&mut input)?));
            if count > MAX_PROTECTED_SPACE_ROOM_DEPENDENCIES {
                return Err(ProtectedSpaceReplyCodecError);
            }
            let mut dependencies = Vec::with_capacity(count);
            for _ in 0..count {
                dependencies.push(read_room_dependency(&mut input)?);
            }
            descriptor = descriptor.with_room_dependencies(dependencies)?;
        } else if kind == ProtectedSpaceReplyKind::Subscribe {
            let subid = read_string(&mut input, MAX_SUBSCRIPTION_ID_BYTES)?;
            descriptor = descriptor.with_subscription(SubId::from_raw(subid))?;
        } else if kind == ProtectedSpaceReplyKind::BookmarkEvent {
            if u16::from_be_bytes(read_array(&mut input)?) != 1 {
                return Err(ProtectedSpaceReplyCodecError);
            }
            let room = read_room_dependency(&mut input)?;
            let subid = read_string(&mut input, MAX_SUBSCRIPTION_ID_BYTES)?;
            let operation = Uuid::from_bytes(read_array(&mut input)?);
            let present = match read_array(&mut input)? {
                [0] => false,
                [1] => true,
                _ => return Err(ProtectedSpaceReplyCodecError),
            };
            descriptor =
                descriptor.with_bookmark_event(SubId::from_raw(subid), operation, room, present)?;
        }
        if !input.is_empty()
            || (kind == ProtectedSpaceReplyKind::DeleteEvent
                && descriptor.deletion_operation().is_none())
        {
            return Err(ProtectedSpaceReplyCodecError);
        }
        Ok(descriptor)
    }
}

fn read_room_dependency(
    input: &mut &[u8],
) -> Result<ProtectedSpaceRoomDependency, ProtectedSpaceReplyCodecError> {
    let room_text = read_string(input, MAX_JID_BYTES)?;
    let room: BareJid = room_text
        .parse()
        .map_err(|_| ProtectedSpaceReplyCodecError)?;
    if room.as_str() != room_text {
        return Err(ProtectedSpaceReplyCodecError);
    }
    ProtectedSpaceRoomDependency::new(
        room,
        Uuid::from_bytes(read_array(input)?),
        Uuid::from_bytes(read_array(input)?),
        i64::from_be_bytes(read_array(input)?),
    )
}

fn read_array<const N: usize>(input: &mut &[u8]) -> Result<[u8; N], ProtectedSpaceReplyCodecError> {
    let (head, tail) = input
        .split_first_chunk()
        .ok_or(ProtectedSpaceReplyCodecError)?;
    *input = tail;
    Ok(*head)
}

fn read_string<'a>(
    input: &mut &'a [u8],
    limit: usize,
) -> Result<&'a str, ProtectedSpaceReplyCodecError> {
    let len = u32::from_be_bytes(read_array(input)?) as usize;
    if len > limit {
        return Err(ProtectedSpaceReplyCodecError);
    }
    let (bytes, tail) = input
        .split_at_checked(len)
        .ok_or(ProtectedSpaceReplyCodecError)?;
    *input = tail;
    std::str::from_utf8(bytes).map_err(|_| ProtectedSpaceReplyCodecError)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn descriptor() -> ProtectedSpaceReplyDescriptor {
        ProtectedSpaceReplyDescriptor::new(
            "spaces.example.test".parse().expect("bare service JID"),
            "docs/仕様".into(),
            "alice@example.test/Firefox"
                .parse()
                .expect("full client JID"),
            "disco:invoice & preview".into(),
            Uuid::from_u128(1),
            Uuid::from_u128(2),
            42,
        )
        .expect("descriptor")
    }

    #[test]
    fn descriptor_preserves_exact_reply_identity() {
        let descriptor = descriptor();
        let encoded = descriptor.encode();
        assert_eq!(
            ProtectedSpaceReplyDescriptor::decode(&encoded),
            Ok(descriptor)
        );
    }

    #[test]
    fn descriptor_bounds_node_and_id_before_encoding() {
        let original = descriptor();
        let make = |id| {
            ProtectedSpaceReplyDescriptor::new(
                original.service.clone(),
                original.node.clone(),
                original.recipient.clone(),
                id,
                original.lifecycle,
                original.incarnation,
                original.generation,
            )
        };
        let id_bytes = MAX_FRAME_SIZE - original.node.len();
        let max = make("x".repeat(id_bytes)).expect("maximum input frame");
        assert_eq!(
            ProtectedSpaceReplyDescriptor::decode(&max.encode()),
            Ok(max)
        );
        assert!(make("x".repeat(id_bytes + 1)).is_err());
    }

    #[test]
    fn malformed_descriptors_do_not_decode_as_ordinary_replay() {
        let encoded = descriptor().encode();
        for end in 0..encoded.len() {
            assert!(ProtectedSpaceReplyDescriptor::decode(&encoded[..end]).is_err());
        }

        let mut bad = encoded.clone();
        bad[HEADER.len() - 1] += 1;
        assert!(ProtectedSpaceReplyDescriptor::decode(&bad).is_err());

        let mut bad = encoded.clone();
        bad[HEADER.len()..HEADER.len() + 4].copy_from_slice(&u32::MAX.to_be_bytes());
        assert!(ProtectedSpaceReplyDescriptor::decode(&bad).is_err());

        let mut bad = encoded.clone();
        bad[HEADER.len() + 4] = 0xff;
        assert!(ProtectedSpaceReplyDescriptor::decode(&bad).is_err());

        let mut bad = encoded.clone();
        bad[HEADER.len() + 4] = b'S';
        assert!(ProtectedSpaceReplyDescriptor::decode(&bad).is_err());

        let mut bad = encoded.clone();
        let generation_offset = bad.len() - size_of::<i64>();
        bad[generation_offset..].copy_from_slice(&0_i64.to_be_bytes());
        assert!(ProtectedSpaceReplyDescriptor::decode(&bad).is_err());

        let mut bad = encoded;
        bad.push(0);
        assert!(ProtectedSpaceReplyDescriptor::decode(&bad).is_err());
        assert!(ProtectedSpaceReplyDescriptor::decode(&vec![
            0;
            MAX_PROTECTED_SPACE_REPLY_BYTES + 1
        ])
        .is_err());
    }
}
