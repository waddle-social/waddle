use super::*;

pub(super) async fn queue_offline_delivery(
    deps: &Deps<'_>,
    recipient: BareJid,
    payload: waddle_xmpp::pending_delivery::PendingPayload,
    original_receipt_at: chrono::DateTime<chrono::Utc>,
    original_message: Box<Message>,
) {
    // XEP-0160 §3 step 2/4 — persist for later delivery.
    // The classifier and OfflineDeliveryHandler have already
    // applied XEP-0160 §4 type rules and the XEP-0334 hint
    // matrix; here we just write the row.
    let Some(storage) = deps.pending_delivery_storage else {
        warn!(
            recipient = %recipient,
            "QueueOfflineDelivery emitted but pending_delivery_storage is not wired; \
             dropping (test fixture or unwired deployment)"
        );
        return;
    };
    let row = waddle_xmpp::pending_delivery::PendingRow {
        id: waddle_xmpp::pending_delivery::PendingRowId::fresh(),
        recipient: recipient.clone(),
        original_receipt_at,
        payload,
        flushed_in_session: None,
        outbound_sequence: None,
    };
    let pending_row_id = row.id.clone();
    match storage.insert(row).await {
        Ok(waddle_xmpp::pending_delivery::InsertOutcome::Inserted) => {
            debug!(
                recipient = %recipient,
                "pending_delivery row inserted"
            );
            publish_xep0357_notifications(deps, &recipient, &pending_row_id, &original_message)
                .await;
        }
        Ok(waddle_xmpp::pending_delivery::InsertOutcome::QuotaExceeded) => {
            waddle_xmpp::prometheus::increment_pending_delivery_quota_exceeded();
            // XEP-0160 §3 step 3 + RFC 6120 §8.3 — return a
            // typed `<service-unavailable/>` bounce that
            // echoes the original payload (RFC 6120 §8.3.4
            // convention).
            //
            // **Known partial inconsistency**: ArchiveHandler
            // runs earlier in the chain than
            // OfflineDeliveryHandler, so by the time we get
            // here the message is already in MAM. Sender
            // sees `<service-unavailable/>` while the
            // recipient can still pull the message from MAM
            // catch-up on next reconnect — i.e. the bounce
            // is for the *live-delivery* obligation, not
            // for archival visibility.
            //
            // This matches every existing reference XMPP
            // server (Prosody, ejabberd) and is consistent
            // with XEP-0160 §3 step 3's narrow scope
            // ("offline message queue is full"). The
            // alternative — un-archiving on quota — would
            // race with concurrent MAM queries and break
            // XEP-0313's monotonic-archive invariant.
            let error = xmpp_parsers::stanza_error::StanzaError::new(
                xmpp_parsers::stanza_error::ErrorType::Cancel,
                xmpp_parsers::stanza_error::DefinedCondition::ServiceUnavailable,
                "en",
                "Recipient's offline message queue is full",
            );
            let bounce = waddle_xmpp::protocol::handlers::errors::message_error_reply(
                &original_message,
                error,
            );
            let sender_jid = match bounce.to.clone() {
                Some(j) => j,
                None => {
                    warn!(
                        recipient = %recipient,
                        "bounce target JID missing; dropping bounce"
                    );
                    return;
                }
            };
            let bounce_stanza = waddle_xmpp::Stanza::Message(bounce);
            let mut delivered = false;
            match sender_jid.clone().try_into_full() {
                Ok(full) => {
                    if matches!(
                        deps.connection_registry.send_to(&full, bounce_stanza).await,
                        waddle_xmpp::registry::SendResult::Sent
                    ) {
                        delivered = true;
                    }
                }
                Err(bare) => {
                    for full in deps.connection_registry.get_resources_for_user(&bare) {
                        if matches!(
                            deps.connection_registry
                                .send_to(&full, bounce_stanza.clone())
                                .await,
                            waddle_xmpp::registry::SendResult::Sent
                        ) {
                            delivered = true;
                        }
                    }
                }
            }
            if delivered {
                warn!(
                    recipient = %recipient,
                    sender = %sender_jid,
                    "pending_delivery quota exceeded — bounced \
                     <service-unavailable/> to sender per XEP-0160 §3 step 3"
                );
            } else {
                // Sender is remote (cross-domain) or has no
                // resources currently bound. S2S routing of
                // the bounce is out of scope today; surface
                // the conformance gap loudly so it shows up
                // in deployment logs.
                warn!(
                    recipient = %recipient,
                    sender = %sender_jid,
                    "pending_delivery quota exceeded but \
                     <service-unavailable/> bounce was not \
                     deliverable (remote sender or no bound \
                     resource) — XEP-0160 §3 step 3 \
                     conformance gap until s2s lands"
                );
            }
        }
        Err(error) => {
            warn!(
                recipient = %recipient,
                error = %error,
                "pending_delivery insert failed"
            );
        }
    }
}

async fn publish_xep0357_notifications(
    deps: &Deps<'_>,
    recipient: &BareJid,
    pending_row_id: &waddle_xmpp::pending_delivery::PendingRowId,
    original_message: &Message,
) {
    let Some(state) = deps.web_socket_state else {
        return;
    };
    // XEP-0492 gate: consult the recipient's per-conversation notification
    // level (defaulting to XEP-0492 conversation-kind defaults via the
    // projection store) and the XEP-0513 mention bit. The decision is a
    // typed `PushDispatchDecision` — never a stringly-typed diagnostic —
    // so the suppression reason flows through to the typed log line.
    let decision =
        evaluate_xep0492_push_dispatch_decision(state, recipient, original_message).await;
    match decision {
        crate::notification_settings_projection::PushDispatchDecision::Deliver => {}
        crate::notification_settings_projection::PushDispatchDecision::Suppressed { reason } => {
            info!(
                recipient = %recipient,
                sender = ?original_message.from.as_ref().map(|jid| jid.to_bare()),
                reason = %reason,
                "XEP-0492 push gate suppressed XEP-0357 push fan-out"
            );
            return;
        }
    }
    let registrations = match state
        .deps
        .protocol
        .push_store
        .get_for_user(&recipient.to_string())
        .await
    {
        Ok(registrations) => registrations,
        Err(error) => {
            warn!(
                recipient = %recipient,
                error = %error,
                "XEP-0357 push registration lookup failed after pending_delivery insert"
            );
            return;
        }
    };
    let first_party_service = state.deps.service_domains.push.as_str();
    for registration in registrations {
        if registration.service_jid != first_party_service {
            debug!(
                recipient = %recipient,
                service = %registration.service_jid,
                "XEP-0357 external Push Service publish is not wired in this first-party boundary"
            );
            continue;
        }
        let Some(node) = registration.node.as_deref() else {
            warn!(
                recipient = %recipient,
                service = %registration.service_jid,
                "first-party XEP-0357 registration missing node; skipping publish"
            );
            continue;
        };
        let notification =
            minidom::Element::builder("notification", waddle_xmpp::xep::xep0357::NS_PUSH).build();
        let item = waddle_xmpp::pubsub::PubSubItem::new(
            Some(pending_row_id.as_str().to_string()),
            Some(notification),
        );
        let iq = match build_xep0357_pubsub_publish_iq(
            first_party_service,
            recipient,
            node,
            pending_row_id,
            &item,
            registration.publish_options.as_ref(),
        ) {
            Ok(iq) => iq,
            Err(error) => {
                warn!(
                    recipient = %recipient,
                    node,
                    error = %error,
                    "XEP-0357 first-party Push Service notification IQ build failed"
                );
                continue;
            }
        };
        match state
            .deps
            .protocol
            .push_service
            .publish_xep0357_pubsub_iq_from_user_server(first_party_service, &iq, recipient)
            .await
        {
            Ok(result) => {
                debug!(
                    recipient = %recipient,
                    node,
                    item_id = %result.item_id(),
                    attempted_devices = result.attempted_devices(),
                    "XEP-0357 first-party Push Service notification published"
                );
            }
            Err(error) => {
                warn!(
                    recipient = %recipient,
                    node,
                    error = %error,
                    "XEP-0357 first-party Push Service notification publish failed"
                );
            }
        }
    }
}

/// Resolve the XEP-0492 push-dispatch gate for a single inbound DM that
/// is about to be projected into the recipient's offline queue.
///
/// The gate combines the recipient's typed
/// [`waddle_xmpp::xep::NotificationLevel`] (resolved by the
/// `NotificationSettingsProjectionStore`, falling back to the XEP-0492
/// conversation-kind defaults) with the XEP-0513 mention bit derived
/// directly from the inbound `<message>` payloads. Both inputs flow as
/// typed values; there are no string-typed payloads on the gate boundary.
///
/// `QueueOfflineDelivery` only fires for DM intake
/// ([`waddle_xmpp::protocol::handlers::offline_delivery::OfflineDeliveryHandler`]
/// is gated on `Locality::Recipient` + headless pass for `<message
/// type='chat'>`), so the conversation kind on this path is always
/// `ConversationKind::Direct`. The shared pure reducer
/// [`crate::notification_settings_projection::PushDispatchDecision::evaluate`]
/// is the single decision point — when MUC push fan-out lands it will
/// reuse the same reducer rather than re-implementing the level matrix.
async fn evaluate_xep0492_push_dispatch_decision(
    state: &WebSocketState,
    recipient: &BareJid,
    original_message: &Message,
) -> crate::notification_settings_projection::PushDispatchDecision {
    let Some(sender) = original_message.from.as_ref().map(|jid| jid.to_bare()) else {
        // Sender bare-JID missing — refuse to fan out because we cannot
        // resolve a per-conversation setting. Treated as suppression with
        // the strictest typed reason so the audit log is unambiguous.
        return crate::notification_settings_projection::PushDispatchDecision::Suppressed {
            reason: waddle_xmpp::xep::NotificationLevel::Never,
        };
    };
    if sender == *recipient {
        // Self-DM: never push to your own offline queue. Per XEP-0492
        // semantics this is a hard suppression independent of the
        // configured level; surface it as `Never` to keep the typed log
        // path uniform.
        return crate::notification_settings_projection::PushDispatchDecision::Suppressed {
            reason: waddle_xmpp::xep::NotificationLevel::Never,
        };
    }
    let level = match state
        .deps
        .protocol
        .notification_settings_projection
        .effective_setting(
            recipient,
            &sender,
            crate::notification_settings_projection::ConversationKind::Direct,
        )
        .await
    {
        Ok(level) => level,
        Err(error) => {
            warn!(
                recipient = %recipient,
                conversation = %sender,
                error = %error,
                "XEP-0492 notification setting lookup failed; suppressing push notification"
            );
            return crate::notification_settings_projection::PushDispatchDecision::Suppressed {
                reason: waddle_xmpp::xep::NotificationLevel::Never,
            };
        }
    };
    let is_mention = message_is_mention_for_recipient(original_message, recipient);
    crate::notification_settings_projection::PushDispatchDecision::evaluate(level, is_mention)
}

/// Returns `true` when the inbound XEP-0513 explicit-mention payloads
/// name `recipient` as a mentioned `<mention jid='…'/>`.
///
/// The recipient JID is the bare JID that owns the offline queue; that
/// is the canonical identity referenced by `<mention jid='…'/>` per
/// XEP-0513 §3. Channel-wide `<mention mentions='urn:xmpp:mentions:0#channel'/>`
/// is intentionally NOT treated as an individual mention here — the
/// XEP-0492 `<on-mention/>` semantics target explicit user mentions; the
/// channel-mention surface is for MUC reflector announcements, which do
/// not flow through the DM `QueueOfflineDelivery` arm.
fn message_is_mention_for_recipient(message: &Message, recipient: &BareJid) -> bool {
    waddle_xmpp::xep::extract_explicit_mentions(message)
        .is_some_and(|mentions| mentions.mentions_jid(recipient))
}

fn build_xep0357_pubsub_publish_iq(
    push_service_jid: &str,
    recipient: &BareJid,
    node: &str,
    pending_row_id: &waddle_xmpp::pending_delivery::PendingRowId,
    item: &waddle_xmpp::pubsub::PubSubItem,
    publish_options: Option<&minidom::Element>,
) -> Result<xmpp_parsers::iq::Iq, jid::Error> {
    let publish = minidom::Element::builder("publish", waddle_xmpp::pubsub::NS_PUBSUB)
        .attr("node", node)
        .append(item.to_element(waddle_xmpp::pubsub::NS_PUBSUB))
        .build();
    let mut pubsub_builder =
        minidom::Element::builder("pubsub", waddle_xmpp::pubsub::NS_PUBSUB).append(publish);
    if let Some(publish_options) = publish_options {
        pubsub_builder = pubsub_builder.append(
            minidom::Element::builder("publish-options", waddle_xmpp::pubsub::NS_PUBSUB)
                .append(publish_options.clone())
                .build(),
        );
    }
    Ok(xmpp_parsers::iq::Iq {
        from: Some(recipient.clone().into()),
        to: Some(push_service_jid.parse()?),
        id: format!("push-{}", pending_row_id.as_str()),
        payload: xmpp_parsers::iq::IqType::Set(pubsub_builder.build()),
    })
}
