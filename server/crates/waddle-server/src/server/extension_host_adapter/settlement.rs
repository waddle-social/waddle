//! The host consumes sender frames while authority-owned settlement continues.
use std::time::Duration;

use crate::ingress::{
    nested::{NestedOutcome, NestedRefusal},
    IngressDecisionClass,
};

use super::ExtensionHostAdapterError;

const SETTLEMENT_RESPONSE_DEADLINE: Duration = Duration::from_secs(2);

pub(super) async fn finish_nested(
    outcome: NestedOutcome,
) -> Result<Vec<(jid::BareJid, waddle_xmpp_core::xep0359::StanzaId)>, ExtensionHostAdapterError> {
    match outcome {
        NestedOutcome::Refused(NestedRefusal::Decision(IngressDecisionClass::PrincipalMissing)) => {
            Err(ExtensionHostAdapterError::NotAuthorized)
        }
        NestedOutcome::Refused(reason) => Err(ExtensionHostAdapterError::Storage(format!(
            "nested ingress refused: {reason:?}"
        ))),
        NestedOutcome::Committed {
            settlement,
            archive_ids,
            ..
        } => {
            // A dropped waiter cannot cancel the authority's task. Once committed,
            // timeout or persistence failure means acceptance, never a retry request.
            if let Ok(Ok(outcome)) =
                tokio::time::timeout(SETTLEMENT_RESPONSE_DEADLINE, settlement).await
            {
                if outcome.terminal.is_ok() {
                    if let Some(rejection) = outcome.rejection {
                        return Err(ExtensionHostAdapterError::Rejected(Box::new(rejection)));
                    }
                }
            }
            Ok(archive_ids)
        }
    }
}
