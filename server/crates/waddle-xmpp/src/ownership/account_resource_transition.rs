//! Inactive, bounded model of one P3.2 account-resource slot.
//!
//! This fixture assumes the supplied UserClaimIdentity was authorized by an
//! external claim owner. It does not check claim authority, persist state,
//! coordinate concurrent processes, or participate in production routing.
//! Its synchronous &mut self transitions model a serialized compare-and-set
//! history for tests only. Publication, registration, cleanup, and custody
//! receipts are caller-supplied evidence and are checked for exact identity.
//!
//! A successful resume replaces the exact transport binding while preserving
//! the resource snapshot and opaque SM session reference. A failed resume
//! followed by a fresh bind on the same transport uses a distinct BindingRef.
//! The snapshot and receipts are model data only; they do not define a
//! production storage schema or wire protocol.

use jid::FullJid;
use minidom::element::Nodes;
use minidom::Element;
use minidom::Node;
use uuid::Uuid;
use xmpp_parsers::presence::Show;

use crate::pending_delivery::{SmSessionId, SM_SESSION_ID_MAX_LEN};

use super::{BindingRef, ConnectionRef, UserClaimIdentity};

/// Maximum number of operation identities retained by one fixture slot.
pub const ACCOUNT_RESOURCE_OPERATION_CAPACITY: usize = 64;
/// Maximum number of presence extension payloads retained per snapshot.
pub const ACCOUNT_RESOURCE_MAX_PRESENCE_PAYLOADS: usize = 64;
/// Maximum serialized size of all presence extension payloads in one snapshot.
pub const ACCOUNT_RESOURCE_MAX_PRESENCE_PAYLOAD_BYTES: usize = 32 * 1024;
/// Maximum counted element headers, child nodes, attributes, and prefix
/// declarations across all presence payloads in one snapshot. Child elements
/// count once as a child node and once as their own element header.
pub const ACCOUNT_RESOURCE_MAX_PRESENCE_PAYLOAD_ITEMS: usize = 4 * 1024;
/// Maximum nesting depth accepted in one presence payload.
pub const ACCOUNT_RESOURCE_MAX_PRESENCE_PAYLOAD_DEPTH: usize = 128;
/// Maximum status text length retained in one snapshot.
pub const ACCOUNT_RESOURCE_MAX_STATUS_BYTES: usize = 4 * 1024;

#[derive(Debug, Default)]
struct PresencePayloadPreflight {
    raw_bytes: usize,
    serialized_upper_bytes: usize,
    items: usize,
    qualified_names: usize,
    longest_prefix: usize,
}

impl PresencePayloadPreflight {
    fn has_raw_capacity(&self, bytes: usize) -> bool {
        self.raw_bytes
            .checked_add(bytes)
            .is_some_and(|total| total <= ACCOUNT_RESOURCE_MAX_PRESENCE_PAYLOAD_BYTES)
    }

    fn add_raw_bytes(&mut self, bytes: usize) -> bool {
        let Some(total) = self.raw_bytes.checked_add(bytes) else {
            return false;
        };
        if total > ACCOUNT_RESOURCE_MAX_PRESENCE_PAYLOAD_BYTES {
            return false;
        }
        self.raw_bytes = total;
        true
    }

    fn add_serialized_bytes(&mut self, bytes: usize) -> bool {
        let Some(total) = self.serialized_upper_bytes.checked_add(bytes) else {
            return false;
        };
        self.serialized_upper_bytes = total;
        true
    }

    fn add_plain_field(&mut self, field: &str, copies: usize) -> bool {
        if !self.has_raw_capacity(field.len())
            || !xml_characters_are_valid(field)
            || !self.add_raw_bytes(field.len())
            || !self.add_serialized_bytes(field.len().saturating_mul(copies))
        {
            return false;
        }
        true
    }

    fn add_escaped_field(&mut self, field: &str, attribute_context: bool) -> bool {
        if !self.has_raw_capacity(field.len()) {
            return false;
        }
        let Some(escaped_bytes) = escaped_xml_bytes(field, attribute_context) else {
            return false;
        };
        self.add_raw_bytes(field.len()) && self.add_serialized_bytes(escaped_bytes)
    }

    fn add_qualified_name(&mut self) -> bool {
        let Some(total) = self.qualified_names.checked_add(1) else {
            return false;
        };
        self.qualified_names = total;
        true
    }

    fn add_markup_allowance(&mut self, item_count: usize) -> bool {
        self.add_serialized_bytes(item_count.saturating_mul(XML_ITEM_MARKUP_ALLOWANCE_BYTES))
    }

    fn serialized_upper_bound_within_limit(&self) -> bool {
        // A qualified name can repeat the longest explicit prefix, or a
        // generated prefix. Allow for both the QName and its namespace
        // declaration; `ns4095` is six bytes, so 16 safely covers generated
        // prefixes while remaining independent of input strings.
        let prefix_bytes = self.longest_prefix.max(XML_GENERATED_PREFIX_MAX_BYTES);
        let Some(repeated_prefix_bytes) = self
            .qualified_names
            .checked_mul(prefix_bytes)
            .and_then(|bytes| bytes.checked_mul(2))
        else {
            return false;
        };
        self.serialized_upper_bytes
            .checked_add(repeated_prefix_bytes)
            .is_some_and(|bytes| bytes <= ACCOUNT_RESOURCE_MAX_PRESENCE_PAYLOAD_BYTES)
    }

    fn add_item(&mut self) -> bool {
        let Some(total) = self.items.checked_add(1) else {
            return false;
        };
        if total > ACCOUNT_RESOURCE_MAX_PRESENCE_PAYLOAD_ITEMS {
            return false;
        }
        self.items = total;
        true
    }
}

const XML_ITEM_MARKUP_ALLOWANCE_BYTES: usize = 32;
const XML_GENERATED_PREFIX_MAX_BYTES: usize = 16;

fn xml_characters_are_valid(value: &str) -> bool {
    value.chars().all(|character| {
        matches!(
            character,
            '\u{9}' | '\u{a}' | '\u{d}' | '\u{20}'..='\u{d7ff}' | '\u{e000}'..='\u{fffd}' | '\u{10000}'..='\u{10ffff}'
        )
    })
}

/// Count the UTF-8 bytes produced by rxml's XML escaping without allocating.
fn escaped_xml_bytes(value: &str, attribute_context: bool) -> Option<usize> {
    if !xml_characters_are_valid(value) {
        return None;
    }

    value.as_bytes().iter().try_fold(0usize, |total, byte| {
        let escaped_len = match byte {
            b'<' | b'>' => 4,
            b'&' => 5,
            b'\'' | b'"' if attribute_context => 5,
            b'\r' => 5,
            b'\n' | b'\t' if attribute_context => 5,
            _ => 1,
        };
        total.checked_add(escaped_len)
    })
}

/// Inspect borrowed XML fields and structure without invoking minidom's
/// serializer. The conservative upper bound counts actual escaped bytes,
/// duplicated element names, repeated explicit/generated prefixes, and fixed
/// XML syntax per bounded item. It can reject a payload whose serialized form
/// would fit; the estimated upper bound for any serialized output is no more
/// than the snapshot's 32 KiB payload limit. This path is panic-free for
/// hostile namespace maps and builder-created invalid XML characters.
fn presence_payloads_within_serialized_upper_bound(payloads: &[Element]) -> bool {
    let mut preflight = PresencePayloadPreflight::default();
    let mut pending = Vec::with_capacity(payloads.len());

    for payload in payloads {
        if !preflight.add_item()
            || !preflight.add_qualified_name()
            || !preflight.add_plain_field(payload.name(), 2)
            || !preflight.add_escaped_field(payload.ns_ref(), true)
        {
            return false;
        }
        pending.push((payload, 0));
    }

    while let Some((element, depth)) = pending.pop() {
        if depth > ACCOUNT_RESOURCE_MAX_PRESENCE_PAYLOAD_DEPTH
            || !xml_ncname_is_valid(element.name())
        {
            return false;
        }

        for (prefix, namespace) in element.prefixes.declared_prefixes() {
            if !preflight.add_item() || !preflight.add_escaped_field(namespace, true) {
                return false;
            }
            if let Some(prefix) = prefix {
                if !xml_ncname_is_valid(prefix) || !preflight.add_plain_field(prefix, 1) {
                    return false;
                }
                preflight.longest_prefix = preflight.longest_prefix.max(prefix.len());
            }
        }

        for ((namespace, name), value) in element.attrs().iter() {
            if !preflight.add_item()
                || !preflight.add_qualified_name()
                || !preflight.add_escaped_field(namespace, true)
                || !preflight.add_plain_field(name, 1)
                || !preflight.add_escaped_field(value, true)
            {
                return false;
            }
        }

        for node in element.nodes() {
            if !preflight.add_item() {
                return false;
            }

            match node {
                Node::Element(child) => {
                    let child_depth = depth + 1;
                    if child_depth > ACCOUNT_RESOURCE_MAX_PRESENCE_PAYLOAD_DEPTH {
                        return false;
                    }
                    if !preflight.add_item()
                        || !preflight.add_qualified_name()
                        || !preflight.add_plain_field(child.name(), 2)
                        || !preflight.add_escaped_field(child.ns_ref(), true)
                    {
                        return false;
                    }
                    pending.push((child, child_depth));
                }
                Node::Text(text) => {
                    if !preflight.add_escaped_field(text, false) {
                        return false;
                    }
                }
            }
        }
    }

    preflight.add_markup_allowance(preflight.items)
        && preflight.serialized_upper_bound_within_limit()
}

fn xml_ncname_is_valid(value: &str) -> bool {
    <&minidom::rxml::NcNameStr>::try_from(value).is_ok()
}

struct ElementNodePair<'a> {
    left: Nodes<'a>,
    right: Nodes<'a>,
}

fn element_payloads_structurally_equal(left: &[Element], right: &[Element]) -> bool {
    if left.len() != right.len() || left.len() > ACCOUNT_RESOURCE_MAX_PRESENCE_PAYLOADS {
        return false;
    }

    let mut compared_items = 0usize;
    for (left, right) in left.iter().zip(right) {
        if !element_headers_structurally_equal(left, right, &mut compared_items) {
            return false;
        }

        // Journaled snapshots have a maximum depth of 128. A fixed stack keeps
        // conflict/replay comparison allocation-free and non-recursive; an
        // over-bound incoming tree cannot match a retained bounded snapshot.
        let mut stack: [Option<ElementNodePair<'_>>;
            ACCOUNT_RESOURCE_MAX_PRESENCE_PAYLOAD_DEPTH + 1] = std::array::from_fn(|_| None);
        stack[0] = Some(ElementNodePair {
            left: left.nodes(),
            right: right.nodes(),
        });
        let mut depth = 0usize;

        loop {
            let next = {
                let Some(frame) = stack[depth].as_mut() else {
                    return false;
                };
                (frame.left.next(), frame.right.next())
            };
            match next {
                (None, None) => {
                    stack[depth] = None;
                    if depth == 0 {
                        break;
                    }
                    depth -= 1;
                }
                (Some(Node::Text(left)), Some(Node::Text(right))) if left == right => {}
                (Some(Node::Element(left)), Some(Node::Element(right))) => {
                    let child_depth = depth + 1;
                    if child_depth > ACCOUNT_RESOURCE_MAX_PRESENCE_PAYLOAD_DEPTH
                        || !element_headers_structurally_equal(left, right, &mut compared_items)
                    {
                        return false;
                    }
                    depth = child_depth;
                    stack[depth] = Some(ElementNodePair {
                        left: left.nodes(),
                        right: right.nodes(),
                    });
                }
                _ => return false,
            }
        }
    }
    true
}

fn element_headers_structurally_equal(
    left: &Element,
    right: &Element,
    compared_items: &mut usize,
) -> bool {
    let nodes = left.nodes();
    let other_nodes = right.nodes();
    let item_count = 1usize
        .checked_add(left.attrs().len())
        .and_then(|count| count.checked_add(left.prefixes.declared_prefixes().len()))
        .and_then(|count| count.checked_add(nodes.len()));
    let Some(item_count) = item_count else {
        return false;
    };
    let Some(total) = compared_items.checked_add(item_count) else {
        return false;
    };
    if total > ACCOUNT_RESOURCE_MAX_PRESENCE_PAYLOAD_ITEMS {
        return false;
    }
    *compared_items = total;

    left.name() == right.name()
        && left.ns_ref() == right.ns_ref()
        && left.prefixes.declared_prefixes() == right.prefixes.declared_prefixes()
        && left.attrs().iter().eq(right.attrs().iter())
        && nodes.len() == other_nodes.len()
}

/// Drop an owned XML tree without invoking Element's recursive derived Drop.
/// Frames contain owned child iterators, so the auxiliary stack grows with
/// nesting depth rather than with the number of siblings.
fn discard_element_tree(mut root: Element) {
    let mut pending = vec![root.take_nodes().into_iter()];
    drop(root);

    while let Some(frame) = pending.last_mut() {
        match frame.next() {
            Some(Node::Element(mut child)) => {
                pending.push(child.take_nodes().into_iter());
                drop(child);
            }
            Some(Node::Text(text)) => drop(text),
            None => {
                pending.pop();
            }
        }
    }
}

fn discard_unretained_operation(mut operation: AccountResourceOperation) {
    if let AccountResourceMutation::UpdateSnapshot { snapshot, .. } = &mut operation.mutation {
        for payload in snapshot.presence_payloads.drain(..) {
            discard_element_tree(payload);
        }
    }
}

/// Monotonic revision of this single full-JID resource slot.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct AccountResourceRevision(u64);

impl AccountResourceRevision {
    pub const INITIAL: Self = Self(0);

    pub const fn value(self) -> u64 {
        self.0
    }

    fn next(self) -> Option<Self> {
        self.0.checked_add(1).map(Self)
    }
}

/// Idempotency key for one resource-slot mutation attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct AccountResourceOperationId(Uuid);

impl AccountResourceOperationId {
    pub fn fresh() -> Self {
        Self(Uuid::new_v4())
    }
}

/// One coherent resource snapshot. The typed SM session id is an opaque
/// session reference carried with the exact binding evidence below; it is not
/// a durable foreign key and does not itself authorize any SM operation.
#[derive(Debug, Clone, Default)]
pub struct AccountResourceSnapshot {
    pub presence_available: bool,
    pub presence_show: Option<Show>,
    pub presence_status: Option<String>,
    pub presence_priority: i8,
    pub presence_payloads: Vec<Element>,
    pub carbons_enabled: bool,
    pub roster_interested: bool,
    pub blocklist_interested: bool,
    pub stream_management_ref: Option<SmSessionId>,
}

impl PartialEq for AccountResourceSnapshot {
    fn eq(&self, other: &Self) -> bool {
        self.presence_available == other.presence_available
            && self.presence_show == other.presence_show
            && self.presence_status == other.presence_status
            && self.presence_priority == other.presence_priority
            && element_payloads_structurally_equal(
                &self.presence_payloads,
                &other.presence_payloads,
            )
            && self.carbons_enabled == other.carbons_enabled
            && self.roster_interested == other.roster_interested
            && self.blocklist_interested == other.blocklist_interested
            && self.stream_management_ref == other.stream_management_ref
    }
}

impl AccountResourceSnapshot {
    fn within_bounds(&self) -> bool {
        if self
            .presence_status
            .as_ref()
            .is_some_and(|status| status.len() > ACCOUNT_RESOURCE_MAX_STATUS_BYTES)
            || self.presence_payloads.len() > ACCOUNT_RESOURCE_MAX_PRESENCE_PAYLOADS
            || self
                .stream_management_ref
                .as_ref()
                .is_some_and(|stream| stream.as_str().len() > SM_SESSION_ID_MAX_LEN)
        {
            return false;
        }

        presence_payloads_within_serialized_upper_bound(&self.presence_payloads)
    }
}

/// Lifecycle states for the one current physical binding of a full-JID resource.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AccountResourceBindingState {
    Prepared,
    Published,
    Active,
    Retiring,
    Retired,
}

/// Identity evidence for one endpoint, actor registration, or cleanup effect.
/// The connection is included explicitly and must agree with the BindingRef.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BindingEvidence {
    binding_ref: BindingRef,
    connection_ref: ConnectionRef,
}

impl BindingEvidence {
    pub fn observed(binding_ref: BindingRef, connection_ref: ConnectionRef) -> Self {
        Self {
            binding_ref,
            connection_ref,
        }
    }

    pub fn for_binding(binding_ref: &BindingRef) -> Self {
        Self::observed(binding_ref.clone(), binding_ref.connection_ref().clone())
    }

    pub fn binding_ref(&self) -> &BindingRef {
        &self.binding_ref
    }

    pub fn connection_ref(&self) -> &ConnectionRef {
        &self.connection_ref
    }
}

/// Evidence that one exact BindingRef was published into the endpoint index.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EndpointPublicationReceipt {
    source: BindingEvidence,
}

impl EndpointPublicationReceipt {
    pub fn observed(source: BindingEvidence) -> Self {
        Self { source }
    }

    pub fn source(&self) -> &BindingEvidence {
        &self.source
    }
}

/// Evidence that one exact binding was registered to one exact account actor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActorRegistrationReceipt {
    claim_identity: UserClaimIdentity,
    source: BindingEvidence,
}

/// Current authorization to finish an XMPP bind response for the exact
/// active resource. Callers must obtain this from the slot immediately before
/// emitting success; a previously observed actor-registration reply can be
/// delayed past retirement or replacement.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccountResourceBindSuccessReceipt {
    claim_identity: UserClaimIdentity,
    source: BindingEvidence,
    revision: AccountResourceRevision,
    publication: EndpointPublicationReceipt,
    registration: ActorRegistrationReceipt,
}

impl AccountResourceBindSuccessReceipt {
    pub fn claim_identity(&self) -> &UserClaimIdentity {
        &self.claim_identity
    }

    pub fn source(&self) -> &BindingEvidence {
        &self.source
    }

    pub fn revision(&self) -> AccountResourceRevision {
        self.revision
    }

    pub fn publication(&self) -> &EndpointPublicationReceipt {
        &self.publication
    }

    pub fn registration(&self) -> &ActorRegistrationReceipt {
        &self.registration
    }
}

impl ActorRegistrationReceipt {
    pub fn observed(claim_identity: UserClaimIdentity, source: BindingEvidence) -> Self {
        Self {
            claim_identity,
            source,
        }
    }

    pub fn claim_identity(&self) -> &UserClaimIdentity {
        &self.claim_identity
    }

    pub fn source(&self) -> &BindingEvidence {
        &self.source
    }
}

/// Evidence that XEP-0198 resume committed on a new physical transport while
/// preserving the same resource-level SM session reference and accepted-work
/// custody. It is a model receipt, not a live resume result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SuccessfulResumeReceipt {
    previous: BindingEvidence,
    resumed: BindingEvidence,
    stream_management_ref: SmSessionId,
    custody_disposition: BindingRetirementDisposition,
}

impl SuccessfulResumeReceipt {
    pub fn observed(
        previous: BindingEvidence,
        resumed: BindingEvidence,
        stream_management_ref: SmSessionId,
        custody_disposition: BindingRetirementDisposition,
    ) -> Self {
        Self {
            previous,
            resumed,
            stream_management_ref,
            custody_disposition,
        }
    }

    pub fn previous(&self) -> &BindingEvidence {
        &self.previous
    }

    pub fn resumed(&self) -> &BindingEvidence {
        &self.resumed
    }

    pub fn stream_management_ref(&self) -> &SmSessionId {
        &self.stream_management_ref
    }

    pub fn custody_disposition(&self) -> BindingRetirementDisposition {
        self.custody_disposition
    }
}

/// How endpoint publication was removed or proven absent during retirement.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EndpointRetirementEvidence {
    NeverPublished {
        source: BindingEvidence,
    },
    Unpublished {
        publication: EndpointPublicationReceipt,
        removed: BindingEvidence,
    },
}

/// How actor registration was removed or proven absent during retirement.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ActorUnregistrationEvidence {
    NeverRegistered {
        claim_identity: UserClaimIdentity,
        source: BindingEvidence,
    },
    Unregistered {
        registration: ActorRegistrationReceipt,
        removed: BindingEvidence,
    },
}

/// Result of retracting the directed-recipient set for one exact resource.
/// The delivered/recovery-owned forms mean the recipient set was removed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DirectedPresenceTeardownDisposition {
    NoDirectedRecipients,
    RecipientsRetractedAndUnavailableDelivered,
    RecipientsRetractedAndUnavailableRecoveryOwned,
    Unknown,
}

/// Exact directed-presence teardown evidence for one resource snapshot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirectedPresenceTeardownReceipt {
    source: BindingEvidence,
    snapshot_revision: AccountResourceRevision,
    disposition: DirectedPresenceTeardownDisposition,
}

impl DirectedPresenceTeardownReceipt {
    pub fn observed(
        source: BindingEvidence,
        snapshot_revision: AccountResourceRevision,
        disposition: DirectedPresenceTeardownDisposition,
    ) -> Self {
        Self {
            source,
            snapshot_revision,
            disposition,
        }
    }
}

/// P1 presentation/flush outcome for the exact resource snapshot being retired.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PresentationFlushDisposition {
    NoPendingPresentation,
    Delivered,
    RecoveryOwner,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PresentationFlushReceipt {
    source: BindingEvidence,
    snapshot_revision: AccountResourceRevision,
    disposition: PresentationFlushDisposition,
}

impl PresentationFlushReceipt {
    pub fn observed(
        source: BindingEvidence,
        snapshot_revision: AccountResourceRevision,
        disposition: PresentationFlushDisposition,
    ) -> Self {
        Self {
            source,
            snapshot_revision,
            disposition,
        }
    }
}

/// Exact external-effect and custody evidence required to finish retirement.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccountResourceRetirementEvidence {
    pub endpoint: EndpointRetirementEvidence,
    pub actor: ActorUnregistrationEvidence,
    pub directed_presence: DirectedPresenceTeardownReceipt,
    pub presentation_flush: PresentationFlushReceipt,
    pub custody_disposition: BindingRetirementDisposition,
}

/// Caller-supplied disposition of accepted work when retiring a binding. This
/// fixture cannot prove the disposition; Unknown fails closed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BindingRetirementDisposition {
    NoAcceptedWork,
    AcceptedWorkHasRecoveryOwner,
    Unknown,
}

/// Evidence captured when retirement begins, before cleanup completes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccountResourceRetirementStart {
    initiated_by: UserClaimIdentity,
    source: BindingEvidence,
    starting_state: AccountResourceBindingState,
    source_revision: AccountResourceRevision,
    snapshot_revision: AccountResourceRevision,
    custody_disposition: BindingRetirementDisposition,
}

impl AccountResourceRetirementStart {
    pub fn initiated_by(&self) -> &UserClaimIdentity {
        &self.initiated_by
    }

    pub fn source(&self) -> &BindingEvidence {
        &self.source
    }

    pub fn starting_state(&self) -> AccountResourceBindingState {
        self.starting_state
    }

    pub fn source_revision(&self) -> AccountResourceRevision {
        self.source_revision
    }

    pub fn snapshot_revision(&self) -> AccountResourceRevision {
        self.snapshot_revision
    }

    pub fn custody_disposition(&self) -> BindingRetirementDisposition {
        self.custody_disposition
    }
}

/// Retained lifecycle receipt for one exact binding. Its snapshot remains
/// intact through successful resume preparation and until retirement settles.
#[derive(Debug, Clone, PartialEq)]
pub struct AccountResourceBindingReceipt {
    source: BindingEvidence,
    binding_claim_identity: UserClaimIdentity,
    state: AccountResourceBindingState,
    snapshot: AccountResourceSnapshot,
    snapshot_revision: AccountResourceRevision,
    publication: Option<EndpointPublicationReceipt>,
    registration: Option<ActorRegistrationReceipt>,
    resumed_from: Option<SuccessfulResumeReceipt>,
    retirement_start: Option<AccountResourceRetirementStart>,
    retirement: Option<AccountResourceRetirementEvidence>,
}

impl AccountResourceBindingReceipt {
    pub fn source(&self) -> &BindingEvidence {
        &self.source
    }

    pub fn binding_ref(&self) -> &BindingRef {
        self.source.binding_ref()
    }

    pub fn connection_ref(&self) -> &ConnectionRef {
        self.source.connection_ref()
    }

    pub fn binding_claim_identity(&self) -> &UserClaimIdentity {
        &self.binding_claim_identity
    }

    pub fn state(&self) -> AccountResourceBindingState {
        self.state
    }

    pub fn snapshot(&self) -> &AccountResourceSnapshot {
        &self.snapshot
    }

    pub fn snapshot_revision(&self) -> AccountResourceRevision {
        self.snapshot_revision
    }

    pub fn publication(&self) -> Option<&EndpointPublicationReceipt> {
        self.publication.as_ref()
    }

    pub fn registration(&self) -> Option<&ActorRegistrationReceipt> {
        self.registration.as_ref()
    }

    pub fn resumed_from(&self) -> Option<&SuccessfulResumeReceipt> {
        self.resumed_from.as_ref()
    }

    pub fn retirement_start(&self) -> Option<&AccountResourceRetirementStart> {
        self.retirement_start.as_ref()
    }

    pub fn retirement(&self) -> Option<&AccountResourceRetirementEvidence> {
        self.retirement.as_ref()
    }
}

/// Compare-and-set request for the one resource slot.
#[derive(Debug, Clone, PartialEq)]
pub enum AccountResourceMutation {
    Bind {
        claim_identity: UserClaimIdentity,
        expected_revision: AccountResourceRevision,
        binding_ref: BindingRef,
    },
    PublishEndpoint {
        claim_identity: UserClaimIdentity,
        expected_revision: AccountResourceRevision,
        binding_ref: BindingRef,
        publication: EndpointPublicationReceipt,
    },
    RegisterActor {
        claim_identity: UserClaimIdentity,
        expected_revision: AccountResourceRevision,
        binding_ref: BindingRef,
        registration: ActorRegistrationReceipt,
    },
    UpdateSnapshot {
        claim_identity: UserClaimIdentity,
        expected_revision: AccountResourceRevision,
        binding_ref: BindingRef,
        snapshot: AccountResourceSnapshot,
    },
    BeginRetirement {
        claim_identity: UserClaimIdentity,
        expected_revision: AccountResourceRevision,
        binding_ref: BindingRef,
        custody_disposition: BindingRetirementDisposition,
    },
    CompleteRetirement {
        claim_identity: UserClaimIdentity,
        expected_revision: AccountResourceRevision,
        binding_ref: BindingRef,
        evidence: Box<AccountResourceRetirementEvidence>,
    },
    ReplaceClaim {
        claim_identity: UserClaimIdentity,
        expected_revision: AccountResourceRevision,
        binding_ref: BindingRef,
        replacement_claim_identity: UserClaimIdentity,
    },
    AdoptBinding {
        claim_identity: UserClaimIdentity,
        expected_revision: AccountResourceRevision,
        binding_ref: BindingRef,
        expected_binding_claim_identity: UserClaimIdentity,
        actor_registration: Option<ActorRegistrationReceipt>,
    },
    SuccessfulResume {
        claim_identity: UserClaimIdentity,
        expected_revision: AccountResourceRevision,
        previous_binding_ref: BindingRef,
        binding_ref: BindingRef,
        receipt: SuccessfulResumeReceipt,
    },
}

/// One identified mutation and its replay key.
#[derive(Debug, Clone, PartialEq)]
pub struct AccountResourceOperation {
    pub id: AccountResourceOperationId,
    pub mutation: AccountResourceMutation,
}

impl AccountResourceOperation {
    pub fn new(id: AccountResourceOperationId, mutation: AccountResourceMutation) -> Self {
        Self { id, mutation }
    }
}

/// A terminal result retained in the bounded operation journal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AccountResourceOperationOutcome {
    Committed { revision: AccountResourceRevision },
    Rejected(AccountResourceTransitionError),
}

/// Whether an execution applied now or replayed an earlier terminal result.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AccountResourceExecution {
    Applied(AccountResourceOperationOutcome),
    Replayed(AccountResourceOperationOutcome),
}

/// Errors outside the state-machine transition, which are not journaled.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AccountResourceExecuteError {
    OperationIdConflict,
    JournalFull,
    SnapshotTooLarge,
}

/// Terminal rejection recorded for an operation ID and payload.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AccountResourceTransitionError {
    AccountMismatch,
    ClaimMismatch,
    ClaimIdentityReused,
    RevisionConflict,
    RevisionExhausted,
    AlreadyBound,
    NotBound,
    BindingMismatch,
    BindingOwnerMismatch,
    BindingRefReused,
    LifecycleConflict,
    EndpointPublicationMismatch,
    ActorRegistrationMismatch,
    StreamManagementReferenceChanged,
    ResumeRequiresStreamManagement,
    ResumeBindingMismatch,
    ResumeConnectionMismatch,
    RetirementCustodyUnknown,
    RetirementEvidenceMismatch,
    RetirementCustodyMismatch,
}

/// Result of looking up an operation after an unknown response.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AccountResourceOperationLookup<'a> {
    Unknown,
    PayloadMismatch,
    Recorded(&'a AccountResourceOperationOutcome),
}

#[derive(Debug, Clone, PartialEq)]
struct JournalEntry {
    id: AccountResourceOperationId,
    mutation: AccountResourceMutation,
    outcome: AccountResourceOperationOutcome,
}

/// One serialized test fixture for a single exact full JID.
#[derive(Debug)]
pub struct AccountResourceSlot {
    full_jid: FullJid,
    claim_identity: UserClaimIdentity,
    revision: AccountResourceRevision,
    binding: Option<AccountResourceBindingReceipt>,
    retired_bindings: Vec<AccountResourceBindingReceipt>,
    successful_resumes: Vec<SuccessfulResumeReceipt>,
    retired_claim_identities: Vec<UserClaimIdentity>,
    seen_binding_refs: Vec<BindingRef>,
    journal: Vec<JournalEntry>,
}

impl AccountResourceSlot {
    /// Start a slot with the externally authorized claim identity supplied by
    /// the fixture caller. The identity's account must match the full JID.
    pub fn new(
        full_jid: FullJid,
        claim_identity: UserClaimIdentity,
    ) -> Result<Self, AccountResourceTransitionError> {
        if claim_identity.bare_jid() != &full_jid.to_bare() {
            return Err(AccountResourceTransitionError::AccountMismatch);
        }

        Ok(Self {
            full_jid,
            claim_identity,
            revision: AccountResourceRevision::INITIAL,
            binding: None,
            retired_bindings: Vec::with_capacity(ACCOUNT_RESOURCE_OPERATION_CAPACITY),
            successful_resumes: Vec::with_capacity(ACCOUNT_RESOURCE_OPERATION_CAPACITY),
            retired_claim_identities: Vec::with_capacity(ACCOUNT_RESOURCE_OPERATION_CAPACITY),
            seen_binding_refs: Vec::with_capacity(ACCOUNT_RESOURCE_OPERATION_CAPACITY),
            journal: Vec::with_capacity(ACCOUNT_RESOURCE_OPERATION_CAPACITY),
        })
    }

    pub fn full_jid(&self) -> &FullJid {
        &self.full_jid
    }

    pub fn claim_identity(&self) -> &UserClaimIdentity {
        &self.claim_identity
    }

    pub fn revision(&self) -> AccountResourceRevision {
        self.revision
    }

    pub fn binding_ref(&self) -> Option<&BindingRef> {
        self.binding
            .as_ref()
            .map(AccountResourceBindingReceipt::binding_ref)
    }

    pub fn binding_receipt(&self) -> Option<&AccountResourceBindingReceipt> {
        self.binding.as_ref()
    }

    /// Re-check exact current activation before completing a delayed bind
    /// response. A receipt observed before retirement or replacement must not
    /// be treated as current authorization afterward.
    pub fn current_bind_success_receipt(
        &self,
        claim_identity: &UserClaimIdentity,
        binding_ref: &BindingRef,
    ) -> Option<AccountResourceBindSuccessReceipt> {
        if &self.claim_identity != claim_identity {
            return None;
        }

        let binding = self.binding.as_ref()?;
        if binding.binding_ref() != binding_ref
            || &binding.binding_claim_identity != claim_identity
            || binding.state != AccountResourceBindingState::Active
        {
            return None;
        }

        let publication = binding.publication.as_ref()?;
        let registration = binding.registration.as_ref()?;
        if publication.source() != binding.source()
            || registration.claim_identity() != claim_identity
            || registration.source() != binding.source()
        {
            return None;
        }

        Some(AccountResourceBindSuccessReceipt {
            claim_identity: claim_identity.clone(),
            source: binding.source.clone(),
            revision: self.revision,
            publication: publication.clone(),
            registration: registration.clone(),
        })
    }

    pub fn snapshot(&self) -> Option<&AccountResourceSnapshot> {
        self.binding
            .as_ref()
            .map(AccountResourceBindingReceipt::snapshot)
    }

    pub fn retired_binding_receipts(&self) -> &[AccountResourceBindingReceipt] {
        &self.retired_bindings
    }

    pub fn successful_resume_receipts(&self) -> &[SuccessfulResumeReceipt] {
        &self.successful_resumes
    }

    /// Apply one operation serially. Terminal transition results are journaled
    /// with structural payload equality; conflicting reuse and oversized
    /// snapshots fail before the slot or journal can change.
    pub fn execute(
        &mut self,
        operation: AccountResourceOperation,
    ) -> Result<AccountResourceExecution, AccountResourceExecuteError> {
        if let Some((matches_payload, outcome)) = self
            .journal
            .iter()
            .find(|entry| entry.id == operation.id)
            .map(|entry| (entry.mutation == operation.mutation, entry.outcome))
        {
            if matches_payload {
                return Ok(AccountResourceExecution::Replayed(outcome));
            }
            discard_unretained_operation(operation);
            return Err(AccountResourceExecuteError::OperationIdConflict);
        }

        if self.journal.len() == ACCOUNT_RESOURCE_OPERATION_CAPACITY {
            discard_unretained_operation(operation);
            return Err(AccountResourceExecuteError::JournalFull);
        }

        if let AccountResourceMutation::UpdateSnapshot { snapshot, .. } = &operation.mutation {
            if !snapshot.within_bounds() {
                discard_unretained_operation(operation);
                return Err(AccountResourceExecuteError::SnapshotTooLarge);
            }
        }

        let outcome = self.apply(&operation.mutation);
        self.journal.push(JournalEntry {
            id: operation.id,
            mutation: operation.mutation,
            outcome,
        });
        Ok(AccountResourceExecution::Applied(outcome))
    }

    /// Resolve a possibly lost operation response from the retained journal.
    /// The complete operation must match. Unknown means this slot has no
    /// record for the ID; PayloadMismatch means the ID is already bound to
    /// another mutation. Neither proves an unavailable external store rolled
    /// back.
    pub fn lookup(
        &self,
        operation: &AccountResourceOperation,
    ) -> AccountResourceOperationLookup<'_> {
        self.journal
            .iter()
            .find(|entry| entry.id == operation.id)
            .map_or(AccountResourceOperationLookup::Unknown, |entry| {
                if entry.mutation == operation.mutation {
                    AccountResourceOperationLookup::Recorded(&entry.outcome)
                } else {
                    AccountResourceOperationLookup::PayloadMismatch
                }
            })
    }

    fn apply(&mut self, mutation: &AccountResourceMutation) -> AccountResourceOperationOutcome {
        let result = match mutation {
            AccountResourceMutation::Bind {
                claim_identity,
                expected_revision,
                binding_ref,
            } => self.bind(claim_identity, *expected_revision, binding_ref),
            AccountResourceMutation::PublishEndpoint {
                claim_identity,
                expected_revision,
                binding_ref,
                publication,
            } => {
                self.publish_endpoint(claim_identity, *expected_revision, binding_ref, publication)
            }
            AccountResourceMutation::RegisterActor {
                claim_identity,
                expected_revision,
                binding_ref,
                registration,
            } => self.register_actor(
                claim_identity,
                *expected_revision,
                binding_ref,
                registration,
            ),
            AccountResourceMutation::UpdateSnapshot {
                claim_identity,
                expected_revision,
                binding_ref,
                snapshot,
            } => self.update_snapshot(
                claim_identity,
                *expected_revision,
                binding_ref,
                snapshot.clone(),
            ),
            AccountResourceMutation::BeginRetirement {
                claim_identity,
                expected_revision,
                binding_ref,
                custody_disposition,
            } => self.begin_retirement(
                claim_identity,
                *expected_revision,
                binding_ref,
                *custody_disposition,
            ),
            AccountResourceMutation::CompleteRetirement {
                claim_identity,
                expected_revision,
                binding_ref,
                evidence,
            } => {
                self.complete_retirement(claim_identity, *expected_revision, binding_ref, evidence)
            }
            AccountResourceMutation::ReplaceClaim {
                claim_identity,
                expected_revision,
                binding_ref,
                replacement_claim_identity,
            } => self.replace_claim(
                claim_identity,
                *expected_revision,
                binding_ref,
                replacement_claim_identity,
            ),
            AccountResourceMutation::AdoptBinding {
                claim_identity,
                expected_revision,
                binding_ref,
                expected_binding_claim_identity,
                actor_registration,
            } => self.adopt_binding(
                claim_identity,
                *expected_revision,
                binding_ref,
                expected_binding_claim_identity,
                actor_registration.as_ref(),
            ),
            AccountResourceMutation::SuccessfulResume {
                claim_identity,
                expected_revision,
                previous_binding_ref,
                binding_ref,
                receipt,
            } => self.successful_resume(
                claim_identity,
                *expected_revision,
                previous_binding_ref,
                binding_ref,
                receipt,
            ),
        };

        match result {
            Ok(revision) => AccountResourceOperationOutcome::Committed { revision },
            Err(error) => AccountResourceOperationOutcome::Rejected(error),
        }
    }

    fn bind(
        &mut self,
        claim_identity: &UserClaimIdentity,
        expected_revision: AccountResourceRevision,
        binding_ref: &BindingRef,
    ) -> Result<AccountResourceRevision, AccountResourceTransitionError> {
        self.check_actor(claim_identity)?;
        self.check_revision(expected_revision)?;
        if self.binding.is_some() {
            return Err(AccountResourceTransitionError::AlreadyBound);
        }
        if self.seen_binding_refs.contains(binding_ref) {
            return Err(AccountResourceTransitionError::BindingRefReused);
        }
        let revision = self.next_revision()?;

        self.binding = Some(AccountResourceBindingReceipt {
            source: BindingEvidence::for_binding(binding_ref),
            binding_claim_identity: claim_identity.clone(),
            state: AccountResourceBindingState::Prepared,
            snapshot: AccountResourceSnapshot::default(),
            snapshot_revision: revision,
            publication: None,
            registration: None,
            resumed_from: None,
            retirement_start: None,
            retirement: None,
        });
        self.seen_binding_refs.push(binding_ref.clone());
        self.revision = revision;
        Ok(revision)
    }

    fn publish_endpoint(
        &mut self,
        claim_identity: &UserClaimIdentity,
        expected_revision: AccountResourceRevision,
        binding_ref: &BindingRef,
        publication: &EndpointPublicationReceipt,
    ) -> Result<AccountResourceRevision, AccountResourceTransitionError> {
        self.check_actor(claim_identity)?;
        self.check_revision(expected_revision)?;
        self.check_binding(binding_ref)?;
        {
            let binding = self.current_binding()?;
            if &binding.binding_claim_identity != claim_identity {
                return Err(AccountResourceTransitionError::BindingOwnerMismatch);
            }
            if binding.state != AccountResourceBindingState::Prepared {
                return Err(AccountResourceTransitionError::LifecycleConflict);
            }
            if publication.source != binding.source {
                return Err(AccountResourceTransitionError::EndpointPublicationMismatch);
            }
        }
        let revision = self.next_revision()?;
        let binding = self.current_binding_mut()?;

        binding.publication = Some(publication.clone());
        binding.state = AccountResourceBindingState::Published;
        self.revision = revision;
        Ok(revision)
    }

    fn register_actor(
        &mut self,
        claim_identity: &UserClaimIdentity,
        expected_revision: AccountResourceRevision,
        binding_ref: &BindingRef,
        registration: &ActorRegistrationReceipt,
    ) -> Result<AccountResourceRevision, AccountResourceTransitionError> {
        self.check_actor(claim_identity)?;
        self.check_revision(expected_revision)?;
        self.check_binding(binding_ref)?;
        {
            let binding = self.current_binding()?;
            if &binding.binding_claim_identity != claim_identity {
                return Err(AccountResourceTransitionError::BindingOwnerMismatch);
            }
            if binding.state != AccountResourceBindingState::Published {
                return Err(AccountResourceTransitionError::LifecycleConflict);
            }
            if registration.claim_identity != *claim_identity
                || registration.source != binding.source
            {
                return Err(AccountResourceTransitionError::ActorRegistrationMismatch);
            }
        }
        let revision = self.next_revision()?;
        let binding = self.current_binding_mut()?;

        binding.registration = Some(registration.clone());
        binding.state = AccountResourceBindingState::Active;
        self.revision = revision;
        Ok(revision)
    }

    fn update_snapshot(
        &mut self,
        claim_identity: &UserClaimIdentity,
        expected_revision: AccountResourceRevision,
        binding_ref: &BindingRef,
        snapshot: AccountResourceSnapshot,
    ) -> Result<AccountResourceRevision, AccountResourceTransitionError> {
        self.check_actor(claim_identity)?;
        self.check_revision(expected_revision)?;
        self.check_binding(binding_ref)?;
        {
            let binding = self.current_binding()?;
            if &binding.binding_claim_identity != claim_identity {
                return Err(AccountResourceTransitionError::BindingOwnerMismatch);
            }
            if binding.state != AccountResourceBindingState::Active {
                return Err(AccountResourceTransitionError::LifecycleConflict);
            }
            if binding.snapshot.stream_management_ref.is_some()
                && snapshot.stream_management_ref != binding.snapshot.stream_management_ref
            {
                return Err(AccountResourceTransitionError::StreamManagementReferenceChanged);
            }
        }
        let revision = self.next_revision()?;
        let binding = self.current_binding_mut()?;

        binding.snapshot = snapshot;
        binding.snapshot_revision = revision;
        self.revision = revision;
        Ok(revision)
    }

    fn begin_retirement(
        &mut self,
        claim_identity: &UserClaimIdentity,
        expected_revision: AccountResourceRevision,
        binding_ref: &BindingRef,
        custody_disposition: BindingRetirementDisposition,
    ) -> Result<AccountResourceRevision, AccountResourceTransitionError> {
        self.check_actor(claim_identity)?;
        self.check_revision(expected_revision)?;
        self.check_binding(binding_ref)?;
        if custody_disposition == BindingRetirementDisposition::Unknown {
            return Err(AccountResourceTransitionError::RetirementCustodyUnknown);
        }

        let (source, starting_state, snapshot_revision) = {
            let binding = self.current_binding()?;
            if !matches!(
                binding.state,
                AccountResourceBindingState::Prepared
                    | AccountResourceBindingState::Published
                    | AccountResourceBindingState::Active
            ) {
                return Err(AccountResourceTransitionError::LifecycleConflict);
            }
            (
                binding.source.clone(),
                binding.state,
                binding.snapshot_revision,
            )
        };
        let source_revision = self.revision;
        let revision = self.next_revision()?;

        let binding = self.current_binding_mut()?;
        if binding.state != starting_state {
            return Err(AccountResourceTransitionError::LifecycleConflict);
        }
        binding.retirement_start = Some(AccountResourceRetirementStart {
            initiated_by: claim_identity.clone(),
            source,
            starting_state,
            source_revision,
            snapshot_revision,
            custody_disposition,
        });
        binding.state = AccountResourceBindingState::Retiring;
        self.revision = revision;
        Ok(revision)
    }

    fn complete_retirement(
        &mut self,
        claim_identity: &UserClaimIdentity,
        expected_revision: AccountResourceRevision,
        binding_ref: &BindingRef,
        evidence: &AccountResourceRetirementEvidence,
    ) -> Result<AccountResourceRevision, AccountResourceTransitionError> {
        self.check_actor(claim_identity)?;
        self.check_revision(expected_revision)?;
        self.check_binding(binding_ref)?;
        let binding = self.current_binding()?;
        if binding.state != AccountResourceBindingState::Retiring {
            return Err(AccountResourceTransitionError::LifecycleConflict);
        }
        let start = binding
            .retirement_start
            .as_ref()
            .ok_or(AccountResourceTransitionError::RetirementEvidenceMismatch)?;
        self.validate_retirement_evidence(binding, start, evidence)?;
        let revision = self.next_revision()?;

        let mut retired = self
            .binding
            .take()
            .ok_or(AccountResourceTransitionError::NotBound)?;
        retired.state = AccountResourceBindingState::Retired;
        retired.retirement = Some(evidence.clone());
        self.retired_bindings.push(retired);
        self.revision = revision;
        Ok(revision)
    }

    fn validate_retirement_evidence(
        &self,
        binding: &AccountResourceBindingReceipt,
        start: &AccountResourceRetirementStart,
        evidence: &AccountResourceRetirementEvidence,
    ) -> Result<(), AccountResourceTransitionError> {
        if start.source != binding.source
            || evidence.custody_disposition != start.custody_disposition
            || evidence.directed_presence.source != binding.source
            || evidence.directed_presence.snapshot_revision != start.snapshot_revision
            || evidence.presentation_flush.source != binding.source
            || evidence.presentation_flush.snapshot_revision != start.snapshot_revision
            || evidence.directed_presence.disposition
                == DirectedPresenceTeardownDisposition::Unknown
            || evidence.presentation_flush.disposition == PresentationFlushDisposition::Unknown
        {
            return Err(AccountResourceTransitionError::RetirementEvidenceMismatch);
        }

        let endpoint_matches = match (&binding.publication, &evidence.endpoint) {
            (None, EndpointRetirementEvidence::NeverPublished { source }) => {
                source == &binding.source
            }
            (
                Some(publication),
                EndpointRetirementEvidence::Unpublished {
                    publication: observed_publication,
                    removed,
                },
            ) => observed_publication == publication && removed == &binding.source,
            _ => false,
        };
        let actor_matches = match (&binding.registration, &evidence.actor) {
            (
                None,
                ActorUnregistrationEvidence::NeverRegistered {
                    claim_identity,
                    source,
                },
            ) => claim_identity == &binding.binding_claim_identity && source == &binding.source,
            (
                Some(registration),
                ActorUnregistrationEvidence::Unregistered {
                    registration: observed_registration,
                    removed,
                },
            ) => observed_registration == registration && removed == &binding.source,
            _ => false,
        };
        if !endpoint_matches || !actor_matches {
            return Err(AccountResourceTransitionError::RetirementEvidenceMismatch);
        }

        let recovery_owned =
            evidence.presentation_flush.disposition == PresentationFlushDisposition::RecoveryOwner
                || evidence.directed_presence.disposition
                    == DirectedPresenceTeardownDisposition::RecipientsRetractedAndUnavailableRecoveryOwned;
        if recovery_owned
            && evidence.custody_disposition
                != BindingRetirementDisposition::AcceptedWorkHasRecoveryOwner
        {
            return Err(AccountResourceTransitionError::RetirementCustodyMismatch);
        }

        Ok(())
    }

    fn replace_claim(
        &mut self,
        claim_identity: &UserClaimIdentity,
        expected_revision: AccountResourceRevision,
        binding_ref: &BindingRef,
        replacement_claim_identity: &UserClaimIdentity,
    ) -> Result<AccountResourceRevision, AccountResourceTransitionError> {
        self.check_actor(claim_identity)?;
        self.check_revision(expected_revision)?;
        self.check_binding(binding_ref)?;
        if self
            .binding
            .as_ref()
            .is_some_and(|binding| binding.state == AccountResourceBindingState::Retiring)
        {
            return Err(AccountResourceTransitionError::LifecycleConflict);
        }
        if replacement_claim_identity.bare_jid() != &self.full_jid.to_bare() {
            return Err(AccountResourceTransitionError::AccountMismatch);
        }
        if replacement_claim_identity == &self.claim_identity
            || self
                .retired_claim_identities
                .contains(replacement_claim_identity)
        {
            return Err(AccountResourceTransitionError::ClaimIdentityReused);
        }
        let revision = self.next_revision()?;

        self.retired_claim_identities
            .push(self.claim_identity.clone());
        self.claim_identity = replacement_claim_identity.clone();
        self.revision = revision;
        Ok(revision)
    }

    fn adopt_binding(
        &mut self,
        claim_identity: &UserClaimIdentity,
        expected_revision: AccountResourceRevision,
        binding_ref: &BindingRef,
        expected_binding_claim_identity: &UserClaimIdentity,
        actor_registration: Option<&ActorRegistrationReceipt>,
    ) -> Result<AccountResourceRevision, AccountResourceTransitionError> {
        self.check_actor(claim_identity)?;
        self.check_revision(expected_revision)?;
        self.check_binding(binding_ref)?;
        let next_state = {
            let binding = self.current_binding()?;
            if binding.binding_claim_identity != *expected_binding_claim_identity
                || expected_binding_claim_identity == claim_identity
            {
                return Err(AccountResourceTransitionError::BindingOwnerMismatch);
            }
            if binding.state == AccountResourceBindingState::Retiring {
                return Err(AccountResourceTransitionError::LifecycleConflict);
            }
            match (binding.state, actor_registration) {
                (AccountResourceBindingState::Prepared, None) => {
                    AccountResourceBindingState::Prepared
                }
                (AccountResourceBindingState::Published, None) => {
                    AccountResourceBindingState::Published
                }
                (
                    AccountResourceBindingState::Published | AccountResourceBindingState::Active,
                    Some(registration),
                ) if registration.claim_identity == *claim_identity
                    && registration.source == binding.source =>
                {
                    AccountResourceBindingState::Active
                }
                (AccountResourceBindingState::Retired, _) => {
                    return Err(AccountResourceTransitionError::LifecycleConflict);
                }
                _ => return Err(AccountResourceTransitionError::ActorRegistrationMismatch),
            }
        };
        let revision = self.next_revision()?;
        let binding = self.current_binding_mut()?;

        binding.binding_claim_identity = claim_identity.clone();
        binding.state = next_state;
        binding.registration = actor_registration.cloned();
        self.revision = revision;
        Ok(revision)
    }

    fn successful_resume(
        &mut self,
        claim_identity: &UserClaimIdentity,
        expected_revision: AccountResourceRevision,
        previous_binding_ref: &BindingRef,
        binding_ref: &BindingRef,
        receipt: &SuccessfulResumeReceipt,
    ) -> Result<AccountResourceRevision, AccountResourceTransitionError> {
        self.check_actor(claim_identity)?;
        self.check_revision(expected_revision)?;
        self.check_binding(previous_binding_ref)?;
        let (binding_claim_identity, snapshot, snapshot_revision) = {
            let binding = self.current_binding()?;
            if binding.binding_claim_identity != *claim_identity
                || binding.state != AccountResourceBindingState::Active
            {
                return Err(AccountResourceTransitionError::BindingOwnerMismatch);
            }
            let Some(stream_management_ref) = binding.snapshot.stream_management_ref.as_ref()
            else {
                return Err(AccountResourceTransitionError::ResumeRequiresStreamManagement);
            };
            if receipt.previous != binding.source
                || receipt.resumed != BindingEvidence::for_binding(binding_ref)
                || &receipt.stream_management_ref != stream_management_ref
                || receipt.custody_disposition == BindingRetirementDisposition::Unknown
            {
                return Err(AccountResourceTransitionError::ResumeBindingMismatch);
            }
            (
                binding.binding_claim_identity.clone(),
                binding.snapshot.clone(),
                binding.snapshot_revision,
            )
        };
        if self.seen_binding_refs.contains(binding_ref) {
            return Err(AccountResourceTransitionError::BindingRefReused);
        }
        if binding_ref.connection_ref() == previous_binding_ref.connection_ref() {
            return Err(AccountResourceTransitionError::ResumeConnectionMismatch);
        }
        let revision = self.next_revision()?;

        self.successful_resumes.push(receipt.clone());
        self.binding = Some(AccountResourceBindingReceipt {
            source: BindingEvidence::for_binding(binding_ref),
            binding_claim_identity,
            state: AccountResourceBindingState::Prepared,
            snapshot,
            snapshot_revision,
            publication: None,
            registration: None,
            resumed_from: Some(receipt.clone()),
            retirement_start: None,
            retirement: None,
        });
        self.seen_binding_refs.push(binding_ref.clone());
        self.revision = revision;
        Ok(revision)
    }

    fn current_binding(
        &self,
    ) -> Result<&AccountResourceBindingReceipt, AccountResourceTransitionError> {
        self.binding
            .as_ref()
            .ok_or(AccountResourceTransitionError::NotBound)
    }

    fn current_binding_mut(
        &mut self,
    ) -> Result<&mut AccountResourceBindingReceipt, AccountResourceTransitionError> {
        self.binding
            .as_mut()
            .ok_or(AccountResourceTransitionError::NotBound)
    }

    fn check_actor(
        &self,
        claim_identity: &UserClaimIdentity,
    ) -> Result<(), AccountResourceTransitionError> {
        if claim_identity != &self.claim_identity {
            return Err(AccountResourceTransitionError::ClaimMismatch);
        }
        Ok(())
    }

    fn check_revision(
        &self,
        expected_revision: AccountResourceRevision,
    ) -> Result<(), AccountResourceTransitionError> {
        if expected_revision != self.revision {
            return Err(AccountResourceTransitionError::RevisionConflict);
        }
        Ok(())
    }

    fn check_binding(
        &self,
        binding_ref: &BindingRef,
    ) -> Result<(), AccountResourceTransitionError> {
        match self.binding_ref() {
            Some(current) if current == binding_ref => Ok(()),
            Some(_) => Err(AccountResourceTransitionError::BindingMismatch),
            None => Err(AccountResourceTransitionError::NotBound),
        }
    }

    fn next_revision(&self) -> Result<AccountResourceRevision, AccountResourceTransitionError> {
        self.revision
            .next()
            .ok_or(AccountResourceTransitionError::RevisionExhausted)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ownership::{ClaimEpoch, NodeIdentity, NodeLeaseIdentity};

    fn claim(node_id: &str, node_epoch: &str, claim_epoch: i64) -> UserClaimIdentity {
        UserClaimIdentity::fresh(
            "alice@example.test".parse().expect("valid bare JID"),
            NodeLeaseIdentity::from_node_identity(&NodeIdentity::new(node_id, node_epoch)),
            ClaimEpoch(claim_epoch),
        )
    }

    fn slot_with_claim(claim_identity: UserClaimIdentity) -> AccountResourceSlot {
        AccountResourceSlot::new(
            "alice@example.test/phone".parse().expect("valid full JID"),
            claim_identity,
        )
        .expect("claim belongs to full-JID account")
    }

    fn slot() -> AccountResourceSlot {
        slot_with_claim(claim("node-a", "epoch-a", 1))
    }

    fn local_connection() -> ConnectionRef {
        ConnectionRef::fresh(NodeLeaseIdentity::local_process())
    }

    fn binding(connection_ref: ConnectionRef) -> BindingRef {
        BindingRef::fresh(connection_ref)
    }

    fn operation(mutation: AccountResourceMutation) -> AccountResourceOperation {
        AccountResourceOperation::new(AccountResourceOperationId::fresh(), mutation)
    }

    fn apply(
        slot: &mut AccountResourceSlot,
        mutation: AccountResourceMutation,
    ) -> AccountResourceOperationOutcome {
        match slot
            .execute(operation(mutation))
            .expect("execute operation")
        {
            AccountResourceExecution::Applied(outcome) => outcome,
            AccountResourceExecution::Replayed(_) => panic!("fresh operation cannot be replayed"),
        }
    }

    fn commit(
        mutation: AccountResourceMutation,
        slot: &mut AccountResourceSlot,
    ) -> AccountResourceRevision {
        match apply(slot, mutation) {
            AccountResourceOperationOutcome::Committed { revision } => revision,
            other => panic!("expected committed mutation, got {other:?}"),
        }
    }

    fn reject(
        mutation: AccountResourceMutation,
        slot: &mut AccountResourceSlot,
        expected: AccountResourceTransitionError,
    ) {
        assert_eq!(
            apply(slot, mutation),
            AccountResourceOperationOutcome::Rejected(expected)
        );
    }

    fn bind_prepared(
        slot: &mut AccountResourceSlot,
        claim_identity: &UserClaimIdentity,
        binding_ref: &BindingRef,
    ) {
        commit(
            AccountResourceMutation::Bind {
                claim_identity: claim_identity.clone(),
                expected_revision: slot.revision(),
                binding_ref: binding_ref.clone(),
            },
            slot,
        );
        assert_eq!(
            slot.binding_receipt()
                .map(AccountResourceBindingReceipt::state),
            Some(AccountResourceBindingState::Prepared)
        );
    }

    fn publish_endpoint(
        slot: &mut AccountResourceSlot,
        claim_identity: &UserClaimIdentity,
        binding_ref: &BindingRef,
    ) {
        commit(
            AccountResourceMutation::PublishEndpoint {
                claim_identity: claim_identity.clone(),
                expected_revision: slot.revision(),
                binding_ref: binding_ref.clone(),
                publication: EndpointPublicationReceipt::observed(BindingEvidence::for_binding(
                    binding_ref,
                )),
            },
            slot,
        );
        assert_eq!(
            slot.binding_receipt()
                .map(AccountResourceBindingReceipt::state),
            Some(AccountResourceBindingState::Published)
        );
    }

    fn register_actor(
        slot: &mut AccountResourceSlot,
        claim_identity: &UserClaimIdentity,
        binding_ref: &BindingRef,
    ) {
        commit(
            AccountResourceMutation::RegisterActor {
                claim_identity: claim_identity.clone(),
                expected_revision: slot.revision(),
                binding_ref: binding_ref.clone(),
                registration: ActorRegistrationReceipt::observed(
                    claim_identity.clone(),
                    BindingEvidence::for_binding(binding_ref),
                ),
            },
            slot,
        );
        assert_eq!(
            slot.binding_receipt()
                .map(AccountResourceBindingReceipt::state),
            Some(AccountResourceBindingState::Active)
        );
    }

    fn bind_active(
        slot: &mut AccountResourceSlot,
        claim_identity: &UserClaimIdentity,
        binding_ref: &BindingRef,
    ) {
        bind_prepared(slot, claim_identity, binding_ref);
        publish_endpoint(slot, claim_identity, binding_ref);
        register_actor(slot, claim_identity, binding_ref);
    }

    fn snapshot(
        available: bool,
        show: Option<Show>,
        status: Option<&str>,
        priority: i8,
        payloads: Vec<Element>,
        carbons_enabled: bool,
        roster_interested: bool,
        blocklist_interested: bool,
        stream_management_ref: Option<SmSessionId>,
    ) -> AccountResourceSnapshot {
        AccountResourceSnapshot {
            presence_available: available,
            presence_show: show,
            presence_status: status.map(str::to_owned),
            presence_priority: priority,
            presence_payloads: payloads,
            carbons_enabled,
            roster_interested,
            blocklist_interested,
            stream_management_ref,
        }
    }

    fn sample_snapshot() -> AccountResourceSnapshot {
        snapshot(
            true,
            Some(Show::Away),
            Some("out for lunch"),
            7,
            vec![
                r#"<c xmlns="urn:xmpp:caps" hash="sha-1" node="urn:example" ver="abc"/>"#
                    .parse()
                    .expect("valid caps payload"),
            ],
            true,
            true,
            false,
            Some(SmSessionId::new("sm-resource-1")),
        )
    }

    fn deep_payload(depth: usize) -> Element {
        let mut child = Element::bare("node", "urn:deep");
        for _ in 0..depth {
            let mut parent = Element::bare("node", "urn:deep");
            parent.append_child(child);
            child = parent;
        }
        child
    }

    fn assert_payload_update_rejected(
        slot: &mut AccountResourceSlot,
        claim_identity: &UserClaimIdentity,
        binding_ref: &BindingRef,
        payload: Element,
        upper_bound_within_limit: bool,
    ) {
        assert_eq!(
            presence_payloads_within_serialized_upper_bound(std::slice::from_ref(&payload)),
            upper_bound_within_limit
        );
        let before_revision = slot.revision();
        let before_snapshot = slot.snapshot().cloned().expect("active snapshot");
        let operation = operation(AccountResourceMutation::UpdateSnapshot {
            claim_identity: claim_identity.clone(),
            expected_revision: before_revision,
            binding_ref: binding_ref.clone(),
            snapshot: snapshot(
                true,
                None,
                None,
                0,
                vec![payload],
                false,
                false,
                false,
                None,
            ),
        });

        assert_eq!(
            slot.execute(operation.clone()),
            Err(AccountResourceExecuteError::SnapshotTooLarge)
        );
        assert_eq!(slot.revision(), before_revision);
        assert_eq!(
            slot.lookup(&operation),
            AccountResourceOperationLookup::Unknown
        );
        assert_eq!(slot.snapshot(), Some(&before_snapshot));
    }

    fn retirement_evidence(
        slot: &AccountResourceSlot,
        custody_disposition: BindingRetirementDisposition,
        directed_presence: DirectedPresenceTeardownDisposition,
        presentation_flush: PresentationFlushDisposition,
    ) -> AccountResourceRetirementEvidence {
        let binding = slot.binding_receipt().expect("current binding");
        let endpoint = match binding.publication() {
            Some(publication) => EndpointRetirementEvidence::Unpublished {
                publication: publication.clone(),
                removed: binding.source.clone(),
            },
            None => EndpointRetirementEvidence::NeverPublished {
                source: binding.source.clone(),
            },
        };
        let actor = match binding.registration() {
            Some(registration) => ActorUnregistrationEvidence::Unregistered {
                registration: registration.clone(),
                removed: binding.source.clone(),
            },
            None => ActorUnregistrationEvidence::NeverRegistered {
                claim_identity: binding.binding_claim_identity.clone(),
                source: binding.source.clone(),
            },
        };

        AccountResourceRetirementEvidence {
            endpoint,
            actor,
            directed_presence: DirectedPresenceTeardownReceipt::observed(
                binding.source.clone(),
                binding.snapshot_revision,
                directed_presence,
            ),
            presentation_flush: PresentationFlushReceipt::observed(
                binding.source.clone(),
                binding.snapshot_revision,
                presentation_flush,
            ),
            custody_disposition,
        }
    }

    fn begin_retirement(
        slot: &mut AccountResourceSlot,
        claim_identity: &UserClaimIdentity,
        binding_ref: &BindingRef,
        custody_disposition: BindingRetirementDisposition,
    ) {
        commit(
            AccountResourceMutation::BeginRetirement {
                claim_identity: claim_identity.clone(),
                expected_revision: slot.revision(),
                binding_ref: binding_ref.clone(),
                custody_disposition,
            },
            slot,
        );
        assert_eq!(
            slot.binding_receipt()
                .map(AccountResourceBindingReceipt::state),
            Some(AccountResourceBindingState::Retiring)
        );
    }

    fn complete_retirement(
        slot: &mut AccountResourceSlot,
        claim_identity: &UserClaimIdentity,
        binding_ref: &BindingRef,
        evidence: AccountResourceRetirementEvidence,
    ) {
        commit(
            AccountResourceMutation::CompleteRetirement {
                claim_identity: claim_identity.clone(),
                expected_revision: slot.revision(),
                binding_ref: binding_ref.clone(),
                evidence: Box::new(evidence),
            },
            slot,
        );
        assert_eq!(slot.binding_ref(), None);
    }

    #[test]
    fn bind_requires_exact_publication_then_actor_registration_before_activation() {
        let mut slot = slot();
        let claim_identity = slot.claim_identity().clone();
        let binding_ref = binding(local_connection());
        bind_prepared(&mut slot, &claim_identity, &binding_ref);

        reject(
            AccountResourceMutation::UpdateSnapshot {
                claim_identity: claim_identity.clone(),
                expected_revision: slot.revision(),
                binding_ref: binding_ref.clone(),
                snapshot: sample_snapshot(),
            },
            &mut slot,
            AccountResourceTransitionError::LifecycleConflict,
        );
        reject(
            AccountResourceMutation::RegisterActor {
                claim_identity: claim_identity.clone(),
                expected_revision: slot.revision(),
                binding_ref: binding_ref.clone(),
                registration: ActorRegistrationReceipt::observed(
                    claim_identity.clone(),
                    BindingEvidence::for_binding(&binding_ref),
                ),
            },
            &mut slot,
            AccountResourceTransitionError::LifecycleConflict,
        );

        let mismatched_connection = local_connection();
        reject(
            AccountResourceMutation::PublishEndpoint {
                claim_identity: claim_identity.clone(),
                expected_revision: slot.revision(),
                binding_ref: binding_ref.clone(),
                publication: EndpointPublicationReceipt::observed(BindingEvidence::observed(
                    binding_ref.clone(),
                    mismatched_connection,
                )),
            },
            &mut slot,
            AccountResourceTransitionError::EndpointPublicationMismatch,
        );
        assert_eq!(
            slot.binding_receipt()
                .map(AccountResourceBindingReceipt::state),
            Some(AccountResourceBindingState::Prepared)
        );

        publish_endpoint(&mut slot, &claim_identity, &binding_ref);
        let other_actor = claim("node-a", "epoch-a", 1);
        reject(
            AccountResourceMutation::RegisterActor {
                claim_identity: claim_identity.clone(),
                expected_revision: slot.revision(),
                binding_ref: binding_ref.clone(),
                registration: ActorRegistrationReceipt::observed(
                    other_actor,
                    BindingEvidence::for_binding(&binding_ref),
                ),
            },
            &mut slot,
            AccountResourceTransitionError::ActorRegistrationMismatch,
        );
        register_actor(&mut slot, &claim_identity, &binding_ref);
        let receipt = slot.binding_receipt().expect("active receipt");
        assert_eq!(
            receipt.publication(),
            Some(&EndpointPublicationReceipt::observed(
                BindingEvidence::for_binding(&binding_ref),
            ))
        );
        assert_eq!(
            receipt.registration(),
            Some(&ActorRegistrationReceipt::observed(
                claim_identity,
                BindingEvidence::for_binding(&binding_ref),
            ))
        );
    }

    #[test]
    fn stale_actor_incarnation_and_claim_movement_require_exact_adoption() {
        let mut slot = slot();
        let predecessor = slot.claim_identity().clone();
        let binding_ref = binding(local_connection());
        bind_active(&mut slot, &predecessor, &binding_ref);
        let original_snapshot = sample_snapshot();
        commit(
            AccountResourceMutation::UpdateSnapshot {
                claim_identity: predecessor.clone(),
                expected_revision: slot.revision(),
                binding_ref: binding_ref.clone(),
                snapshot: original_snapshot.clone(),
            },
            &mut slot,
        );

        let successor = claim("node-a", "epoch-a", 1);
        assert_ne!(predecessor, successor);
        assert_eq!(predecessor.owner(), successor.owner());
        assert_eq!(predecessor.claim_epoch(), successor.claim_epoch());
        commit(
            AccountResourceMutation::ReplaceClaim {
                claim_identity: predecessor.clone(),
                expected_revision: slot.revision(),
                binding_ref: binding_ref.clone(),
                replacement_claim_identity: successor.clone(),
            },
            &mut slot,
        );
        assert_eq!(slot.snapshot(), Some(&original_snapshot));

        reject(
            AccountResourceMutation::UpdateSnapshot {
                claim_identity: predecessor.clone(),
                expected_revision: slot.revision(),
                binding_ref: binding_ref.clone(),
                snapshot: AccountResourceSnapshot::default(),
            },
            &mut slot,
            AccountResourceTransitionError::ClaimMismatch,
        );
        reject(
            AccountResourceMutation::UpdateSnapshot {
                claim_identity: successor.clone(),
                expected_revision: slot.revision(),
                binding_ref: binding_ref.clone(),
                snapshot: AccountResourceSnapshot::default(),
            },
            &mut slot,
            AccountResourceTransitionError::BindingOwnerMismatch,
        );

        let wrong_predecessor = claim("node-a", "epoch-a", 1);
        reject(
            AccountResourceMutation::AdoptBinding {
                claim_identity: successor.clone(),
                expected_revision: slot.revision(),
                binding_ref: binding_ref.clone(),
                expected_binding_claim_identity: wrong_predecessor,
                actor_registration: Some(ActorRegistrationReceipt::observed(
                    successor.clone(),
                    BindingEvidence::for_binding(&binding_ref),
                )),
            },
            &mut slot,
            AccountResourceTransitionError::BindingOwnerMismatch,
        );
        commit(
            AccountResourceMutation::AdoptBinding {
                claim_identity: successor.clone(),
                expected_revision: slot.revision(),
                binding_ref: binding_ref.clone(),
                expected_binding_claim_identity: predecessor.clone(),
                actor_registration: Some(ActorRegistrationReceipt::observed(
                    successor.clone(),
                    BindingEvidence::for_binding(&binding_ref),
                )),
            },
            &mut slot,
        );
        assert_eq!(
            slot.binding_receipt()
                .expect("adopted receipt")
                .binding_claim_identity(),
            &successor
        );
        assert_eq!(slot.snapshot(), Some(&original_snapshot));

        reject(
            AccountResourceMutation::UpdateSnapshot {
                claim_identity: predecessor,
                expected_revision: slot.revision(),
                binding_ref: binding_ref.clone(),
                snapshot: AccountResourceSnapshot::default(),
            },
            &mut slot,
            AccountResourceTransitionError::ClaimMismatch,
        );
        let updated = snapshot(
            false,
            None,
            Some("offline"),
            -1,
            Vec::new(),
            false,
            false,
            true,
            Some(SmSessionId::new("sm-resource-1")),
        );
        commit(
            AccountResourceMutation::UpdateSnapshot {
                claim_identity: successor,
                expected_revision: slot.revision(),
                binding_ref,
                snapshot: updated.clone(),
            },
            &mut slot,
        );
        assert_eq!(slot.snapshot(), Some(&updated));
    }

    #[test]
    fn failed_resume_then_fresh_bind_on_one_transport_uses_a_distinct_binding_ref() {
        let mut slot = slot();
        let claim_identity = slot.claim_identity().clone();
        let connection = local_connection();
        let failed_resume = binding(connection.clone());
        let fresh_bind = binding(connection);
        assert_eq!(failed_resume.connection_ref(), fresh_bind.connection_ref());
        assert_ne!(failed_resume, fresh_bind);

        bind_prepared(&mut slot, &claim_identity, &failed_resume);
        begin_retirement(
            &mut slot,
            &claim_identity,
            &failed_resume,
            BindingRetirementDisposition::NoAcceptedWork,
        );
        let failed_resume_evidence = retirement_evidence(
            &slot,
            BindingRetirementDisposition::NoAcceptedWork,
            DirectedPresenceTeardownDisposition::NoDirectedRecipients,
            PresentationFlushDisposition::NoPendingPresentation,
        );
        complete_retirement(
            &mut slot,
            &claim_identity,
            &failed_resume,
            failed_resume_evidence,
        );

        bind_active(&mut slot, &claim_identity, &fresh_bind);
        let fresh_snapshot = sample_snapshot();
        commit(
            AccountResourceMutation::UpdateSnapshot {
                claim_identity: claim_identity.clone(),
                expected_revision: slot.revision(),
                binding_ref: fresh_bind.clone(),
                snapshot: fresh_snapshot.clone(),
            },
            &mut slot,
        );
        reject(
            AccountResourceMutation::RegisterActor {
                claim_identity: claim_identity.clone(),
                expected_revision: slot.revision(),
                binding_ref: failed_resume,
                registration: ActorRegistrationReceipt::observed(
                    claim_identity,
                    BindingEvidence::for_binding(&fresh_bind),
                ),
            },
            &mut slot,
            AccountResourceTransitionError::BindingMismatch,
        );
        assert_eq!(slot.binding_ref(), Some(&fresh_bind));
        assert_eq!(slot.snapshot(), Some(&fresh_snapshot));
    }

    #[test]
    fn snapshot_updates_are_atomic_revision_fenced_bounded_and_payload_bound() {
        let mut slot = slot();
        let claim_identity = slot.claim_identity().clone();
        let binding_ref = binding(local_connection());
        bind_active(&mut slot, &claim_identity, &binding_ref);

        let original = sample_snapshot();
        let expected_revision = slot.revision();
        let update = operation(AccountResourceMutation::UpdateSnapshot {
            claim_identity: claim_identity.clone(),
            expected_revision,
            binding_ref: binding_ref.clone(),
            snapshot: original.clone(),
        });
        let committed = slot.execute(update.clone()).expect("update snapshot");
        let new_revision = match committed {
            AccountResourceExecution::Applied(AccountResourceOperationOutcome::Committed {
                revision,
            }) => revision,
            other => panic!("expected committed snapshot update, got {other:?}"),
        };
        assert_eq!(slot.snapshot(), Some(&original));
        assert_eq!(
            slot.execute(update.clone()).expect("replay snapshot"),
            AccountResourceExecution::Replayed(AccountResourceOperationOutcome::Committed {
                revision: new_revision,
            })
        );

        let equivalent_payload_snapshot = snapshot(
            true,
            Some(Show::Away),
            Some("out for lunch"),
            7,
            vec![
                r#"<c ver="abc" node="urn:example" hash="sha-1" xmlns="urn:xmpp:caps"/>"#
                    .parse()
                    .expect("equivalent caps payload"),
            ],
            true,
            true,
            false,
            Some(SmSessionId::new("sm-resource-1")),
        );
        let equivalent_replay = AccountResourceOperation::new(
            update.id,
            AccountResourceMutation::UpdateSnapshot {
                claim_identity: claim_identity.clone(),
                expected_revision,
                binding_ref: binding_ref.clone(),
                snapshot: equivalent_payload_snapshot.clone(),
            },
        );
        assert_eq!(
            slot.execute(equivalent_replay)
                .expect("structurally equal payload replay"),
            AccountResourceExecution::Replayed(AccountResourceOperationOutcome::Committed {
                revision: new_revision,
            })
        );

        let conflicting = AccountResourceOperation::new(
            update.id,
            AccountResourceMutation::UpdateSnapshot {
                claim_identity: claim_identity.clone(),
                expected_revision,
                binding_ref: binding_ref.clone(),
                snapshot: snapshot(
                    true,
                    Some(Show::Away),
                    Some("different status"),
                    7,
                    original.presence_payloads.clone(),
                    true,
                    true,
                    false,
                    original.stream_management_ref.clone(),
                ),
            },
        );
        assert_eq!(
            slot.execute(conflicting.clone()),
            Err(AccountResourceExecuteError::OperationIdConflict)
        );
        assert_eq!(
            slot.lookup(&conflicting),
            AccountResourceOperationLookup::PayloadMismatch
        );

        reject(
            AccountResourceMutation::UpdateSnapshot {
                claim_identity: claim_identity.clone(),
                expected_revision,
                binding_ref: binding_ref.clone(),
                snapshot: AccountResourceSnapshot::default(),
            },
            &mut slot,
            AccountResourceTransitionError::RevisionConflict,
        );
        assert_eq!(slot.snapshot(), Some(&original));
        let changed_sm_reference = snapshot(
            true,
            Some(Show::Away),
            Some("out for lunch"),
            7,
            original.presence_payloads.clone(),
            true,
            true,
            false,
            Some(SmSessionId::new("replacement-sm-id")),
        );
        reject(
            AccountResourceMutation::UpdateSnapshot {
                claim_identity: claim_identity.clone(),
                expected_revision: slot.revision(),
                binding_ref: binding_ref.clone(),
                snapshot: changed_sm_reference,
            },
            &mut slot,
            AccountResourceTransitionError::StreamManagementReferenceChanged,
        );
        assert_eq!(slot.snapshot(), Some(&original));

        let oversized = snapshot(
            true,
            None,
            Some(&"x".repeat(ACCOUNT_RESOURCE_MAX_STATUS_BYTES + 1)),
            0,
            Vec::new(),
            false,
            false,
            false,
            None,
        );
        let oversized_operation = operation(AccountResourceMutation::UpdateSnapshot {
            claim_identity: claim_identity.clone(),
            expected_revision: slot.revision(),
            binding_ref: binding_ref.clone(),
            snapshot: oversized,
        });
        let before_oversized = slot.revision();
        assert_eq!(
            slot.execute(oversized_operation.clone()),
            Err(AccountResourceExecuteError::SnapshotTooLarge)
        );
        assert_eq!(slot.revision(), before_oversized);
        assert_eq!(
            slot.lookup(&oversized_operation),
            AccountResourceOperationLookup::Unknown
        );
        assert_eq!(slot.snapshot(), Some(&original));

        let oversized_payload = Element::builder("payload", "urn:example")
            .append("x".repeat(ACCOUNT_RESOURCE_MAX_PRESENCE_PAYLOAD_BYTES + 1))
            .build();
        assert!(!presence_payloads_within_serialized_upper_bound(
            std::slice::from_ref(&oversized_payload)
        ));

        let oversized_payload_snapshot = snapshot(
            true,
            None,
            None,
            0,
            vec![oversized_payload],
            false,
            false,
            false,
            None,
        );
        let oversized_payload_operation = operation(AccountResourceMutation::UpdateSnapshot {
            claim_identity: claim_identity.clone(),
            expected_revision: slot.revision(),
            binding_ref: binding_ref.clone(),
            snapshot: oversized_payload_snapshot,
        });
        let before_oversized_payload = slot.revision();
        assert_eq!(
            slot.execute(oversized_payload_operation.clone()),
            Err(AccountResourceExecuteError::SnapshotTooLarge)
        );
        assert_eq!(slot.revision(), before_oversized_payload);
        assert_eq!(
            slot.lookup(&oversized_payload_operation),
            AccountResourceOperationLookup::Unknown
        );
        assert_eq!(slot.snapshot(), Some(&original));

        let payload_with_prefix_one = Element::builder("payload", "urn:example")
            .prefix(Some("p".to_owned()), "urn:prefix:one")
            .expect("first prefix declaration")
            .attr(minidom::rxml::xml_ncname!("type").to_owned(), "p:Thing")
            .build();
        let payload_with_prefix_two = Element::builder("payload", "urn:example")
            .prefix(Some("p".to_owned()), "urn:prefix:two")
            .expect("second prefix declaration")
            .attr(minidom::rxml::xml_ncname!("type").to_owned(), "p:Thing")
            .build();
        let first_prefix_snapshot = snapshot(
            true,
            None,
            None,
            0,
            vec![payload_with_prefix_one],
            false,
            false,
            false,
            None,
        );
        let second_prefix_snapshot = snapshot(
            true,
            None,
            None,
            0,
            vec![payload_with_prefix_two],
            false,
            false,
            false,
            None,
        );
        assert_ne!(first_prefix_snapshot, second_prefix_snapshot);

        let prefix_expected_revision = slot.revision();
        let prefix_operation = AccountResourceOperation::new(
            AccountResourceOperationId::fresh(),
            AccountResourceMutation::UpdateSnapshot {
                claim_identity: claim_identity.clone(),
                expected_revision: prefix_expected_revision,
                binding_ref: binding_ref.clone(),
                snapshot: first_prefix_snapshot,
            },
        );
        slot.execute(prefix_operation.clone())
            .expect("commit first prefix mapping");
        let prefix_conflict = AccountResourceOperation::new(
            prefix_operation.id,
            AccountResourceMutation::UpdateSnapshot {
                claim_identity,
                expected_revision: prefix_expected_revision,
                binding_ref,
                snapshot: second_prefix_snapshot,
            },
        );
        assert_eq!(
            slot.execute(prefix_conflict.clone()),
            Err(AccountResourceExecuteError::OperationIdConflict)
        );
        assert_eq!(
            slot.lookup(&prefix_conflict),
            AccountResourceOperationLookup::PayloadMismatch
        );
    }

    #[test]
    fn xml_tree_preflight_bounds_text_attributes_namespaces_and_depth() {
        let mut slot = slot();
        let claim_identity = slot.claim_identity().clone();
        let binding_ref = binding(local_connection());
        bind_active(&mut slot, &claim_identity, &binding_ref);

        let oversized_text = Element::builder("payload", "urn:example")
            .append("x".repeat(ACCOUNT_RESOURCE_MAX_PRESENCE_PAYLOAD_BYTES + 1))
            .build();
        assert_payload_update_rejected(
            &mut slot,
            &claim_identity,
            &binding_ref,
            oversized_text,
            false,
        );

        let oversized_attribute = Element::builder("payload", "urn:example")
            .attr(
                minidom::rxml::xml_ncname!("value").to_owned(),
                "x".repeat(ACCOUNT_RESOURCE_MAX_PRESENCE_PAYLOAD_BYTES + 1),
            )
            .build();
        assert_payload_update_rejected(
            &mut slot,
            &claim_identity,
            &binding_ref,
            oversized_attribute,
            false,
        );

        let oversized_namespace = Element::bare(
            "payload",
            "x".repeat(ACCOUNT_RESOURCE_MAX_PRESENCE_PAYLOAD_BYTES + 1),
        );
        assert_payload_update_rejected(
            &mut slot,
            &claim_identity,
            &binding_ref,
            oversized_namespace,
            false,
        );

        let mut deep_payload = Element::bare("leaf", "urn:example");
        for _ in 0..=ACCOUNT_RESOURCE_MAX_PRESENCE_PAYLOAD_DEPTH {
            let mut parent = Element::bare("node", "urn:example");
            parent.append_child(deep_payload);
            deep_payload = parent;
        }
        assert_payload_update_rejected(
            &mut slot,
            &claim_identity,
            &binding_ref,
            deep_payload,
            false,
        );

        // Escaped-byte accounting rejects this without constructing a
        // serialized buffer: each ampersand expands to `&amp;`.
        let escaped_overflow = Element::builder("payload", "urn:example")
            .append("&".repeat(ACCOUNT_RESOURCE_MAX_PRESENCE_PAYLOAD_BYTES / 4 + 1))
            .build();
        assert!(!presence_payloads_within_serialized_upper_bound(
            std::slice::from_ref(&escaped_overflow)
        ));
        assert_payload_update_rejected(
            &mut slot,
            &claim_identity,
            &binding_ref,
            escaped_overflow,
            false,
        );
    }

    #[test]
    fn payload_bound_avoids_minidom_serializer_and_allows_plain_text_over_five_kib() {
        let large_text = Element::builder("payload", "urn:example")
            .append("x".repeat(6 * 1024))
            .build();
        assert!(presence_payloads_within_serialized_upper_bound(&[
            large_text.clone()
        ]));
        assert!(snapshot(
            true,
            None,
            None,
            0,
            vec![large_text],
            false,
            false,
            false,
            None,
        )
        .within_bounds());

        let child_with_rebound_prefix = Element::builder("child", "urn:child")
            .prefix(Some("p".to_owned()), "urn:child-prefix")
            .expect("child prefix")
            .build();
        let prefix_rebinding_tree = Element::builder("payload", "urn:parent")
            .prefix(Some("p".to_owned()), "urn:parent-prefix")
            .expect("parent prefix")
            .append(child_with_rebound_prefix)
            .build();
        assert!(presence_payloads_within_serialized_upper_bound(&[
            prefix_rebinding_tree
        ]));

        let explicit_xml_prefix = Element::builder("payload", "urn:example")
            .prefix(
                Some("xml".to_owned()),
                "http://www.w3.org/XML/1998/namespace",
            )
            .expect("valid explicit xml prefix")
            .build();
        assert!(presence_payloads_within_serialized_upper_bound(&[
            explicit_xml_prefix
        ]));

        let whitespace_expansion = Element::builder("payload", "urn:example")
            .attr(
                minidom::rxml::xml_ncname!("value").to_owned(),
                "\r\n\t".repeat(3 * 1024),
            )
            .build();
        assert!(!presence_payloads_within_serialized_upper_bound(&[
            whitespace_expansion
        ]));

        let invalid_builder_name = Element::builder("payload\0invalid", "urn:example").build();
        assert!(!presence_payloads_within_serialized_upper_bound(&[
            invalid_builder_name
        ]));
    }

    #[test]
    fn near_limit_many_text_nodes_replay_without_false_conflict() {
        let mut slot = slot();
        let claim_identity = slot.claim_identity().clone();
        let binding_ref = binding(local_connection());
        bind_active(&mut slot, &claim_identity, &binding_ref);

        let mut payload = Element::bare("payload", "urn:example");
        for _ in 0..900 {
            payload.append_text_node("x");
        }
        let snapshot = snapshot(
            true,
            None,
            None,
            0,
            vec![payload],
            false,
            false,
            false,
            None,
        );
        assert!(snapshot.within_bounds());

        let update = operation(AccountResourceMutation::UpdateSnapshot {
            claim_identity,
            expected_revision: slot.revision(),
            binding_ref,
            snapshot,
        });
        let applied = slot
            .execute(update.clone())
            .expect("apply bounded snapshot");
        assert!(matches!(
            applied,
            AccountResourceExecution::Applied(AccountResourceOperationOutcome::Committed { .. })
        ));
        assert!(matches!(
            slot.execute(update).expect("replay bounded snapshot"),
            AccountResourceExecution::Replayed(AccountResourceOperationOutcome::Committed { .. })
        ));
    }

    #[test]
    fn conflicting_deep_payload_is_drained_without_recursive_drop() {
        let mut slot = slot();
        let claim_identity = slot.claim_identity().clone();
        let binding_ref = binding(local_connection());
        bind_active(&mut slot, &claim_identity, &binding_ref);

        let expected_revision = slot.revision();
        let retained = operation(AccountResourceMutation::UpdateSnapshot {
            claim_identity: claim_identity.clone(),
            expected_revision,
            binding_ref: binding_ref.clone(),
            snapshot: snapshot(
                true,
                None,
                None,
                0,
                vec![deep_payload(ACCOUNT_RESOURCE_MAX_PRESENCE_PAYLOAD_DEPTH)],
                false,
                false,
                false,
                None,
            ),
        });
        let retained_id = retained.id;
        slot.execute(retained).expect("commit bounded deep payload");

        let rejected = AccountResourceOperation::new(
            retained_id,
            AccountResourceMutation::UpdateSnapshot {
                claim_identity,
                expected_revision,
                binding_ref,
                snapshot: snapshot(
                    true,
                    None,
                    None,
                    0,
                    vec![deep_payload(100_000)],
                    false,
                    false,
                    false,
                    None,
                ),
            },
        );
        std::thread::Builder::new()
            .stack_size(64 * 1024)
            .spawn(move || {
                assert_eq!(
                    slot.lookup(&rejected),
                    AccountResourceOperationLookup::PayloadMismatch
                );
                assert_eq!(
                    slot.execute(rejected),
                    Err(AccountResourceExecuteError::OperationIdConflict)
                );
            })
            .expect("spawn small-stack test thread")
            .join()
            .expect("deep payload rejection must not overflow the thread stack");
    }

    #[test]
    fn accepted_and_rejected_deep_updates_clone_on_a_small_stack() {
        let mut slot = slot();
        let claim_identity = slot.claim_identity().clone();
        let binding_ref = binding(local_connection());
        bind_active(&mut slot, &claim_identity, &binding_ref);

        let accepted_payload = deep_payload(ACCOUNT_RESOURCE_MAX_PRESENCE_PAYLOAD_DEPTH);
        let rejected_payload = deep_payload(ACCOUNT_RESOURCE_MAX_PRESENCE_PAYLOAD_DEPTH);
        let (accepted_result, rejected_result, accepted_snapshot_retained) =
            std::thread::Builder::new()
                .stack_size(64 * 1024)
                .spawn(move || {
                    let accepted_revision = slot.revision();
                    let accepted =
                        slot.execute(operation(AccountResourceMutation::UpdateSnapshot {
                            claim_identity: claim_identity.clone(),
                            expected_revision: accepted_revision,
                            binding_ref: binding_ref.clone(),
                            snapshot: snapshot(
                                true,
                                None,
                                None,
                                0,
                                vec![accepted_payload],
                                false,
                                false,
                                false,
                                None,
                            ),
                        }));
                    let rejected =
                        slot.execute(operation(AccountResourceMutation::UpdateSnapshot {
                            claim_identity,
                            expected_revision: AccountResourceRevision::INITIAL,
                            binding_ref,
                            snapshot: snapshot(
                                true,
                                None,
                                None,
                                0,
                                vec![rejected_payload],
                                false,
                                false,
                                false,
                                None,
                            ),
                        }));
                    let accepted_snapshot_retained = slot
                        .snapshot()
                        .is_some_and(AccountResourceSnapshot::within_bounds);
                    drop(slot);
                    (accepted, rejected, accepted_snapshot_retained)
                })
                .expect("spawn small-stack update test thread")
                .join()
                .expect("snapshot update cloning and drop must not overflow the thread stack");

        assert!(matches!(
            accepted_result,
            Ok(AccountResourceExecution::Applied(
                AccountResourceOperationOutcome::Committed { .. }
            ))
        ));
        assert!(matches!(
            rejected_result,
            Ok(AccountResourceExecution::Applied(
                AccountResourceOperationOutcome::Rejected(
                    AccountResourceTransitionError::RevisionConflict
                )
            ))
        ));
        assert!(accepted_snapshot_retained);
    }

    #[test]
    fn accepted_and_rejected_deep_resumes_clone_and_drop_without_recursion() {
        let mut accepted_slot = slot();
        let accepted_claim = accepted_slot.claim_identity().clone();
        let accepted_previous = binding(local_connection());
        bind_active(&mut accepted_slot, &accepted_claim, &accepted_previous);
        let retained_snapshot = snapshot(
            true,
            None,
            None,
            0,
            vec![deep_payload(ACCOUNT_RESOURCE_MAX_PRESENCE_PAYLOAD_DEPTH)],
            false,
            false,
            false,
            Some(SmSessionId::new("sm-deep")),
        );
        commit(
            AccountResourceMutation::UpdateSnapshot {
                claim_identity: accepted_claim.clone(),
                expected_revision: accepted_slot.revision(),
                binding_ref: accepted_previous.clone(),
                snapshot: retained_snapshot.clone(),
            },
            &mut accepted_slot,
        );
        let accepted_resume_binding = binding(local_connection());
        let accepted_receipt = SuccessfulResumeReceipt::observed(
            BindingEvidence::for_binding(&accepted_previous),
            BindingEvidence::for_binding(&accepted_resume_binding),
            SmSessionId::new("sm-deep"),
            BindingRetirementDisposition::AcceptedWorkHasRecoveryOwner,
        );

        let mut rejected_slot = slot();
        let rejected_claim = rejected_slot.claim_identity().clone();
        let shared_connection = local_connection();
        let rejected_previous = binding(shared_connection.clone());
        bind_active(&mut rejected_slot, &rejected_claim, &rejected_previous);
        commit(
            AccountResourceMutation::UpdateSnapshot {
                claim_identity: rejected_claim.clone(),
                expected_revision: rejected_slot.revision(),
                binding_ref: rejected_previous.clone(),
                snapshot: retained_snapshot.clone(),
            },
            &mut rejected_slot,
        );
        let rejected_resume_binding = binding(shared_connection);
        let rejected_receipt = SuccessfulResumeReceipt::observed(
            BindingEvidence::for_binding(&rejected_previous),
            BindingEvidence::for_binding(&rejected_resume_binding),
            SmSessionId::new("sm-deep"),
            BindingRetirementDisposition::AcceptedWorkHasRecoveryOwner,
        );

        let (
            accepted_result,
            accepted_snapshot_matches,
            rejected_result,
            rejected_snapshot_matches,
        ) = std::thread::Builder::new()
            // A shallow-snapshot probe also overflows before `successful_resume`
            // enters on 64 KiB, so use 128 KiB for this pre-existing transition
            // call path while keeping every owned deep tree on the test thread.
            .stack_size(128 * 1024)
            .spawn(move || {
                let accepted_revision = accepted_slot.revision();
                let accepted_operation = operation(AccountResourceMutation::SuccessfulResume {
                    claim_identity: accepted_claim,
                    expected_revision: accepted_revision,
                    previous_binding_ref: accepted_previous,
                    binding_ref: accepted_resume_binding,
                    receipt: accepted_receipt,
                });
                let accepted = accepted_slot.execute(accepted_operation);
                let accepted_snapshot_matches =
                    accepted_slot.snapshot() == Some(&retained_snapshot);
                let rejected_revision = rejected_slot.revision();
                let rejected =
                    rejected_slot.execute(operation(AccountResourceMutation::SuccessfulResume {
                        claim_identity: rejected_claim,
                        expected_revision: rejected_revision,
                        previous_binding_ref: rejected_previous,
                        binding_ref: rejected_resume_binding,
                        receipt: rejected_receipt,
                    }));
                let rejected_snapshot_matches =
                    rejected_slot.snapshot() == Some(&retained_snapshot);
                drop(accepted_slot);
                drop(rejected_slot);
                drop(retained_snapshot);
                (
                    accepted,
                    accepted_snapshot_matches,
                    rejected,
                    rejected_snapshot_matches,
                )
            })
            .expect("spawn small-stack resume test thread")
            .join()
            .expect("resume snapshot cloning and drop must not overflow the thread stack");

        assert!(matches!(
            accepted_result,
            Ok(AccountResourceExecution::Applied(
                AccountResourceOperationOutcome::Committed { .. }
            ))
        ));
        assert!(accepted_snapshot_matches);
        assert!(matches!(
            rejected_result,
            Ok(AccountResourceExecution::Applied(
                AccountResourceOperationOutcome::Rejected(
                    AccountResourceTransitionError::ResumeConnectionMismatch
                )
            ))
        ));
        assert!(rejected_snapshot_matches);
    }

    #[test]
    fn successful_resume_changes_transport_binding_but_preserves_resource_state() {
        let mut slot = slot();
        let claim_identity = slot.claim_identity().clone();
        let old_binding = binding(local_connection());
        bind_active(&mut slot, &claim_identity, &old_binding);
        let resource_snapshot = sample_snapshot();
        commit(
            AccountResourceMutation::UpdateSnapshot {
                claim_identity: claim_identity.clone(),
                expected_revision: slot.revision(),
                binding_ref: old_binding.clone(),
                snapshot: resource_snapshot.clone(),
            },
            &mut slot,
        );
        let snapshot_revision = slot
            .binding_receipt()
            .expect("active binding")
            .snapshot_revision();
        let resumed_binding = binding(local_connection());
        assert_ne!(
            old_binding.connection_ref(),
            resumed_binding.connection_ref()
        );

        let bad_receipt = SuccessfulResumeReceipt::observed(
            BindingEvidence::for_binding(&old_binding),
            BindingEvidence::for_binding(&resumed_binding),
            SmSessionId::new("wrong-sm-session"),
            BindingRetirementDisposition::AcceptedWorkHasRecoveryOwner,
        );
        reject(
            AccountResourceMutation::SuccessfulResume {
                claim_identity: claim_identity.clone(),
                expected_revision: slot.revision(),
                previous_binding_ref: old_binding.clone(),
                binding_ref: resumed_binding.clone(),
                receipt: bad_receipt,
            },
            &mut slot,
            AccountResourceTransitionError::ResumeBindingMismatch,
        );

        let resume_receipt = SuccessfulResumeReceipt::observed(
            BindingEvidence::for_binding(&old_binding),
            BindingEvidence::for_binding(&resumed_binding),
            SmSessionId::new("sm-resource-1"),
            BindingRetirementDisposition::AcceptedWorkHasRecoveryOwner,
        );
        commit(
            AccountResourceMutation::SuccessfulResume {
                claim_identity: claim_identity.clone(),
                expected_revision: slot.revision(),
                previous_binding_ref: old_binding.clone(),
                binding_ref: resumed_binding.clone(),
                receipt: resume_receipt.clone(),
            },
            &mut slot,
        );
        let prepared = slot.binding_receipt().expect("resumed binding");
        assert_eq!(prepared.binding_ref(), &resumed_binding);
        assert_eq!(prepared.connection_ref(), resumed_binding.connection_ref());
        assert_eq!(prepared.state(), AccountResourceBindingState::Prepared);
        assert_eq!(prepared.snapshot(), &resource_snapshot);
        assert_eq!(prepared.snapshot_revision(), snapshot_revision);
        assert_eq!(prepared.resumed_from(), Some(&resume_receipt));
        assert_eq!(slot.successful_resume_receipts(), &[resume_receipt]);

        reject(
            AccountResourceMutation::UpdateSnapshot {
                claim_identity: claim_identity.clone(),
                expected_revision: slot.revision(),
                binding_ref: old_binding,
                snapshot: AccountResourceSnapshot::default(),
            },
            &mut slot,
            AccountResourceTransitionError::BindingMismatch,
        );
        publish_endpoint(&mut slot, &claim_identity, &resumed_binding);
        register_actor(&mut slot, &claim_identity, &resumed_binding);
        assert_eq!(slot.snapshot(), Some(&resource_snapshot));
    }

    #[test]
    fn cancellation_and_delayed_publication_are_exact_and_fenced() {
        let mut slot = slot();
        let claim_identity = slot.claim_identity().clone();
        let cancelled_binding = binding(local_connection());
        bind_prepared(&mut slot, &claim_identity, &cancelled_binding);
        begin_retirement(
            &mut slot,
            &claim_identity,
            &cancelled_binding,
            BindingRetirementDisposition::NoAcceptedWork,
        );
        let revision_retiring = slot.revision();

        reject(
            AccountResourceMutation::PublishEndpoint {
                claim_identity: claim_identity.clone(),
                expected_revision: revision_retiring,
                binding_ref: cancelled_binding.clone(),
                publication: EndpointPublicationReceipt::observed(BindingEvidence::for_binding(
                    &cancelled_binding,
                )),
            },
            &mut slot,
            AccountResourceTransitionError::LifecycleConflict,
        );
        reject(
            AccountResourceMutation::RegisterActor {
                claim_identity: claim_identity.clone(),
                expected_revision: slot.revision(),
                binding_ref: cancelled_binding.clone(),
                registration: ActorRegistrationReceipt::observed(
                    claim_identity.clone(),
                    BindingEvidence::for_binding(&cancelled_binding),
                ),
            },
            &mut slot,
            AccountResourceTransitionError::LifecycleConflict,
        );
        let invalid = retirement_evidence(
            &slot,
            BindingRetirementDisposition::NoAcceptedWork,
            DirectedPresenceTeardownDisposition::Unknown,
            PresentationFlushDisposition::NoPendingPresentation,
        );
        reject(
            AccountResourceMutation::CompleteRetirement {
                claim_identity: claim_identity.clone(),
                expected_revision: slot.revision(),
                binding_ref: cancelled_binding.clone(),
                evidence: Box::new(invalid),
            },
            &mut slot,
            AccountResourceTransitionError::RetirementEvidenceMismatch,
        );
        let exact = retirement_evidence(
            &slot,
            BindingRetirementDisposition::NoAcceptedWork,
            DirectedPresenceTeardownDisposition::NoDirectedRecipients,
            PresentationFlushDisposition::NoPendingPresentation,
        );
        complete_retirement(&mut slot, &claim_identity, &cancelled_binding, exact);
        assert_eq!(
            slot.retired_binding_receipts()[0].state(),
            AccountResourceBindingState::Retired
        );
        assert!(slot.retired_binding_receipts()[0].retirement().is_some());

        let replacement = binding(local_connection());
        bind_active(&mut slot, &claim_identity, &replacement);
        reject(
            AccountResourceMutation::PublishEndpoint {
                claim_identity: claim_identity.clone(),
                expected_revision: slot.revision(),
                binding_ref: cancelled_binding.clone(),
                publication: EndpointPublicationReceipt::observed(BindingEvidence::for_binding(
                    &cancelled_binding,
                )),
            },
            &mut slot,
            AccountResourceTransitionError::BindingMismatch,
        );
        assert_eq!(slot.binding_ref(), Some(&replacement));
    }

    #[test]
    fn delayed_bind_success_reply_must_match_the_current_full_jid_binding() {
        let mut slot = slot();
        let claim_identity = slot.claim_identity().clone();
        let same_transport = local_connection();
        let delayed_binding = binding(same_transport.clone());
        bind_active(&mut slot, &claim_identity, &delayed_binding);

        // The actor-registration reply can be observed, then delayed before
        // the connection writer emits bind success.
        let delayed_success = slot
            .current_bind_success_receipt(&claim_identity, &delayed_binding)
            .expect("exact active binding has publication and registration evidence");
        assert_eq!(delayed_success.source().binding_ref(), &delayed_binding);

        begin_retirement(
            &mut slot,
            &claim_identity,
            &delayed_binding,
            BindingRetirementDisposition::NoAcceptedWork,
        );
        let retirement = retirement_evidence(
            &slot,
            BindingRetirementDisposition::NoAcceptedWork,
            DirectedPresenceTeardownDisposition::NoDirectedRecipients,
            PresentationFlushDisposition::NoPendingPresentation,
        );
        complete_retirement(&mut slot, &claim_identity, &delayed_binding, retirement);

        // A successor reuses the same full JID and transport identity but has
        // a new BindingRef. The old receipt cannot authorize its bind result.
        let successor_binding = binding(same_transport);
        assert_eq!(
            delayed_binding.connection_ref(),
            successor_binding.connection_ref()
        );
        assert_ne!(delayed_binding, successor_binding);
        bind_active(&mut slot, &claim_identity, &successor_binding);

        assert!(slot
            .current_bind_success_receipt(&claim_identity, &delayed_binding)
            .is_none());
        let successor_success = slot
            .current_bind_success_receipt(&claim_identity, &successor_binding)
            .expect("successor requires its own exact current receipts");
        assert_eq!(successor_success.source().binding_ref(), &successor_binding);
    }

    #[test]
    fn retirement_requires_exact_custody_directed_teardown_and_p1_flush_receipts() {
        let mut slot = slot();
        let claim_identity = slot.claim_identity().clone();
        let binding_ref = binding(local_connection());
        bind_active(&mut slot, &claim_identity, &binding_ref);
        let current_snapshot = snapshot(
            true,
            Some(Show::Chat),
            Some("available"),
            2,
            Vec::new(),
            true,
            true,
            true,
            None,
        );
        commit(
            AccountResourceMutation::UpdateSnapshot {
                claim_identity: claim_identity.clone(),
                expected_revision: slot.revision(),
                binding_ref: binding_ref.clone(),
                snapshot: current_snapshot,
            },
            &mut slot,
        );

        reject(
            AccountResourceMutation::BeginRetirement {
                claim_identity: claim_identity.clone(),
                expected_revision: slot.revision(),
                binding_ref: binding_ref.clone(),
                custody_disposition: BindingRetirementDisposition::Unknown,
            },
            &mut slot,
            AccountResourceTransitionError::RetirementCustodyUnknown,
        );
        begin_retirement(
            &mut slot,
            &claim_identity,
            &binding_ref,
            BindingRetirementDisposition::AcceptedWorkHasRecoveryOwner,
        );

        let mut mismatched_custody = retirement_evidence(
            &slot,
            BindingRetirementDisposition::NoAcceptedWork,
            DirectedPresenceTeardownDisposition::NoDirectedRecipients,
            PresentationFlushDisposition::NoPendingPresentation,
        );
        reject(
            AccountResourceMutation::CompleteRetirement {
                claim_identity: claim_identity.clone(),
                expected_revision: slot.revision(),
                binding_ref: binding_ref.clone(),
                evidence: Box::new(mismatched_custody),
            },
            &mut slot,
            AccountResourceTransitionError::RetirementEvidenceMismatch,
        );

        let source = slot
            .binding_receipt()
            .expect("retiring binding")
            .source
            .clone();
        let snapshot_revision = slot
            .binding_receipt()
            .expect("retiring binding")
            .snapshot_revision;
        mismatched_custody = retirement_evidence(
            &slot,
            BindingRetirementDisposition::AcceptedWorkHasRecoveryOwner,
            DirectedPresenceTeardownDisposition::RecipientsRetractedAndUnavailableRecoveryOwned,
            PresentationFlushDisposition::Unknown,
        );
        reject(
            AccountResourceMutation::CompleteRetirement {
                claim_identity: claim_identity.clone(),
                expected_revision: slot.revision(),
                binding_ref: binding_ref.clone(),
                evidence: Box::new(mismatched_custody),
            },
            &mut slot,
            AccountResourceTransitionError::RetirementEvidenceMismatch,
        );

        let wrong_source = BindingEvidence::observed(binding_ref.clone(), local_connection());
        let wrong_directed = DirectedPresenceTeardownReceipt::observed(
            wrong_source,
            snapshot_revision,
            DirectedPresenceTeardownDisposition::NoDirectedRecipients,
        );
        let mut wrong_binding_evidence = retirement_evidence(
            &slot,
            BindingRetirementDisposition::AcceptedWorkHasRecoveryOwner,
            DirectedPresenceTeardownDisposition::NoDirectedRecipients,
            PresentationFlushDisposition::NoPendingPresentation,
        );
        wrong_binding_evidence.directed_presence = wrong_directed;
        reject(
            AccountResourceMutation::CompleteRetirement {
                claim_identity: claim_identity.clone(),
                expected_revision: slot.revision(),
                binding_ref: binding_ref.clone(),
                evidence: Box::new(wrong_binding_evidence),
            },
            &mut slot,
            AccountResourceTransitionError::RetirementEvidenceMismatch,
        );

        let exact = AccountResourceRetirementEvidence {
            endpoint: EndpointRetirementEvidence::Unpublished {
                publication: slot
                    .binding_receipt()
                    .and_then(AccountResourceBindingReceipt::publication)
                    .expect("published endpoint")
                    .clone(),
                removed: source.clone(),
            },
            actor: ActorUnregistrationEvidence::Unregistered {
                registration: slot
                    .binding_receipt()
                    .and_then(AccountResourceBindingReceipt::registration)
                    .expect("registered actor")
                    .clone(),
                removed: source.clone(),
            },
            directed_presence: DirectedPresenceTeardownReceipt::observed(
                source.clone(),
                snapshot_revision,
                DirectedPresenceTeardownDisposition::RecipientsRetractedAndUnavailableRecoveryOwned,
            ),
            presentation_flush: PresentationFlushReceipt::observed(
                source,
                snapshot_revision,
                PresentationFlushDisposition::RecoveryOwner,
            ),
            custody_disposition: BindingRetirementDisposition::AcceptedWorkHasRecoveryOwner,
        };
        complete_retirement(&mut slot, &claim_identity, &binding_ref, exact.clone());
        assert_eq!(
            slot.retired_binding_receipts()[0].retirement(),
            Some(&exact)
        );
        assert_eq!(
            slot.retired_binding_receipts()[0]
                .retirement_start()
                .expect("retirement start")
                .custody_disposition(),
            BindingRetirementDisposition::AcceptedWorkHasRecoveryOwner
        );
    }

    #[test]
    fn same_full_jid_replacement_fences_late_register_unregister_and_teardown() {
        let mut slot = slot();
        let claim_identity = slot.claim_identity().clone();
        let old_binding = binding(local_connection());
        bind_active(&mut slot, &claim_identity, &old_binding);
        begin_retirement(
            &mut slot,
            &claim_identity,
            &old_binding,
            BindingRetirementDisposition::NoAcceptedWork,
        );
        let old_retirement = retirement_evidence(
            &slot,
            BindingRetirementDisposition::NoAcceptedWork,
            DirectedPresenceTeardownDisposition::NoDirectedRecipients,
            PresentationFlushDisposition::NoPendingPresentation,
        );
        complete_retirement(
            &mut slot,
            &claim_identity,
            &old_binding,
            old_retirement.clone(),
        );

        assert_eq!(
            slot.full_jid().as_str(),
            "alice@example.test/phone",
            "the successor occupies the same exact full JID"
        );
        let successor_binding = binding(local_connection());
        bind_prepared(&mut slot, &claim_identity, &successor_binding);
        publish_endpoint(&mut slot, &claim_identity, &successor_binding);
        let successor_revision = slot.revision();

        reject(
            AccountResourceMutation::RegisterActor {
                claim_identity: claim_identity.clone(),
                expected_revision: successor_revision,
                binding_ref: old_binding.clone(),
                registration: ActorRegistrationReceipt::observed(
                    claim_identity.clone(),
                    BindingEvidence::for_binding(&old_binding),
                ),
            },
            &mut slot,
            AccountResourceTransitionError::BindingMismatch,
        );
        reject(
            AccountResourceMutation::CompleteRetirement {
                claim_identity: claim_identity.clone(),
                expected_revision: slot.revision(),
                binding_ref: old_binding.clone(),
                evidence: Box::new(old_retirement),
            },
            &mut slot,
            AccountResourceTransitionError::BindingMismatch,
        );
        reject(
            AccountResourceMutation::BeginRetirement {
                claim_identity: claim_identity.clone(),
                expected_revision: slot.revision(),
                binding_ref: old_binding.clone(),
                custody_disposition: BindingRetirementDisposition::NoAcceptedWork,
            },
            &mut slot,
            AccountResourceTransitionError::BindingMismatch,
        );
        assert_eq!(slot.binding_ref(), Some(&successor_binding));
        assert_eq!(
            slot.binding_receipt()
                .map(AccountResourceBindingReceipt::state),
            Some(AccountResourceBindingState::Published)
        );
        register_actor(&mut slot, &claim_identity, &successor_binding);
        assert_eq!(slot.binding_ref(), Some(&successor_binding));
    }

    #[test]
    fn stale_and_reused_bindings_and_claim_identities_cannot_be_revived() {
        let mut slot = slot();
        let original_claim = slot.claim_identity().clone();
        let old_binding = binding(local_connection());
        bind_active(&mut slot, &original_claim, &old_binding);
        begin_retirement(
            &mut slot,
            &original_claim,
            &old_binding,
            BindingRetirementDisposition::NoAcceptedWork,
        );
        let evidence = retirement_evidence(
            &slot,
            BindingRetirementDisposition::NoAcceptedWork,
            DirectedPresenceTeardownDisposition::NoDirectedRecipients,
            PresentationFlushDisposition::NoPendingPresentation,
        );
        complete_retirement(&mut slot, &original_claim, &old_binding, evidence);
        reject(
            AccountResourceMutation::Bind {
                claim_identity: original_claim.clone(),
                expected_revision: slot.revision(),
                binding_ref: old_binding.clone(),
            },
            &mut slot,
            AccountResourceTransitionError::BindingRefReused,
        );

        let fresh_binding = binding(local_connection());
        bind_active(&mut slot, &original_claim, &fresh_binding);
        let successor_claim = claim("node-b", "epoch-b", 2);
        commit(
            AccountResourceMutation::ReplaceClaim {
                claim_identity: original_claim.clone(),
                expected_revision: slot.revision(),
                binding_ref: fresh_binding.clone(),
                replacement_claim_identity: successor_claim.clone(),
            },
            &mut slot,
        );
        reject(
            AccountResourceMutation::ReplaceClaim {
                claim_identity: successor_claim.clone(),
                expected_revision: slot.revision(),
                binding_ref: fresh_binding,
                replacement_claim_identity: original_claim,
            },
            &mut slot,
            AccountResourceTransitionError::ClaimIdentityReused,
        );
    }

    #[test]
    fn operation_journal_handles_unknown_replay_payload_conflict_and_capacity() {
        let mut slot = slot();
        let claim_identity = slot.claim_identity().clone();
        let binding_ref = binding(local_connection());
        let bind = operation(AccountResourceMutation::Bind {
            claim_identity: claim_identity.clone(),
            expected_revision: slot.revision(),
            binding_ref: binding_ref.clone(),
        });
        assert_eq!(slot.lookup(&bind), AccountResourceOperationLookup::Unknown);
        let committed = slot.execute(bind.clone()).expect("bind resource");
        let committed_revision = match committed {
            AccountResourceExecution::Applied(AccountResourceOperationOutcome::Committed {
                revision,
            }) => revision,
            other => panic!("expected bind commit, got {other:?}"),
        };
        assert_eq!(
            slot.lookup(&bind),
            AccountResourceOperationLookup::Recorded(&AccountResourceOperationOutcome::Committed {
                revision: committed_revision,
            })
        );
        assert_eq!(
            slot.execute(bind.clone()).expect("replay bind"),
            AccountResourceExecution::Replayed(AccountResourceOperationOutcome::Committed {
                revision: committed_revision,
            })
        );

        let conflicting = AccountResourceOperation::new(
            bind.id,
            AccountResourceMutation::Bind {
                claim_identity: claim_identity.clone(),
                expected_revision: AccountResourceRevision::INITIAL,
                binding_ref: binding(local_connection()),
            },
        );
        assert_eq!(
            slot.lookup(&conflicting),
            AccountResourceOperationLookup::PayloadMismatch
        );
        assert_eq!(
            slot.execute(conflicting),
            Err(AccountResourceExecuteError::OperationIdConflict)
        );

        let mut last_recorded = bind;
        while slot.journal.len() < ACCOUNT_RESOURCE_OPERATION_CAPACITY {
            last_recorded = operation(AccountResourceMutation::UpdateSnapshot {
                claim_identity: claim_identity.clone(),
                expected_revision: slot.revision(),
                binding_ref: binding_ref.clone(),
                snapshot: AccountResourceSnapshot::default(),
            });
            assert_eq!(
                slot.execute(last_recorded.clone()),
                Ok(AccountResourceExecution::Applied(
                    AccountResourceOperationOutcome::Rejected(
                        AccountResourceTransitionError::LifecycleConflict,
                    ),
                ))
            );
        }
        let at_capacity = slot.revision();
        let refused = operation(AccountResourceMutation::Bind {
            claim_identity,
            expected_revision: at_capacity,
            binding_ref: binding(local_connection()),
        });
        assert_eq!(
            slot.execute(refused.clone()),
            Err(AccountResourceExecuteError::JournalFull)
        );
        assert_eq!(slot.revision(), at_capacity);
        assert_eq!(
            slot.lookup(&refused),
            AccountResourceOperationLookup::Unknown
        );
        assert!(matches!(
            slot.execute(last_recorded).expect("replay retained result"),
            AccountResourceExecution::Replayed(AccountResourceOperationOutcome::Rejected(
                AccountResourceTransitionError::LifecycleConflict
            ))
        ));
    }

    #[test]
    fn revision_exhaustion_fails_closed_without_partial_snapshot_update() {
        let mut slot = slot();
        let claim_identity = slot.claim_identity().clone();
        let binding_ref = binding(local_connection());
        bind_active(&mut slot, &claim_identity, &binding_ref);
        let previous = sample_snapshot();
        commit(
            AccountResourceMutation::UpdateSnapshot {
                claim_identity: claim_identity.clone(),
                expected_revision: slot.revision(),
                binding_ref: binding_ref.clone(),
                snapshot: previous.clone(),
            },
            &mut slot,
        );
        slot.revision = AccountResourceRevision(u64::MAX);
        let next = snapshot(
            false,
            None,
            None,
            -1,
            Vec::new(),
            false,
            false,
            false,
            previous.stream_management_ref.clone(),
        );
        reject(
            AccountResourceMutation::UpdateSnapshot {
                claim_identity,
                expected_revision: slot.revision(),
                binding_ref,
                snapshot: next,
            },
            &mut slot,
            AccountResourceTransitionError::RevisionExhausted,
        );
        assert_eq!(slot.revision(), AccountResourceRevision(u64::MAX));
        assert_eq!(slot.snapshot(), Some(&previous));
    }
}
