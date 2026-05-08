use super::*;

pub(super) async fn handle_muc_admin_iq(
    iq: &xmpp_parsers::iq::Iq,
    muc_domain: &str,
    state: &WebSocketState,
    sender_jid: Option<&FullJid>,
    response_from: Option<&str>,
    response_to: Option<&str>,
) -> Vec<String> {
    let Some(sender_jid) = sender_jid else {
        return vec![build_iq_error_xml_typed(
            &iq.id,
            response_from,
            response_to,
            not_authorized_iq_error("Authentication required."),
        )];
    };
    let mut iq_with_from = iq.clone();
    iq_with_from.from = Some(Jid::from(sender_jid.clone()));
    let query = match parse_admin_query(&iq_with_from, muc_domain) {
        Ok(query) => query,
        Err(error) => return vec![build_xmpp_error_response(&iq_with_from, error)],
    };
    let Some(room_actor) = get_room_actor(state, &query.room_jid).await else {
        return vec![build_iq_error_xml_typed(
            &iq.id,
            response_from,
            response_to,
            item_not_found_iq_error("Requested item not found."),
        )];
    };
    let context = match room_actor
        .ask(GetAdminContext {
            sender_jid: sender_jid.clone(),
        })
        .await
    {
        Ok(context) => context,
        Err(_) => {
            return vec![build_iq_error_xml_typed(
                &iq.id,
                response_from,
                response_to,
                internal_server_error_iq_error("Internal server error."),
            )];
        }
    };
    let is_admin = matches!(context.affiliation, Affiliation::Owner | Affiliation::Admin)
        || matches!(context.role, waddle_xmpp::Role::Moderator);
    if !is_admin {
        return vec![build_iq_error_xml_typed(
            &iq.id,
            response_from,
            response_to,
            forbidden_iq_error("Operation not permitted."),
        )];
    }
    if query.is_get {
        let snapshot = match room_actor.ask(GetSnapshot).await {
            Ok(snapshot) => snapshot.room,
            Err(_) => {
                return vec![build_iq_error_xml_typed(
                    &iq.id,
                    response_from,
                    response_to,
                    internal_server_error_iq_error("Internal server error."),
                )];
            }
        };
        let to_jid = Jid::from(sender_jid.clone());
        if is_role_change_query(&query.items) {
            let role_filter = query.items.iter().find_map(|item| item.role);
            let items: Vec<(String, waddle_xmpp::Role, Option<BareJid>)> = snapshot
                .occupants
                .values()
                .filter(|occupant| role_filter.is_none_or(|role| occupant.role == role))
                .map(|occupant| {
                    (
                        occupant.nick.clone(),
                        occupant.role,
                        Some(occupant.real_jid.to_bare()),
                    )
                })
                .collect();
            return vec![iq_to_xml(build_role_result(
                &iq.id,
                &query.room_jid,
                &to_jid,
                &items,
            ))];
        }
        let affiliation_filter = query.items.iter().find_map(|item| item.affiliation);
        let items: Vec<(BareJid, Affiliation)> = if let Some(affiliation) = affiliation_filter {
            snapshot
                .get_jids_by_affiliation(affiliation)
                .into_iter()
                .map(|jid| (jid, affiliation))
                .collect()
        } else {
            snapshot
                .get_all_affiliations()
                .into_iter()
                .map(|entry| (entry.jid, entry.affiliation))
                .collect()
        };
        return vec![iq_to_xml(build_admin_result(
            &iq.id,
            &query.room_jid,
            &to_jid,
            &items,
        ))];
    }
    let room_jid = query.room_jid.clone();
    let updates = match room_actor
        .ask(ApplyAdminItems {
            sender_jid: sender_jid.clone(),
            sender_affiliation: context.affiliation,
            sender_role: context.role,
            items: query.items,
        })
        .await
    {
        Ok(updates) => updates,
        Err(kameo::error::SendError::HandlerError(error)) => {
            warn!(room = %room_jid, error = %error, "MUC admin set rejected");
            return vec![build_iq_error_xml_typed(
                &iq.id,
                response_from,
                response_to,
                forbidden_iq_error("Operation not permitted."),
            )];
        }
        Err(error) => {
            warn!(room = %room_jid, error = ?error, "Failed to apply MUC admin IQ");
            return vec![build_iq_error_xml_typed(
                &iq.id,
                response_from,
                response_to,
                internal_server_error_iq_error("Internal server error."),
            )];
        }
    };
    for (recipient, presence) in updates {
        let _ = state
            .deps
            .protocol
            .connection_registry
            .send_to(&recipient, Stanza::Presence(presence))
            .await;
    }
    vec![iq_to_xml(build_admin_set_result(
        &iq.id,
        &room_jid,
        &Jid::from(sender_jid.clone()),
    ))]
}
