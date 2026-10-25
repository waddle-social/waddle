//! Upgrade scheduler ancestry without treating an old transport ACK as delivery.
use super::{
    build_waddle_context, decode_candidate, BareJid, Database, Element, Jid, NotificationCandidate,
    NotificationClass, NotificationOutboxError, NotificationOutboxStore, NotificationThreadId, Row,
    StanzaId, SuppressedReason, STATUS_FAILED, STATUS_IN_PROGRESS, STATUS_PUBLISHED, STATUS_QUEUED,
};
use crate::db::{DatabaseDriver, Transaction};
use crate::ingress_uow::{run_with_retry, EffectDescendantRepository, IngressUowError};
use sha2::{Digest, Sha256};
use uuid::Uuid;
use waddle_xmpp::ingress::{IngressEffectIntent, MessageKey, NotificationActivityMutation};

#[path = "legacy_ancestry/quarantine.rs"]
mod quarantine;

#[cfg(test)]
#[path = "legacy_ancestry_tests.rs"]
mod tests;

mod implementation;
use implementation::begin_bounded;
#[cfg(test)]
use implementation::load_page;
use implementation::target;
#[cfg(test)]
use implementation::AdoptionPhase;
use implementation::Recorded;
