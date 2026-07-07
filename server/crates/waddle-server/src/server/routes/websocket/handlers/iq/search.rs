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
    if is_search_form_request(iq) {
        return vec![iq_to_xml(build_search_form_response(iq))];
    }
    let Some(request) = parse_search_request(iq) else {
        return vec![build_iq_error_xml_typed(
            iq.id(),
            response_from,
            response_to,
            bad_request_iq_error("Malformed IQ payload."),
        )];
    };
    let default_limit = request.max.unwrap_or(50).clamp(1, 200);
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
        }
    }
    let (page, rsm) = search_result_page(&results, request.rsm.as_ref(), default_limit);
    vec![iq_to_xml(build_search_response_with_rsm(iq, &page, &rsm))]
}

fn search_result_page(
    results: &[ChannelResult],
    rsm: Option<&RsmRequest>,
    default_limit: u32,
) -> (Vec<ChannelResult>, RsmResponse) {
    let limit = rsm
        .and_then(|rsm| rsm.max)
        .unwrap_or(default_limit)
        .clamp(1, 200) as usize;
    let (start, end) = search_page_window(results, limit, rsm);
    let page = results[start..end].to_vec();
    let count = u32::try_from(results.len()).unwrap_or(u32::MAX);
    let mut response = RsmResponse::new().with_count(count);
    if let Some(first) = page.first() {
        response = response.with_first(
            first.address.clone(),
            Some(u32::try_from(start).unwrap_or(u32::MAX)),
        );
    }
    if let Some(last) = page.last() {
        response = response.with_last(last.address.clone());
    }
    (page, response)
}

fn search_page_window(
    results: &[ChannelResult],
    limit: usize,
    rsm: Option<&RsmRequest>,
) -> (usize, usize) {
    let total = results.len();
    if total == 0 {
        return (0, 0);
    }
    let limit = limit.clamp(1, 200);
    if let Some(rsm) = rsm {
        if let Some(before) = rsm.before.as_deref() {
            let end = if before.is_empty() {
                total
            } else {
                cursor_position(results, before).unwrap_or(total)
            };
            let start = end.saturating_sub(limit);
            return (start, end);
        }
        if let Some(after) = rsm.after.as_deref() {
            let start = cursor_position(results, after)
                .map_or(0, |index| index.saturating_add(1))
                .min(total);
            return (start, start.saturating_add(limit).min(total));
        }
        if let Some(index) = rsm.index {
            let start = (index as usize).min(total);
            return (start, start.saturating_add(limit).min(total));
        }
    }
    (0, limit.min(total))
}

fn cursor_position(results: &[ChannelResult], cursor: &str) -> Option<usize> {
    results
        .iter()
        .position(|result| result.address.as_str() == cursor)
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

#[cfg(test)]
mod tests {
    use super::*;

    fn channel_results() -> Vec<ChannelResult> {
        [
            "a@muc.example",
            "b@muc.example",
            "c@muc.example",
            "d@muc.example",
        ]
        .into_iter()
        .map(ChannelResult::new)
        .collect()
    }

    #[test]
    fn channel_search_rsm_after_returns_page_with_total_count() {
        let rsm_request = RsmRequest::new().with_max(2).with_after("b@muc.example");
        let (page, rsm_response) = search_result_page(&channel_results(), Some(&rsm_request), 50);

        let addresses: Vec<_> = page.into_iter().map(|result| result.address).collect();
        assert_eq!(addresses, vec!["c@muc.example", "d@muc.example"]);
        assert_eq!(rsm_response.first.as_deref(), Some("c@muc.example"));
        assert_eq!(rsm_response.first_index, Some(2));
        assert_eq!(rsm_response.last.as_deref(), Some("d@muc.example"));
        assert_eq!(rsm_response.count, Some(4));
    }

    #[test]
    fn channel_search_rsm_before_empty_returns_last_page() {
        let rsm_request = RsmRequest::new().with_max(2).last_page();
        let (page, rsm_response) = search_result_page(&channel_results(), Some(&rsm_request), 50);

        let addresses: Vec<_> = page.into_iter().map(|result| result.address).collect();
        assert_eq!(addresses, vec!["c@muc.example", "d@muc.example"]);
        assert_eq!(rsm_response.first_index, Some(2));
        assert_eq!(rsm_response.count, Some(4));
    }

    #[test]
    fn channel_search_without_rsm_uses_default_page_limit() {
        let (page, rsm_response) = search_result_page(&channel_results(), None, 2);

        let addresses: Vec<_> = page.into_iter().map(|result| result.address).collect();
        assert_eq!(addresses, vec!["a@muc.example", "b@muc.example"]);
        assert_eq!(rsm_response.first_index, Some(0));
        assert_eq!(rsm_response.count, Some(4));
    }
}
