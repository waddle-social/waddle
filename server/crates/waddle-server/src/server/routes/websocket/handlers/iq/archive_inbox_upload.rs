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
        let Some(target) = request_iq.to().map(|jid| jid.to_string()) else {
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

        // Resolve the managed-channel row once. Both the XEP-0313 §5.1
        // archive gate and the group-DM visibility boundary need it;
        // sharing the lookup avoids a duplicate DB round-trip per query.
        let managed_channel = match get_managed_channel_for_room(state, &target_bare).await {
            Ok(channel) => channel,
            Err(error) => {
                warn!(error = %error, target = %target_bare, "Failed to resolve managed channel for MAM query");
                return vec![build_iq_error_xml_typed(
                    id,
                    None,
                    None,
                    internal_server_error_iq_error("Internal server error."),
                )];
            }
        };

        // XEP-0313 §5.1: a MUC archive MUST only serve users who have the
        // right to enter the room at query time. Gate before any archive
        // read, for every room type (#1093).
        if !is_personal {
            match resolve_muc_room_archive_access(
                state,
                &target_bare,
                sender_bare.as_ref(),
                managed_channel.as_ref(),
            )
            .await
            {
                RoomArchiveAccess::Allowed => {}
                RoomArchiveAccess::Denied => {
                    return vec![build_iq_error_xml_typed(
                        id,
                        None,
                        None,
                        forbidden_iq_error("Operation not permitted."),
                    )];
                }
                RoomArchiveAccess::Error => {
                    return vec![build_iq_error_xml_typed(
                        id,
                        None,
                        None,
                        internal_server_error_iq_error("Internal server error."),
                    )];
                }
            }
        }

        if is_mam_query_form_request(request_iq) {
            return vec![iq_to_xml(build_query_form_iq(request_iq))];
        }

        let (query_id, mut query) = match parse_mam_query(request_iq) {
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

        let visibility = group_dm_archive_visibility(
            state,
            &target_bare,
            sender_bare.as_ref(),
            managed_channel.as_ref(),
        )
        .await;
        let visibility_boundary = match visibility {
            GroupDmArchiveVisibility::NotGroupDm | GroupDmArchiveVisibility::Full => None,
            GroupDmArchiveVisibility::Restricted(boundary) => Some(boundary),
            GroupDmArchiveVisibility::Denied => {
                return vec![build_iq_error_xml_typed(
                    id,
                    None,
                    None,
                    forbidden_iq_error("Operation not permitted."),
                )];
            }
            GroupDmArchiveVisibility::Error => {
                return vec![build_iq_error_xml_typed(
                    id,
                    None,
                    None,
                    internal_server_error_iq_error("Internal server error."),
                )];
            }
        };
        if let Some(boundary) = visibility_boundary {
            query.start = Some(query.start.map_or(boundary, |start| start.max(boundary)));
        }

        let mut result = match state
            .deps
            .protocol
            .mam_storage
            .query_messages(
                &target_bare,
                if is_personal {
                    waddle_xmpp::mam::MamArchiveKind::Personal
                } else {
                    waddle_xmpp::mam::MamArchiveKind::Room
                },
                &query,
            )
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
            .from()
            .cloned()
            .or_else(|| phase.bound_jid().cloned().map(jid::Jid::from))
        else {
            return vec![build_iq_error_xml_typed(
                id,
                None,
                None,
                bad_request_iq_error("Malformed IQ payload."),
            )];
        };

        // XEP-0313 §Security "Sender Impersonation": result envelopes
        // MUST come from the queried archive JID — the room bare JID
        // for MUC archives, the user's own bare JID for personal
        // archives (#1250). `target_bare` is exactly that queried
        // archive address for both cases.
        let archive_jid = jid::Jid::from(target_bare.clone());
        let mut responses: Vec<String> =
            build_result_messages(&query_id, &archive_jid, &recipient_jid, &result.messages)
                .into_iter()
                .map(|message| stanza_to_xml(&Stanza::Message(message)))
                .collect();
        responses.push(iq_to_xml(build_fin_iq(request_iq, &result)));
        return responses;
    }

    if is_inbox_iq(iq) {
        return handle_inbox_query_iq(iq, id, state, phase).await;
    }

    if is_mark_read_iq(iq) {
        return handle_inbox_mark_read_iq(iq, id, state, phase).await;
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
                let response = xmpp_parsers::iq::Iq::Result {
                    from: request_iq.to().cloned(),
                    to: request_iq.from().cloned(),
                    id: request_iq.id().to_string(),
                    payload: Some(payload),
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
                    return vec![iq_to_xml(build_upload_error(request_iq, &e))];
                }
            };

            // Check file size limits (default 10 MB). The helper also
            // caps operator configuration to the database BIGINT range
            // so accepted XEP-0363 sizes are stored losslessly.
            let max_size = crate::server::routes::uploads::max_upload_size();
            if request.size > max_size {
                return vec![iq_to_xml(build_upload_error(
                    request_iq,
                    &UploadError::FileTooLarge { max_size },
                ))];
            }
            let Some(size_bytes) = crate::server::routes::uploads::upload_size_to_i64(request.size)
            else {
                return vec![iq_to_xml(build_upload_error(
                    request_iq,
                    &UploadError::FileTooLarge {
                        max_size: crate::server::routes::uploads::MAX_DATABASE_UPLOAD_SIZE,
                    },
                ))];
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
                return vec![iq_to_xml(build_upload_error(
                    request_iq,
                    &UploadError::InternalError,
                ))];
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

enum GroupDmArchiveVisibility {
    NotGroupDm,
    Full,
    Restricted(chrono::DateTime<chrono::Utc>),
    Denied,
    Error,
}

async fn group_dm_archive_visibility(
    state: &WebSocketState,
    room_jid: &BareJid,
    sender_bare: Option<&BareJid>,
    channel: Option<&XmppChannelRecord>,
) -> GroupDmArchiveVisibility {
    let Some(sender_bare) = sender_bare else {
        return GroupDmArchiveVisibility::Denied;
    };
    // The managed-channel row was resolved once by the caller and shared
    // with the archive gate; a `None` row means the room is unmanaged and
    // therefore not a group DM.
    let Some(channel) = channel else {
        return GroupDmArchiveVisibility::NotGroupDm;
    };
    if channel.channel_type != waddle_xmpp::admin::CHANNEL_TYPE_GROUP_DM {
        return GroupDmArchiveVisibility::NotGroupDm;
    }
    let channel_id = channel.id.as_str();

    let allowed = state
        .deps
        .app_state
        .permission_actor
        .ask(CheckPermission {
            subject: Subject::user(sender_bare.to_string()),
            permission: Permission::Member,
            object: Object::new(ObjectType::Channel, channel_id),
        })
        .await
        .map(|response| response.allowed);
    match allowed {
        Ok(true) => {}
        Ok(false) => return GroupDmArchiveVisibility::Denied,
        Err(error) => {
            warn!(
                error = %error,
                room = %room_jid,
                requester = %sender_bare,
                "Failed to authorize group-DM MAM query"
            );
            return GroupDmArchiveVisibility::Error;
        }
    }

    let actor = state.deps.app_state.db_pool.global_actor().clone();
    let row = match actor
        .ask(crate::db::actor::DbQueryOne {
            sql: "SELECT visible_after FROM group_dm_archive_boundaries WHERE room_jid = ? AND member_jid = ?".to_string(),
            params: vec![room_jid.to_string().into(), sender_bare.to_string().into()],
        })
        .await
    {
        Ok(Some(row)) => row,
        Ok(None) => return GroupDmArchiveVisibility::Full,
        Err(error) => {
            warn!(
                error = %error,
                room = %room_jid,
                requester = %sender_bare,
                "Failed to load group-DM archive boundary"
            );
            return GroupDmArchiveVisibility::Error;
        }
    };
    match row.first() {
        Some(crate::db::Value::Text(value)) => chrono::DateTime::parse_from_rfc3339(value)
            .map(|dt| GroupDmArchiveVisibility::Restricted(dt.with_timezone(&chrono::Utc)))
            .unwrap_or(GroupDmArchiveVisibility::Error),
        Some(value) if value.is_null() => GroupDmArchiveVisibility::Full,
        _ => GroupDmArchiveVisibility::Error,
    }
}

/// Handle the XEP-0430 `<inbox xmlns='urn:xmpp:inbox:1'/>` IQ-get
/// streaming response. Emits one `<message/>` per matched conversation
/// (with an optional embedded MAM `<result>/<forwarded>` body when
/// `messages='true'`) followed by the final `<iq type='result'><fin/></iq>`.
async fn handle_inbox_query_iq(
    iq: &xmpp_parsers::iq::Iq,
    id: &str,
    state: &WebSocketState,
    phase: &ConnectionPhase,
) -> Vec<String> {
    let Some(user_jid) = phase.bound_jid().map(|jid| jid.to_bare()) else {
        return vec![build_iq_error_xml_typed(
            id,
            None,
            None,
            not_authorized_iq_error("Authentication required."),
        )];
    };

    if !matches!(iq, xmpp_parsers::iq::Iq::Get { .. }) {
        return vec![build_iq_error_xml_typed(
            id,
            None,
            None,
            bad_request_iq_error("Malformed IQ payload."),
        )];
    }

    let query = match parse_inbox_query(iq) {
        Ok(query) => query,
        Err(error) => {
            warn!(error = %error, "Invalid XEP-0430 inbox query");
            return vec![build_iq_error_xml_typed(
                id,
                None,
                None,
                bad_request_iq_error("Malformed IQ payload."),
            )];
        }
    };

    let entries = match state.deps.protocol.inbox_storage.list(&user_jid).await {
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
    };

    let all_unread_total = match state
        .deps
        .protocol
        .inbox_storage
        .total_unread(&user_jid)
        .await
    {
        Ok(total) => total,
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

    // XEP-0430 filtering: drop read conversations when `unread-only`.
    let mut filtered: Vec<_> = entries
        .into_iter()
        .filter(|entry| !query.unread_only || entry.unread > 0)
        .collect();
    filtered.sort_by(|left, right| {
        right
            .last_updated
            .cmp(&left.last_updated)
            .then_with(|| left.partner.cmp(&right.partner))
    });

    // Apply RSM `<max/>` paging. Other RSM cursors (`<after/>`,
    // `<before/>`, `<index/>`) are best-effort: the inbox is
    // small per-user and clients typically request the full set;
    // a future iteration can switch to opaque cursor encoding.
    let (page, rsm_response) = apply_inbox_rsm(filtered, query.rsm.as_ref());

    // Determine the recipient JID for the streamed messages. Use the
    // requesting client's full JID when available; fall back to the
    // bound JID.
    let Some(recipient_jid) = iq
        .from()
        .cloned()
        .or_else(|| phase.bound_jid().cloned().map(jid::Jid::from))
    else {
        return vec![build_iq_error_xml_typed(
            id,
            None,
            None,
            bad_request_iq_error("Malformed IQ payload."),
        )];
    };

    let mut responses: Vec<String> = Vec::with_capacity(page.len() + 1);
    let mut page_unread = 0u32;
    let mut page_unread_sum: u32 = 0;
    for entry in &page {
        if entry.unread > 0 {
            page_unread = page_unread.saturating_add(1);
            page_unread_sum = page_unread_sum.saturating_add(entry.unread);
        }
        let last_message = if query.messages {
            inbox_last_message_for_entry(state, &user_jid, entry).await
        } else {
            None
        };
        let archive_keepalive;
        let last_message_borrowed: Option<InboxLastMessage<'_>> = match &last_message {
            Some(payload) => {
                archive_keepalive = payload;
                Some(InboxLastMessage {
                    mam_id: archive_keepalive.mam_id.as_str(),
                    forwarded_inner: archive_keepalive.forwarded_inner.clone(),
                    delay_stamp: Some(archive_keepalive.delay_stamp.as_str()),
                })
            }
            None => None,
        };
        let message =
            build_inbox_entry_message(recipient_jid.clone(), iq.id(), entry, last_message_borrowed);
        responses.push(stanza_to_xml(&Stanza::Message(message)));
    }

    let counts = InboxFinCounts {
        total: u32::try_from(page.len()).unwrap_or(u32::MAX),
        unread: page_unread,
        all_unread: u32::try_from(all_unread_total)
            .unwrap_or(u32::MAX)
            .max(page_unread_sum),
    };
    let fin = build_inbox_fin_iq(iq, counts, rsm_response);
    responses.push(iq_to_xml(fin));
    responses
}

/// Owned form of the inbox `<result/><forwarded/>` payload kept on
/// the async heap so the typed [`InboxLastMessage`] borrow can be
/// reconstructed when handing it to [`build_inbox_entry_message`].
struct OwnedLastMessage {
    mam_id: String,
    forwarded_inner: xmpp_parsers::minidom::Element,
    delay_stamp: String,
}

/// Resolve the most-recent archived `<message/>` body for one inbox
/// entry by looking it up in MAM keyed on the entry's
/// `last_stanza_id`. Returns `None` when the archive lookup fails or
/// the row is missing — the streamed `<entry/>` still carries the id
/// so clients can decide whether to back-fill via a follow-up MAM
/// query.
async fn inbox_last_message_for_entry(
    state: &WebSocketState,
    user_jid: &BareJid,
    entry: &waddle_xmpp::inbox::InboxEntry,
) -> Option<OwnedLastMessage> {
    // 1:1 conversations archive under the user's bare JID; MUC
    // conversations archive under the room JID. The inbox entry's
    // `partner` field is the right archive key for MUC rows; for
    // direct rows we resolve via the requesting user's archive.
    let archive_jid = match entry.kind {
        waddle_xmpp::inbox::ConversationKind::Direct => user_jid,
        waddle_xmpp::inbox::ConversationKind::MucRoom => &entry.partner,
    };

    let archived = match state
        .deps
        .protocol
        .mam_storage
        .get_message_by_archive_or_stanza_id(archive_jid, entry.last_stanza_id.as_str())
        .await
    {
        Ok(Some(archived)) => archived,
        Ok(None) => return None,
        Err(error) => {
            warn!(
                error = %error,
                archive = %archive_jid,
                stanza_id = %entry.last_stanza_id,
                "Inbox: MAM lookup for last message failed"
            );
            return None;
        }
    };

    let inner = waddle_xmpp::mam::archived_inner_message(&archived);
    Some(OwnedLastMessage {
        mam_id: archived.id,
        forwarded_inner: inner,
        delay_stamp: archived.timestamp.to_rfc3339(),
    })
}

/// Apply RSM `<max/>` paging to a sorted inbox list and produce the
/// matching `<set/>` response with `first`/`last`/`count`.
/// Server-side ceiling on one XEP-0430 result page. XEP-0059 §2.1 lets the
/// server return a partial set with an RSM `<set/>` regardless of what the
/// client asked for; an unbounded inbox (one countable `<message/>` per
/// conversation plus `<fin/>`) could otherwise exceed the terminal-recovery
/// window when a send-window pause exhausts mid-response, permanently losing
/// the acknowledged query's tail.
const INBOX_QUERY_MAX_PAGE: usize = 512;

fn apply_inbox_rsm(
    entries: Vec<waddle_xmpp::inbox::InboxEntry>,
    rsm: Option<&waddle_xmpp::xep::xep0059::RsmRequest>,
) -> (
    Vec<waddle_xmpp::inbox::InboxEntry>,
    Option<waddle_xmpp::xep::xep0059::RsmResponse>,
) {
    use waddle_xmpp::xep::xep0059::RsmResponse;

    let total = u32::try_from(entries.len()).unwrap_or(u32::MAX);
    let requested = rsm.and_then(|r| r.max).map(|m| m as usize);
    let effective = requested.map_or(INBOX_QUERY_MAX_PAGE, |max| max.min(INBOX_QUERY_MAX_PAGE));

    // Cursors resolve BEFORE the ceiling. The server-enforced ceiling makes
    // paging mandatory for large inboxes, so a follow-up `<after>` request
    // must actually continue from the advertised cursor — slicing from index
    // zero would repeat page one forever and leave everything past the
    // ceiling unreachable.
    let window_start: usize = match rsm {
        Some(request) => {
            if let Some(after) = request.after.as_deref() {
                entries
                    .iter()
                    .position(|entry| inbox_rsm_cursor(entry) == after)
                    .map_or(entries.len(), |index| index + 1)
            } else if let Some(index) = request.index {
                (index as usize).min(entries.len())
            } else {
                0
            }
        }
        None => 0,
    };
    let window_end: usize = match rsm.and_then(|request| request.before.as_deref()) {
        // XEP-0059 §2.3: an empty <before/> asks for the LAST page.
        Some("") => entries.len(),
        Some(before) => entries
            .iter()
            .position(|entry| inbox_rsm_cursor(entry) == before)
            .unwrap_or(entries.len()),
        None => entries.len(),
    };
    let window_start = window_start.min(window_end);
    let window: Vec<_> = entries
        .into_iter()
        .skip(window_start)
        .take(window_end - window_start)
        .collect();
    let truncated = effective < window.len();
    let (first_index, page): (usize, Vec<_>) =
        if matches!(rsm.and_then(|request| request.before.as_deref()), Some("")) {
            // Last page: take the final `effective` items of the window.
            let skip = window.len().saturating_sub(effective);
            (window_start + skip, window.into_iter().skip(skip).collect())
        } else if truncated {
            (window_start, window.into_iter().take(effective).collect())
        } else {
            (window_start, window)
        };

    // Carry an RSM response whenever the client asked for paging OR the
    // server truncated on its own ceiling — a partial set without `<set/>`
    // would read as complete.
    if rsm.is_none() && !truncated {
        return (page, None);
    }

    let first = page.first().map(inbox_rsm_cursor);
    let last = page.last().map(inbox_rsm_cursor);
    let response = RsmResponse {
        first,
        first_index: Some(u32::try_from(first_index).unwrap_or(u32::MAX)),
        last,
        count: Some(total),
    };
    (page, Some(response))
}

fn inbox_rsm_cursor(entry: &waddle_xmpp::inbox::InboxEntry) -> String {
    // Composite cursor: partner JID + optional thread id, opaque to
    // the client per XEP-0059 §2.1. Encoding stays inside this module
    // so server-side paging can evolve without a wire-shape break.
    match &entry.thread_id {
        Some(thread) => format!("{}#{}", entry.partner, thread),
        None => entry.partner.to_string(),
    }
}

/// Handle the Waddle-private `<mark-read xmlns='urn:waddle:inbox:0'/>`
/// IQ-set. Returns the IQ result and fans the post-update entry out to
/// the user's other resources for cross-device unread-state sync.
async fn handle_inbox_mark_read_iq(
    iq: &xmpp_parsers::iq::Iq,
    id: &str,
    state: &WebSocketState,
    phase: &ConnectionPhase,
) -> Vec<String> {
    let Some(user_jid) = phase.bound_jid().map(|jid| jid.to_bare()) else {
        return vec![build_iq_error_xml_typed(
            id,
            None,
            None,
            not_authorized_iq_error("Authentication required."),
        )];
    };

    let mark_read = match parse_mark_read(iq) {
        Ok(parsed) => parsed,
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

    if let Some(entry) = updated {
        crate::server::routes::interpret::push_inbox_update(
            state.deps.protocol.connection_registry.as_ref(),
            Some(&state.deps.protocol.user_registry),
            &user_jid,
            &entry,
        )
        .await;
    }

    vec![iq_to_xml(build_mark_read_result(iq))]
}

#[cfg(test)]
mod inbox_rsm_tests {
    use super::{apply_inbox_rsm, INBOX_QUERY_MAX_PAGE};
    use waddle_xmpp::xep::xep0059::RsmRequest;

    fn entries(count: usize) -> Vec<waddle_xmpp::inbox::InboxEntry> {
        (0..count)
            .map(|index| {
                waddle_xmpp::inbox::InboxEntry::new(
                    format!("peer{index}@example.com")
                        .parse()
                        .expect("valid bare jid"),
                    waddle_xmpp::inbox::ConversationKind::Direct,
                    format!("anchor-{index}"),
                    1_700_000_000_000 + index as i64,
                )
            })
            .collect()
    }

    /// XEP-0430 + XEP-0059 §2.1: the server may return a partial set, but a
    /// partial set MUST carry an RSM `<set/>` so the client can page. The
    /// ceiling keeps one response page well under the terminal-recovery
    /// window so an exhausted send pause cannot strand an acknowledged
    /// query's unbounded tail.
    #[test]
    fn inbox_pages_are_bounded_by_the_server_ceiling() {
        let (page, set) = apply_inbox_rsm(entries(INBOX_QUERY_MAX_PAGE + 100), None);
        assert_eq!(page.len(), INBOX_QUERY_MAX_PAGE);
        let set = set.expect("server-side truncation must advertise RSM paging");
        assert_eq!(
            set.count,
            Some(u32::try_from(INBOX_QUERY_MAX_PAGE + 100).expect("count fits"))
        );

        let (page, set) = apply_inbox_rsm(entries(3), None);
        assert_eq!(page.len(), 3);
        assert!(set.is_none(), "a complete un-paged set stays bare");

        let (page, _) = apply_inbox_rsm(
            entries(20),
            Some(&RsmRequest {
                max: Some(10),
                ..RsmRequest::default()
            }),
        );
        assert_eq!(page.len(), 10);

        let (page, set) = apply_inbox_rsm(
            entries(INBOX_QUERY_MAX_PAGE + 5),
            Some(&RsmRequest {
                max: Some(u32::MAX),
                ..RsmRequest::default()
            }),
        );
        assert_eq!(
            page.len(),
            INBOX_QUERY_MAX_PAGE,
            "a client max above the ceiling is clamped"
        );
        assert!(set.is_some());
    }
}
