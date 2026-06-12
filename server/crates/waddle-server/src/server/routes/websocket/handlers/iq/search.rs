use super::*;

pub(super) async fn handle_channel_search_iq(
    iq: &xmpp_parsers::iq::Iq,
    muc_domain: &str,
    state: &WebSocketState,
    response_from: Option<&str>,
    response_to: Option<&str>,
) -> Vec<String> {
    if iq.to().is_some_and(|to| to.to_string() != muc_domain) {
        return vec![build_iq_error_xml_typed(
            iq.id(),
            response_from,
            response_to,
            item_not_found_iq_error("Requested item not found."),
        )];
    }
    let Some(request) = parse_search_request(iq) else {
        return vec![build_iq_error_xml_typed(
            iq.id(),
            response_from,
            response_to,
            bad_request_iq_error("Malformed IQ payload."),
        )];
    };
    let limit = request.max.unwrap_or(50).clamp(1, 200) as usize;
    let channels =
        match list_xmpp_channels(state.deps.app_state.db_pool.global_actor().clone(), 500, 0).await
        {
            Ok(channels) => channels,
            Err(error) => {
                warn!(error = %error, "Failed to load channels for WebSocket search");
                return vec![build_iq_error_xml_typed(
                    iq.id(),
                    response_from,
                    response_to,
                    internal_server_error_iq_error("Internal server error."),
                )];
            }
        };
    let mut results = Vec::new();
    for channel in channels {
        if channel.channel_type == waddle_xmpp::admin::CHANNEL_TYPE_GROUP_DM {
            continue;
        }
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
    const MIN_USER_SEARCH_TERM_CHARS: usize = 2;

    match iq {
        xmpp_parsers::iq::Iq::Get { .. } => {
            let payload = Element::builder("query", "jabber:iq:search")
                .append(
                    Element::builder("instructions", "jabber:iq:search")
                        .append("Search users by username")
                        .build(),
                )
                .append(Element::builder("nick", "jabber:iq:search").build())
                .build();
            vec![build_iq_result_xml(
                iq.id(),
                response_from,
                response_to,
                Some(payload),
            )]
        }
        xmpp_parsers::iq::Iq::Set { payload: query, .. } => {
            let term = query
                .children()
                .find(|child| child.name() == "nick" && child.ns() == "jabber:iq:search")
                .map(|child| child.text())
                .unwrap_or_default();
            let term = term.trim();
            if term.chars().count() < MIN_USER_SEARCH_TERM_CHARS {
                return vec![build_iq_error_xml_typed(
                    iq.id(),
                    response_from,
                    response_to,
                    bad_request_iq_error("Malformed IQ payload."),
                )];
            }
            let like = format!("%{}%", escape_like_pattern(term));
            let rows = match state
                .deps
                .app_state
                .db_pool
                .global_actor()
                .clone()
                .ask(DbQuery {
                    sql: "SELECT username FROM native_users WHERE domain = ? AND username LIKE ? ESCAPE '\\' ORDER BY username LIMIT 50".to_string(),
                    params: vec![domain.into(), like.into()],
                })
                .await
            {
                Ok(rows) => rows,
                Err(error) => {
                    warn!(error = %error, "Failed to search native users over WebSocket");
                    return vec![build_iq_error_xml_typed(
                                    iq.id(),
                                    response_from,
                                    response_to,
                                    internal_server_error_iq_error("Internal server error."),
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
                let item = Element::builder("item", "jabber:iq:search")
                    .attr(
                        minidom::rxml::xml_ncname!("jid").to_owned(),
                        format!("{username}@{domain}"),
                    )
                    .append(
                        Element::builder("nick", "jabber:iq:search")
                            .append(username.clone())
                            .build(),
                    )
                    .build();
                query = query.append(item);
            }
            vec![build_iq_result_xml(
                iq.id(),
                response_from,
                response_to,
                Some(query.build()),
            )]
        }
        _ => vec![build_iq_error_xml_typed(
            iq.id(),
            response_from,
            response_to,
            bad_request_iq_error("Malformed IQ payload."),
        )],
    }
}

fn escape_like_pattern(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for ch in value.chars() {
        if matches!(ch, '\\' | '%' | '_') {
            escaped.push('\\');
        }
        escaped.push(ch);
    }
    escaped
}
