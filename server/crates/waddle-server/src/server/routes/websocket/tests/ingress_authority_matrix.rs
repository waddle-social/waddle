use super::*;
use crate::ingress::IngressDecisionClass;
use waddle_xmpp::ingress::ProtocolEpoch;

/// XEP-0198 §4 and §6: every uncommitted decision retains sender responsibility.
#[tokio::test]
async fn non_advancing_failure_matrix_keeps_wire_ack_and_rows_unchanged() {
    non_advancing_matrix_case(create_test_websocket_state().await).await;
}

/// XEP-0198 §4–§6: PostgreSQL failures retain sender responsibility and unchanged rows.
#[tokio::test]
async fn non_advancing_failure_matrix_keeps_wire_ack_and_rows_unchanged_postgres() {
    postgres_case(non_advancing_matrix_case).await;
}

async fn non_advancing_matrix_case(state: Arc<WebSocketState>) {
    let failures = [
        (
            IngressUowError::PrincipalAssertionFailed,
            IngressDecisionClass::PrincipalMissing,
        ),
        #[cfg(feature = "clustering")]
        (
            IngressUowError::ClaimFenceMissing,
            IngressDecisionClass::ClaimFenceMissing,
        ),
        (
            IngressUowError::RoomGenerationStale,
            IngressDecisionClass::RoomGenerationStale,
        ),
        (
            IngressUowError::IngressFrontierStale,
            IngressDecisionClass::FrontierStale,
        ),
        (
            IngressUowError::EffectIntentConflict,
            IngressDecisionClass::IntentContradiction,
        ),
        (
            IngressUowError::EffectIntentMessageMissing,
            IngressDecisionClass::Storage,
        ),
        (IngressUowError::Timeout, IngressDecisionClass::Timeout),
        (
            IngressUowError::AmbiguousCommit,
            IngressDecisionClass::AmbiguousCommit,
        ),
        (
            IngressUowError::EpochUnsupported {
                live: ProtocolEpoch::from_storage(1),
                supported: ProtocolEpoch::ZERO,
            },
            IngressDecisionClass::EpochUnsupported,
        ),
        (
            IngressUowError::Substrate(
                crate::ingress_substrate::IngressSubstrateError::SmOrdinalConflict,
            ),
            IngressDecisionClass::SmOrdinalConflict,
        ),
        (
            IngressUowError::Lineage(crate::db::DatabaseError::ConnectionFailed(
                "injected lineage failure".into(),
            )),
            IngressDecisionClass::Lineage,
        ),
    ];
    for (failure, expected) in failures {
        let mut conn = connection(&state, true).await;
        assert_ack(&state, &mut conn, 0).await;
        let observed = forced_failures::FAILURE
            .scope(
                std::cell::RefCell::new(Some(failure)),
                observed_dispatch(&state, &mut conn, &offered_message()),
            )
            .await;
        assert_eq!(observed, expected);
        assert!(!observed.advances());
        assert!(conn.sm_inbound_completion.has_unhandled_hole());
        assert!(conn.sm_state.is_resumable());
        assert_ack(&state, &mut conn, 0).await;
        for table in [
            "ingress_messages",
            "ingress_sm_refs",
            "ingress_effect_intents",
            "ingress_effect_receipts",
        ] {
            assert_eq!(row_count(&state, table).await, 0, "{table}");
        }
    }
}
