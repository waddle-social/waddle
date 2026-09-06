use super::super::{CarbonKind, OrderedRelayRouteOrigin};
use jid::{BareJid, FullJid};
use waddle_xmpp::{pending_delivery::PendingRow, telemetry::call::PendingCallSetupRoute, Stanza};
use xmpp_parsers::message::Message;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PeerDeliveryKind {
    PeerStanza,
    DirectFrame,
}

/// Delivery obligations are executed after the ingress transaction commits.
/// Relay and detached obligations capture their recipient SM append identities
/// after execution, when the actual receiving stream is known.
#[derive(Debug, Clone)]
pub enum ExternalDeliveryEffect {
    UndeliverableBounce {
        reply: Box<Stanza>,
    },
    SfuRevokeToken {
        call_id: waddle_sfu::CallId,
        identity: waddle_sfu::Identity,
        jti: waddle_sfu::Jti,
    },
    RouteToPeer {
        jid: FullJid,
        stanza: Box<Stanza>,
        kind: PeerDeliveryKind,
        call_setup: Option<PendingCallSetupRoute>,
    },
    QueueDetached {
        call_setup: Option<PendingCallSetupRoute>,
        bare: BareJid,
        resources: Vec<FullJid>,
        stanza: Box<Stanza>,
    },
    RelayFullJid {
        origin: Option<OrderedRelayRouteOrigin>,
        target: FullJid,
        stanza: Box<Stanza>,
        call_setup: Option<PendingCallSetupRoute>,
    },
    RelayBareJid {
        origin: Option<OrderedRelayRouteOrigin>,
        target: BareJid,
        stanza: Box<Stanza>,
    },
    RelayCarbons {
        origin: Option<OrderedRelayRouteOrigin>,
        owner: BareJid,
        exclude: Vec<FullJid>,
        message: Box<Message>,
        kind: CarbonKind,
    },
    Carbons {
        owner: BareJid,
        exclude: Vec<FullJid>,
        message: Box<Message>,
        kind: CarbonKind,
    },
    QueueOfflineDelivery {
        prepared_notification: PreparedOfflineNotification,
        row: PendingRow,
        original_message: Box<Message>,
    },
}

/// Frozen T0 decision; post-commit insertion does not recompute message policy.
#[derive(Debug, Clone)]
pub enum PreparedOfflineNotification {
    Prepared(Box<crate::notification_outbox::NotificationCandidate>),
    Suppressed,
    RetryLater,
}

/// Preserve archive references at every delivery boundary, including replay
/// rows whose archive reference is not carried in the original sender stanza.
pub(crate) fn record(deps: &super::super::Deps<'_>, effect: ExternalDeliveryEffect) {
    use super::{Effect, ExternalEffect, PlanEffectDependency, PlannedEffect};
    let mut dependencies = Vec::new();
    let message = match &effect {
        ExternalDeliveryEffect::RouteToPeer { stanza, .. }
        | ExternalDeliveryEffect::QueueDetached { stanza, .. }
        | ExternalDeliveryEffect::RelayFullJid { stanza, .. }
        | ExternalDeliveryEffect::RelayBareJid { stanza, .. } => match stanza.as_ref() {
            Stanza::Message(message) => Some(message),
            _ => None,
        },
        ExternalDeliveryEffect::Carbons { message, .. }
        | ExternalDeliveryEffect::RelayCarbons { message, .. } => Some(message.as_ref()),
        ExternalDeliveryEffect::QueueOfflineDelivery { row, .. } => {
            if let waddle_xmpp::pending_delivery::PendingPayload::Archived(minted) = &row.payload {
                dependencies.push(PlanEffectDependency::AfterArchive {
                    archive: row.recipient.clone(),
                    minted: minted.clone(),
                });
            }
            None
        }
        ExternalDeliveryEffect::UndeliverableBounce { .. }
        | ExternalDeliveryEffect::SfuRevokeToken { .. } => None,
    };
    if let Some(message) = message {
        dependencies.extend(
            waddle_xmpp::xep::extract_stanza_ids(message)
                .into_iter()
                .map(|minted| PlanEffectDependency::AfterArchive {
                    archive: minted.by.to_bare(),
                    minted,
                }),
        );
    }
    let suppression = match &effect {
        ExternalDeliveryEffect::Carbons { .. }
        | ExternalDeliveryEffect::RelayCarbons { .. }
        | ExternalDeliveryEffect::QueueOfflineDelivery { .. }
        | ExternalDeliveryEffect::QueueDetached { .. } => super::PlanSuppressionPolicy::SenderOnly,
        _ => super::PlanSuppressionPolicy::Always,
    };
    let mut planned = PlannedEffect::new(Effect::External(ExternalEffect::Delivery(effect)))
        .with_suppression(suppression);
    planned.dependencies = dependencies;
    deps.effects.record(planned);
}
