use super::*;

pub(super) async fn handle_archive_inbox_upload_iq(
    iq: &xmpp_parsers::iq::Iq,
    id: &str,
    payload_ns: &str,
    domain: &str,
    state: &WebSocketState,
    phase: &ConnectionPhase,
) -> Vec<String> {
    // MAM (Message Archive Management) query
    if is_mam_query(iq) {
        let request_iq = &iq;
        let Some(target) = request_iq.to.as_ref().map(|jid| jid.to_string()) else {
            return vec![build_iq_error_xml_typed(
                id,
                None,
                None,
                bad_request_iq_error("Malformed IQ payload."),
            )];
        };

        let room_target = target.split('/').next().unwrap_or(target.as_str());
        let Ok(target_bare) = room_target.parse::<BareJid>() else {
            return vec![build_iq_error_xml_typed(
                id,
                None,
                None,
                jid_malformed_iq_error("Malformed JID in IQ addressing."),
            )];
        };

        // Determine whether this is a personal archive query (to=self) or a
        // MUC room archive query. Personal queries are allowed only when the
        // bound session identity matches the requested bare JID.
        let sender_bare = phase.bound_jid().map(|jid| jid.to_bare());

        let is_personal = sender_bare
            .as_ref()
            .is_some_and(|bare| *bare == target_bare);

        if !is_personal && !is_muc_room_jid(state, &target_bare).await {
            return vec![build_iq_error_xml_typed(
                id,
                None,
                None,
                item_not_found_iq_error("Requested item not found."),
            )];
        }

        if is_mam_query_form_request(request_iq) {
            return vec![iq_to_xml(build_query_form_iq(request_iq))];
        }

        let (query_id, query) = match parse_mam_query(request_iq) {
            Ok(parsed) => parsed,
            Err(err) => {
                warn!(error = %err, target = %target_bare, "Invalid MAM query");
                if matches!(err, waddle_xmpp::CoreError::NotImplemented) {
                    return vec![build_iq_error_xml_typed(
                        id,
                        None,
                        None,
                        feature_not_implemented_iq_error("Requested feature not implemented."),
                    )];
                }
                return vec![build_iq_error_xml_typed(
                    id,
                    None,
                    None,
                    bad_request_iq_error("Malformed IQ payload."),
                )];
            }
        };

        let mut result = match state
            .deps
            .protocol
            .mam_storage
            .query_messages(&target_bare, &query)
            .await
        {
            Ok(result) => result,
            Err(waddle_xmpp::mam::MamStorageError::NotFound(_)) => {
                return vec![build_iq_error_xml_typed(
                    id,
                    None,
                    None,
                    item_not_found_iq_error("Requested item not found."),
                )];
            }
            Err(err) => {
                warn!(error = %err, target = %target_bare, "MAM query failed");
                return vec![build_iq_error_xml_typed(
                    id,
                    None,
                    None,
                    internal_server_error_iq_error("Internal server error."),
                )];
            }
        };

        if result.count.is_none() {
            result.count = state
                .deps
                .protocol
                .mam_storage
                .count_messages(&target_bare)
                .await
                .ok();
        }

        // XEP-0313 §5.1: result `<message/>` envelopes are addressed to
        // the requesting client. Prefer the IQ's `from` (the client JID
        // it stamped on the request) and fall back to the bound JID.
        // Both are typed `Jid` already; the prior `to_string()` /
        // `parse_message_jid` round-trip with an "unknown@localhost"
        // fallback was a hot-path data-loss bug for unauthenticated /
        // unbound edge cases. Reject the request here instead — a MAM
        // query without an addressable recipient is ill-formed.
        let Some(recipient_jid) = request_iq
            .from
            .clone()
            .or_else(|| phase.bound_jid().cloned().map(jid::Jid::from))
        else {
            return vec![build_iq_error_xml_typed(
                id,
                None,
                None,
                bad_request_iq_error("Malformed IQ payload."),
            )];
        };

        let mut responses: Vec<String> =
            build_result_messages(&query_id, &recipient_jid, &result.messages)
                .into_iter()
                .map(|message| stanza_to_xml(&Stanza::Message(message)))
                .collect();
        responses.push(iq_to_xml(build_fin_iq(request_iq, &result)));
        return responses;
    }

    if is_inbox_iq(iq) {
        let request_iq = &iq;
        let Some(user_jid) = phase.bound_jid().map(|jid| jid.to_bare()) else {
            return vec![build_iq_error_xml_typed(
                id,
                None,
                None,
                not_authorized_iq_error("Authentication required."),
            )];
        };

        match &request_iq.payload {
            xmpp_parsers::iq::IqType::Get(_) => {
                let query = match parse_inbox_query(request_iq) {
                    Ok(query) => query,
                    Err(error) => {
                        warn!(error = %error, "Invalid inbox query");
                        return vec![build_iq_error_xml_typed(
                            id,
                            None,
                            None,
                            bad_request_iq_error("Malformed IQ payload."),
                        )];
                    }
                };
                let entries = if query.threads {
                    if let Some(room) = &query.room {
                        match state
                            .deps
                            .protocol
                            .inbox_storage
                            .list_threads(&user_jid, room)
                            .await
                        {
                            Ok(entries) => entries,
                            Err(error) => {
                                warn!(error = %error, jid = %user_jid, "Failed to list thread inbox");
                                return vec![build_iq_error_xml_typed(
                                    id,
                                    None,
                                    None,
                                    internal_server_error_iq_error("Internal server error."),
                                )];
                            }
                        }
                    } else {
                        return vec![build_iq_error_xml_typed(
                            id,
                            None,
                            None,
                            bad_request_iq_error("Malformed IQ payload."),
                        )];
                    }
                } else {
                    match state.deps.protocol.inbox_storage.list(&user_jid).await {
                        Ok(entries) => entries,
                        Err(error) => {
                            warn!(error = %error, jid = %user_jid, "Failed to list inbox");
                            return vec![build_iq_error_xml_typed(
                                id,
                                None,
                                None,
                                internal_server_error_iq_error("Internal server error."),
                            )];
                        }
                    }
                };
                let total_unread = match state
                    .deps
                    .protocol
                    .inbox_storage
                    .total_unread(&user_jid)
                    .await
                {
                    Ok(total_unread) => total_unread,
                    Err(error) => {
                        warn!(error = %error, jid = %user_jid, "Failed to count inbox unread");
                        return vec![build_iq_error_xml_typed(
                            id,
                            None,
                            None,
                            internal_server_error_iq_error("Internal server error."),
                        )];
                    }
                };
                let response = build_inbox_query_result(
                    request_iq,
                    &filter_query(entries, &query),
                    total_unread,
                );
                return vec![iq_to_xml(response)];
            }
            xmpp_parsers::iq::IqType::Set(_) => {
                let mark_read = match parse_mark_read(request_iq) {
                    Ok(mark_read) => mark_read,
                    Err(error) => {
                        warn!(error = %error, "Invalid inbox mark-read");
                        return vec![build_iq_error_xml_typed(
                            id,
                            None,
                            None,
                            bad_request_iq_error("Malformed IQ payload."),
                        )];
                    }
                };
                let updated = match state
                    .deps
                    .protocol
                    .inbox_storage
                    .mark_read(
                        &user_jid,
                        &mark_read.partner,
                        mark_read.thread_id.as_deref(),
                    )
                    .await
                {
                    Ok(updated) => updated,
                    Err(error) => {
                        warn!(error = %error, jid = %user_jid, partner = %mark_read.partner, "Failed to mark inbox read");
                        return vec![build_iq_error_xml_typed(
                            id,
                            None,
                            None,
                            internal_server_error_iq_error("Internal server error."),
                        )];
                    }
                };
                // Cross-device sync: fan the post-update entry out to every
                // resource bound for this user so other devices clear their
                // unread badges without waiting for a fresh inbox query.
                if let Some(entry) = updated {
                    crate::server::routes::interpret::push_inbox_update(
                        state.deps.protocol.connection_registry.as_ref(),
                        &user_jid,
                        &entry,
                    )
                    .await;
                }
                return vec![iq_to_xml(build_mark_read_result(request_iq))];
            }
            _ => {
                return vec![build_iq_error_xml_typed(
                    id,
                    None,
                    None,
                    bad_request_iq_error("Malformed IQ payload."),
                )];
            }
        }
    }

    // urn:waddle:threads:0 — global threads view (PR #671).
    if crate::threads::handler::is_threads_iq(iq) {
        use crate::threads::handler::{handle_threads_iq, ThreadsIqOutcome};

        let request_iq = &iq;
        let Some(user_jid) = phase.bound_jid().map(|jid| jid.to_bare()) else {
            return vec![build_iq_error_xml_typed(
                id,
                None,
                None,
                not_authorized_iq_error("Authentication required."),
            )];
        };

        let outcome = handle_threads_iq(
            state.deps.protocol.threads_storage.as_ref(),
            request_iq,
            Some(&user_jid),
        )
        .await;

        return match outcome {
            ThreadsIqOutcome::Result(payload) => {
                let response = xmpp_parsers::iq::Iq {
                    from: request_iq.to.clone(),
                    to: request_iq.from.clone(),
                    id: request_iq.id.clone(),
                    payload: xmpp_parsers::iq::IqType::Result(Some(payload)),
                };
                vec![iq_to_xml(response)]
            }
            ThreadsIqOutcome::BadRequest(err) => {
                warn!(error = %err, "Invalid urn:waddle:threads:0 query");
                vec![build_iq_error_xml_typed(
                    id,
                    None,
                    None,
                    bad_request_iq_error("Malformed IQ payload."),
                )]
            }
            ThreadsIqOutcome::Forbidden => vec![build_iq_error_xml_typed(
                id,
                None,
                None,
                forbidden_iq_error("Threads query refused for cross-user target."),
            )],
            ThreadsIqOutcome::NotAuthorized => vec![build_iq_error_xml_typed(
                id,
                None,
                None,
                not_authorized_iq_error("Authentication required."),
            )],
            ThreadsIqOutcome::InternalError(error) => {
                warn!(error = %error, jid = %user_jid, "Failed to page threads");
                vec![build_iq_error_xml_typed(
                    id,
                    None,
                    None,
                    internal_server_error_iq_error("Internal server error."),
                )]
            }
        };
    }

    // urn:xmpp:carbons:2 enable/disable is now served by
    // protocol::handlers::carbons::CarbonsHandler via the short-circuit above.

    // XEP-0363: HTTP File Upload slot request
    if payload_ns == "urn:xmpp:http:upload:0" {
        let request_iq = &iq;
        if is_upload_request(request_iq) {
            let Some(sender_jid) = phase.bound_jid() else {
                return vec![build_iq_error_xml_typed(
                    id,
                    None,
                    None,
                    not_authorized_iq_error("Authentication required."),
                )];
            };
            let request = match parse_upload_request(request_iq) {
                Ok(req) => req,
                Err(e) => {
                    return vec![build_upload_error(id, &e)];
                }
            };

            // Check file size limits (default 10 MB). The helper also
            // caps operator configuration to the database BIGINT range
            // so accepted XEP-0363 sizes are stored losslessly.
            let max_size = crate::server::routes::uploads::max_upload_size();
            if request.size > max_size {
                return vec![build_upload_error(
                    id,
                    &UploadError::FileTooLarge { max_size },
                )];
            }
            let Some(size_bytes) = crate::server::routes::uploads::upload_size_to_i64(request.size)
            else {
                return vec![build_upload_error(
                    id,
                    &UploadError::FileTooLarge {
                        max_size: crate::server::routes::uploads::MAX_DATABASE_UPLOAD_SIZE,
                    },
                )];
            };

            let safe_filename = sanitize_filename(&request.filename);
            let content_type = effective_content_type(request.content_type.as_deref()).to_string();
            let slot_id = uuid::Uuid::new_v4().to_string();
            let expires_at = (chrono::Utc::now() + chrono::Duration::minutes(15)).to_rfc3339();

            let base_url =
                std::env::var("WADDLE_BASE_URL").unwrap_or_else(|_| format!("https://{}", domain));
            let base_url = base_url.trim_end_matches('/');
            let put_url = format!("{}/api/upload/{}", base_url, slot_id);
            let get_url = format!("{}/api/files/{}/{}", base_url, slot_id, safe_filename);

            if let Err(e) = state
                .deps
                .app_state
                .db_pool
                .global_actor()
                .clone()
                .ask(DbExecute {
                    sql: "INSERT INTO upload_slots (id, requester_jid, filename, size_bytes, content_type, status, expires_at) VALUES (?, ?, ?, ?, ?, 'pending', ?)".to_string(),
                    params: vec![
                        slot_id.clone().into(),
                        sender_jid.to_bare().to_string().into(),
                        safe_filename.clone().into(),
                        size_bytes.into(),
                        content_type.clone().into(),
                        expires_at.into(),
                    ],
                })
                .await
            {
                warn!(error = %e, "Failed to create upload slot in database");
                return vec![build_upload_error(
                    id,
                    &UploadError::InternalError(format!("Database error: {}", e)),
                )];
            }

            debug!(
                slot_id = %slot_id,
                put_url = %put_url,
                get_url = %get_url,
                "Created upload slot via WebSocket"
            );

            let slot = UploadSlot {
                put_url,
                put_headers: vec![("Content-Type".to_string(), content_type)],
                get_url,
            };
            let response = build_upload_slot_response(request_iq, &slot);
            return vec![iq_to_xml(response)];
        }
    }
    Vec::new()
}
