//! Durable authority asserted for each ingress producer.
use crate::ingress_uow::IngressUowError;
use crate::ingress_uow::IngressUowTransaction;
use crate::ingress_uow::RoomPublication;
use crate::permissions::host_grant::{PrincipalEvidence, StreamEvidence};
use jid::BareJid;
use waddle_xmpp::auth::{AuthenticatedPrincipalRef, ExtensionGrantRef};

#[derive(Clone, Debug)]
pub struct ExtensionPrincipal {
    pub grant: ExtensionGrantRef,
    pub requester: Option<BareJid>,
    /// Effective sender: the requester for direct sends, the plugin actor for rooms.
    pub sender: BareJid,
}

/// An account at a domain this deployment does not serve, whose stanza an
/// inbound S2S stream carried to a local room, as an occupant (ADR-0020
/// D10), or to a local account, as the sender of a direct message (D7).
///
/// It names the account and what its stream proved, and nothing else: no
/// session, credential or grant. Whether its domain is admitted, and what
/// its grants are honoured as, is decided in the acceptance transaction
/// from the S2S snapshot the replica runs.
#[derive(Clone, Debug)]
pub struct RemoteOccupantPrincipal {
    pub account: BareJid,
    pub evidence: StreamEvidence,
}

#[derive(Clone, Debug)]
pub enum IngressPrincipal {
    RoomResult(Box<RoomPublication>),
    Authenticated(AuthenticatedPrincipalRef),
    Extension(ExtensionPrincipal),
    Remote(RemoteOccupantPrincipal),
}

impl IngressPrincipal {
    pub fn bare_jid(&self) -> &BareJid {
        match self {
            Self::RoomResult(result) => &result.source.room,
            Self::Authenticated(principal) => principal.bare_jid(),
            Self::Extension(principal) => &principal.sender,
            Self::Remote(principal) => &principal.account,
        }
    }

    /// How the principal's stanza reached the acceptance: on an inbound S2S
    /// stream for a remote occupant, and without one for every producer of
    /// this deployment (a local session, an extension, a room's own
    /// result).
    pub fn evidence(&self) -> PrincipalEvidence {
        match self {
            Self::Remote(principal) => PrincipalEvidence::Stream(principal.evidence),
            Self::RoomResult(_) | Self::Authenticated(_) | Self::Extension(_) => {
                PrincipalEvidence::NoStream
            }
        }
    }
}

pub(super) async fn assert_admission(
    tx: &mut IngressUowTransaction<'_>,
    submission: &super::submission::IngressSubmission,
) -> Result<(), IngressUowError> {
    use super::identity::{IngressStreamIdentity, RemoteIngressTarget};
    use crate::ingress_uow::{
        ExtensionGrantRepository, IngressUowError, PrincipalAssertion, PrincipalRepository,
    };
    use crate::server::routes::interpret::effects::RoomExecutionPath;
    use waddle_xmpp::auth::ExtensionGrantScope;
    use waddle_xmpp::ingress::{NormalizedTarget, TransportGeneration};
    match (
        &submission.principal,
        &submission.identity,
        submission.connection_generation,
    ) {
        (
            IngressPrincipal::Authenticated(principal),
            identity,
            TransportGeneration::Connection(_),
        ) => {
            match identity {
                IngressStreamIdentity::Ephemeral {
                    principal: expected,
                } if principal != expected => {
                    return Err(IngressUowError::PrincipalAssertionFailed)
                }
                IngressStreamIdentity::Extension { .. }
                | IngressStreamIdentity::RoomResult { .. }
                | IngressStreamIdentity::Remote { .. } => {
                    return Err(IngressUowError::PrincipalAssertionFailed)
                }
                IngressStreamIdentity::Resumable { .. }
                | IngressStreamIdentity::Ephemeral { .. }
                | IngressStreamIdentity::Relayed { .. } => {}
            }
            if PrincipalRepository::assert_principal(tx, principal).await?
                != PrincipalAssertion::Asserted
            {
                return Err(IngressUowError::PrincipalAssertionFailed);
            }
        }
        (
            IngressPrincipal::Extension(principal),
            IngressStreamIdentity::Extension { plugin, requester },
            TransportGeneration::Host,
        ) => {
            if plugin != &principal.grant.plugin || requester != &principal.requester {
                return Err(IngressUowError::PrincipalAssertionFailed);
            }
            if let ExtensionGrantScope::ProviderRoom(room) = &principal.grant.scope {
                let matches = match &submission.target {
                    NormalizedTarget::Absent => false,
                    NormalizedTarget::Bare(target) => target == room,
                    NormalizedTarget::Full(target) => target.to_bare() == *room,
                };
                if !matches {
                    return Err(IngressUowError::PrincipalAssertionFailed);
                }
            }
            ExtensionGrantRepository::assert_grant(tx, &principal.grant).await?;
            if let Some(requester) = &principal.requester {
                ExtensionGrantRepository::assert_requester(tx, requester).await?;
            }
        }
        (
            IngressPrincipal::RoomResult(result),
            IngressStreamIdentity::RoomResult { id, room },
            TransportGeneration::Host,
        ) if id == &result.id
            && room == &result.source.room
            && result.subscription.room == *room
            && submission.sender.to_bare() == *room
            && submission.target == NormalizedTarget::Bare(room.clone()) =>
        {
            // Durable authority is checked after room/sequence locks in
            // apply_durable, and retained through the archive commit.
        }
        (
            IngressPrincipal::Remote(principal),
            IngressStreamIdentity::Remote {
                account,
                target: RemoteIngressTarget::Room,
            },
            TransportGeneration::Host,
        ) if account == &principal.account
            && submission.sender.to_bare() == principal.account
            && matches!(&submission.target, NormalizedTarget::Bare(_)) =>
        {
            // The D1 decision at the effect boundary: a domain the snapshot
            // this replica runs does not admit commits nothing, whatever
            // admitted its stream or its seat earlier. A managed room
            // decides the rest from its grants; any other room keeps its
            // own checks.
            if let Some(attachment) = tx.host_grant_attachment() {
                let witness = attachment.principals().witness(
                    &principal.account,
                    &PrincipalEvidence::Stream(principal.evidence),
                );
                if !witness.grants_honoured() {
                    return Err(IngressUowError::PrincipalAssertionFailed);
                }
            }
        }
        (
            IngressPrincipal::Remote(principal),
            IngressStreamIdentity::Remote {
                account,
                target: RemoteIngressTarget::Account { admission },
            },
            TransportGeneration::Host,
        ) if account == &principal.account
            && submission.sender.to_bare() == principal.account
            && matches!(submission.plan.room_execution, RoomExecutionPath::None) =>
        {
            assert_remote_direct_admission(tx, submission, principal, admission).await?;
        }
        _ => return Err(IngressUowError::PrincipalAssertionFailed),
    }
    Ok(())
}

/// The acceptance of a remote sender's direct message to a local account
/// (ADR-0020 D7; gate G18), decided in the acceptance transaction:
///
/// - D1 at the effect: the sender's domain is admitted under `admission`,
///   the S2S policy of the replica that built the submission. A domain no
///   longer admitted commits nothing, whatever admitted its stream earlier.
/// - The recipient named by the target, bare or full, is an account of this
///   deployment and its JID is not retired (`RemoteRecipientRepository`).
///
/// Every refusal is the same failed assertion, so the caller, which answers
/// a remote sender nothing whatever the outcome, cannot tell them apart
/// either.
async fn assert_remote_direct_admission(
    tx: &mut IngressUowTransaction<'_>,
    submission: &super::submission::IngressSubmission,
    principal: &RemoteOccupantPrincipal,
    admission: &crate::permissions::host_grant::PrincipalPolicy,
) -> Result<(), IngressUowError> {
    use crate::ingress_uow::{RecipientAssertion, RemoteRecipientRepository};
    use waddle_xmpp::ingress::NormalizedTarget;
    let witness = admission.witness(
        &principal.account,
        &PrincipalEvidence::Stream(principal.evidence),
    );
    if !witness
        .remote()
        .is_some_and(|remote| remote.admission().is_ok())
    {
        return Err(IngressUowError::PrincipalAssertionFailed);
    }
    let recipient = match &submission.target {
        NormalizedTarget::Bare(recipient) => recipient.clone(),
        NormalizedTarget::Full(recipient) => recipient.to_bare(),
        NormalizedTarget::Absent => return Err(IngressUowError::PrincipalAssertionFailed),
    };
    if recipient.node().is_none() || recipient == principal.account {
        return Err(IngressUowError::PrincipalAssertionFailed);
    }
    match RemoteRecipientRepository::assert_recipient(tx, &recipient).await? {
        RecipientAssertion::Asserted => Ok(()),
        RecipientAssertion::Unknown => Err(IngressUowError::PrincipalAssertionFailed),
    }
}
