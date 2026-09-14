//! Durable authority asserted for each ingress producer.
use jid::BareJid;
use waddle_xmpp::auth::{AuthenticatedPrincipalRef, ExtensionGrantRef};

#[derive(Clone, Debug)]
pub struct ExtensionPrincipal {
    pub grant: ExtensionGrantRef,
    pub requester: Option<BareJid>,
    /// Effective sender: the requester for direct sends, the plugin actor for rooms.
    pub sender: BareJid,
}

#[derive(Clone, Debug)]
pub enum IngressPrincipal {
    Authenticated(AuthenticatedPrincipalRef),
    Extension(ExtensionPrincipal),
}

impl IngressPrincipal {
    pub fn bare_jid(&self) -> &BareJid {
        match self {
            Self::Authenticated(principal) => principal.bare_jid(),
            Self::Extension(principal) => &principal.sender,
        }
    }
}

pub(super) async fn assert_admission(
    tx: &mut crate::ingress_uow::IngressUowTransaction<'_>,
    submission: &super::submission::IngressSubmission,
) -> Result<(), crate::ingress_uow::IngressUowError> {
    use super::identity::IngressStreamIdentity;
    use crate::ingress_uow::{
        ExtensionGrantRepository, IngressUowError, PrincipalAssertion, PrincipalRepository,
    };
    use waddle_xmpp::{
        auth::ExtensionGrantScope,
        ingress::{NormalizedTarget, TransportGeneration},
    };
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
                IngressStreamIdentity::Extension { .. } => {
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
        _ => return Err(IngressUowError::PrincipalAssertionFailed),
    }
    Ok(())
}
