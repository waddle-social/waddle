//! Pure replacement of provisional archive identities with committed XEP-0359 IDs.
use crate::notification_outbox::NotificationCandidate;
mod intents;
#[cfg(test)]
mod tests;

use crate::ingress_uow::IngressUowError;
use crate::server::routes::interpret::effects::delivery::{
    ExternalDeliveryEffect, PreparedOfflineNotification,
};
use crate::server::routes::interpret::effects::direct::{
    DurableDirectEffect, ExternalDirectEffect,
};
use crate::server::routes::interpret::effects::room::{DurableRoomEffect, ExternalRoomEffect};
use crate::server::routes::interpret::effects::{
    DurableEffect, Effect, ExternalEffect, IngressPlan, PlanEffectDependency,
};
use jid::BareJid;
use waddle_xmpp::inbox::{ConversationKind, InboxEntry};
use waddle_xmpp::ingress::{ArchiveRole, IngressEffectIntent};
use waddle_xmpp::mam::ArchivedMessage;
use waddle_xmpp::pending_delivery::PendingPayload;
use waddle_xmpp::Stanza;
use waddle_xmpp_core::xep0359::{StanzaId, NS_SID};
use xmpp_parsers::message::Message;

mod implementation;
pub use implementation::restamp_plan;
pub(super) use implementation::restore_route_identities;
use implementation::Replacements;
