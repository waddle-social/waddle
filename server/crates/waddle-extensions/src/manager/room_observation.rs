use super::*;
use crate::types::{
    ConfiguredRoomObserver, ObservationFailure, ObservationSkip, RoomMessageObserve,
    RoomMessageSource, RoomObservationOutcome, RoomObservationResult, RoomObservationSubscription,
};

impl ExtensionManager {
    pub fn configured_room_observers(&self) -> Vec<ConfiguredRoomObserver> {
        self.actors
            .iter()
            .filter_map(|actor| actor.room_observer().cloned())
            .collect()
    }

    /// The caller supplies an authoritative hosted room, never an untrusted tenant label.
    pub fn room_observation_subscriptions(
        &self,
        room: &BareJid,
    ) -> Vec<RoomObservationSubscription> {
        self.configured_room_observers()
            .into_iter()
            .filter(|observer| observer.scope.includes(room))
            .map(|observer| RoomObservationSubscription {
                plugin: observer.plugin,
                generation: observer.generation,
                identity: observer.identity,
                room: room.clone(),
            })
            .collect()
    }

    pub async fn observe_room_message(
        &self,
        subscription: &RoomObservationSubscription,
        source: RoomMessageSource,
        body: DisplayText,
    ) -> RoomObservationOutcome {
        let Some(actor) = self.actors.iter().find(|actor| {
            actor.room_observer().is_some_and(|current| {
                current.plugin == subscription.plugin
                    && current.generation == subscription.generation
                    && current.identity == subscription.identity
                    && current.scope.includes(&subscription.room)
            })
        }) else {
            return RoomObservationOutcome::NotApplicable(ObservationSkip::SubscriptionUnavailable);
        };
        if source.room != subscription.room
            || source.body_digest.as_str() != hex::encode(Sha256::digest(body.as_str().as_bytes()))
            || chrono::DateTime::parse_from_rfc3339(source.observed_at.as_str()).is_err()
        {
            return RoomObservationOutcome::PermanentFailure(ObservationFailure::SourceMismatch);
        }
        if source
            .origin_id
            .as_ref()
            .is_none_or(|id| id.as_str().trim().is_empty())
        {
            return RoomObservationOutcome::NotApplicable(ObservationSkip::MissingOriginId);
        }
        match actor
            .observe_room_message(RoomMessageObserve { source, body })
            .await
        {
            Ok(response) => {
                validated_observation_result(response, |effect| actor.validate_effect(effect))
            }
            Err(
                error @ (ObservationFailure::TemporaryFailure
                | ObservationFailure::DeadlineExceeded),
            ) => RoomObservationOutcome::RetryableFailure(error),
            Err(error) => RoomObservationOutcome::PermanentFailure(error),
        }
    }
}

fn validated_observation_result(
    response: crate::types::ExtensionResponse,
    validate: impl Fn(&ExtensionEffect) -> bool,
) -> RoomObservationOutcome {
    let mut payloads = Vec::new();
    for effect in response.effects {
        if !validate(&effect) {
            return RoomObservationOutcome::PermanentFailure(ObservationFailure::InvalidResult);
        }
        match effect {
            ExtensionEffect::PublishRoomResult(payload) if payloads.is_empty() => {
                payloads.push(payload)
            }
            ExtensionEffect::Noop => {}
            _ => {
                return RoomObservationOutcome::PermanentFailure(ObservationFailure::InvalidResult)
            }
        }
    }
    RoomObservationOutcome::Completed(RoomObservationResult {
        payloads,
        usage: response.usage,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{ExtensionPayload, ExtensionResponse, PayloadNamespace, XmlElement};

    fn result_effect() -> ExtensionEffect {
        let namespace = PayloadNamespace::new("urn:test:room-result").expect("namespace");
        ExtensionEffect::PublishRoomResult(
            ExtensionPayload::new(
                namespace.clone(),
                XmlElement::new(namespace, "result", vec![], vec![]).expect("element"),
            )
            .expect("payload"),
        )
    }

    #[test]
    fn observation_outputs_reject_undeclared_results_and_unrelated_effects() {
        let response = |effects| ExtensionResponse {
            effects,
            usage: None,
        };
        assert_eq!(
            validated_observation_result(response(vec![result_effect()]), |_| false),
            RoomObservationOutcome::PermanentFailure(ObservationFailure::InvalidResult)
        );
        assert_eq!(
            validated_observation_result(
                response(vec![ExtensionEffect::HostWarning(
                    DisplayText::new("guest error").expect("text")
                )]),
                |_| true
            ),
            RoomObservationOutcome::PermanentFailure(ObservationFailure::InvalidResult)
        );
        assert_eq!(
            validated_observation_result(response(vec![result_effect(), result_effect()]), |_| {
                true
            }),
            RoomObservationOutcome::PermanentFailure(ObservationFailure::InvalidResult)
        );
        assert!(
            matches!(validated_observation_result(response(vec![result_effect()]), |_| true),
            RoomObservationOutcome::Completed(RoomObservationResult { payloads, .. }) if payloads.len() == 1)
        );
    }
}
