use super::*;

pub(super) async fn handle_channel_search_iq(
    iq: &xmpp_parsers::iq::Iq,
    muc_domain: &str,
    state: &WebSocketState,
    response_from: Option<&str>,
    response_to: Option<&str>,
) -> Vec<String> {
    if iq
        .to
        .as_ref()
        .is_some_and(|to| to.to_string() != muc_domain)
    {
        return vec![build_iq_error_xml_with_addresses(
            &iq.id,
            response_from,
            response_to,
            "cancel",
            "item-not-found",
        )];
    }
    let Some(request) = parse_search_request(iq) else {
        return vec![build_iq_error_xml_with_addresses(
            &iq.id,
            response_from,
            response_to,
            "modify",
            "bad-request",
        )];
    };
    let limit = request.max.unwrap_or(50).clamp(1, 200) as usize;
    let channels =
        match list_xmpp_channels(state.deps.app_state.db_pool.global_actor().clone(), 500, 0).await
        {
            Ok(channels) => channels,
            Err(error) => {
                warn!(error = %error, "Failed to load channels for WebSocket search");
                return vec![build_iq_error_xml_with_addresses(
                    &iq.id,
                    response_from,
                    response_to,
                    "wait",
                    "internal-server-error",
                )];
            }
        };
    let mut results = Vec::new();
    for channel in channels {
        let Some(room_jid) = waddle_xmpp::managed_room_jid(&channel.id, muc_domain).ok() else {
            continue;
        };
        let result = ChannelResult::new(room_jid.to_string())
            .with_name(channel.name)
            .with_description(channel.description.unwrap_or_default());
        if result.matches_query(&request.query) {
            results.push(result);
            if results.len() >= limit {
                break;
            }
        }
    }
    vec![iq_to_xml(build_search_response(iq, &results))]
}

pub(super) async fn handle_user_search_iq(
    iq: &xmpp_parsers::iq::Iq,
    domain: &str,
    state: &WebSocketState,
    response_from: Option<&str>,
    response_to: Option<&str>,
) -> Vec<String> {
    match &iq.payload {
        xmpp_parsers::iq::IqType::Get(_) => {
            let payload = Element::builder("query", "jabber:iq:search")
                .append(
                    Element::builder("instructions", "jabber:iq:search")
                        .append("Search users by username or email")
                        .build(),
                )
                .append(Element::builder("nick", "jabber:iq:search").build())
                .append(Element::builder("email", "jabber:iq:search").build())
                .build();
            vec![build_iq_result_xml(
                &iq.id,
                response_from,
                response_to,
                Some(payload),
            )]
        }
        xmpp_parsers::iq::IqType::Set(query) => {
            let term = query
                .children()
                .find(|child| matches!(child.name(), "nick" | "email" | "first" | "last"))
                .map(|child| child.text())
                .unwrap_or_default();
            let like = format!("%{}%", term.trim());
            let rows = match state
                .deps
                .app_state
                .db_pool
                .global_actor()
                .clone()
                .ask(DbQuery {
                    sql: "SELECT username, email FROM native_users WHERE domain = ? AND (username LIKE ? OR COALESCE(email, '') LIKE ?) ORDER BY username LIMIT 50".to_string(),
                    params: vec![domain.into(), like.clone().into(), like.into()],
                })
                .await
            {
                Ok(rows) => rows,
                Err(error) => {
                    warn!(error = %error, "Failed to search native users over WebSocket");
                    return vec![build_iq_error_xml_with_addresses(
                        &iq.id,
                        response_from,
                        response_to,
                        "wait",
                        "internal-server-error",
                    )];
                }
            };
            let mut query = Element::builder("query", "jabber:iq:search");
            for row in rows {
                let username = row_value(&row, 0)
                    .and_then(ValueExt::as_string)
                    .unwrap_or_default();
                if username.is_empty() {
                    continue;
                }
                let email = row_value(&row, 1)
                    .and_then(ValueExt::as_optional_string)
                    .ok()
                    .flatten();
                let mut item = Element::builder("item", "jabber:iq:search")
                    .attr("jid", format!("{username}@{domain}"))
                    .append(
                        Element::builder("nick", "jabber:iq:search")
                            .append(username.clone())
                            .build(),
                    );
                if let Some(email) = email {
                    item = item.append(
                        Element::builder("email", "jabber:iq:search")
                            .append(email)
                            .build(),
                    );
                }
                query = query.append(item.build());
            }
            vec![build_iq_result_xml(
                &iq.id,
                response_from,
                response_to,
                Some(query.build()),
            )]
        }
        _ => vec![build_iq_error_xml_with_addresses(
            &iq.id,
            response_from,
            response_to,
            "modify",
            "bad-request",
        )],
    }
}
