use futures::{stream, StreamExt};
use jid::BareJid;
use waddle_xmpp::pubsub::{
    build_pubsub_items_result, parse_pubsub_iq, PubSubItem, PubSubRequest, NS_PUBSUB,
};
use xmpp_parsers::disco::{DiscoItemsQuery, DiscoItemsResult, Item};
use xmpp_parsers::iq::Iq;

use super::disco_items::{GRANT_READS_AT_ONCE, LISTING_LIMIT};
use super::{page_listing, Session, WebSocketState};
use crate::permissions::host_grant::ScopeKey;
use crate::server::routes::websocket::frame::{ResponseBatch, ResponseFrame};
use crate::server::routes::websocket::resume_replay::{authorize_legacy_reply, LegacyReplay};
use crate::server::routes::websocket::s2s_spaces::{
    bookmarked_room, space_image, MAX_RETURNED_ITEMS,
};

pub(super) async fn reply(
    iq: &Iq,
    state: &WebSocketState,
    requester: Option<&jid::FullJid>,
    session: Option<&Session>,
) -> Option<ResponseFrame> {
    let service = state.deps.service_domains.spaces.as_str();
    if iq.to()?.as_str() != service {
        return None;
    }
    let Iq::Get { payload, .. } = iq else {
        return None;
    };
    let (node, limit, ids, children) = if payload.is("pubsub", NS_PUBSUB) {
        let PubSubRequest::Items {
            node,
            max_items,
            item_ids,
        } = parse_pubsub_iq(iq).ok()?
        else {
            return None;
        };
        (
            node,
            max_items
                .unwrap_or(MAX_RETURNED_ITEMS)
                .min(MAX_RETURNED_ITEMS),
            item_ids,
            None,
        )
    } else {
        let query = DiscoItemsQuery::try_from(payload.clone()).ok()?;
        (
            query.node.clone()?,
            MAX_RETURNED_ITEMS,
            Vec::new(),
            Some(query),
        )
    };
    let gate = &state.deps.app_state.host_grants;
    if gate.space_enrolled(&node).await != Ok(true) {
        return None;
    }
    let requester = requester?;
    let account = requester.to_bare();
    if session?.user_jid != account.as_str() {
        return None;
    }
    let service: BareJid = service.parse().ok()?;
    let key = ScopeKey::new(service.clone(), &node).ok()?;
    let grant = gate.space_reply_local(&key, requester, iq).await?;
    let deadline = tokio::time::Instant::now() + LISTING_LIMIT;
    let stored = tokio::time::timeout_at(
        deadline,
        state
            .deps
            .protocol
            .pubsub_storage
            .get_items(&service, &node, Some(limit), &ids),
    )
    .await
    .ok()?
    .ok()?;
    let incarnation = grant.replay_identity().incarnation();
    let account = &account;
    let items_requested = children.is_none();
    let decisions = stream::iter(stored.into_iter().map(|item| item.to_pubsub_item()))
        .map(|item| async move {
            let (Some(id), Some(payload)) = (item.id.as_deref(), item.payload.as_ref()) else {
                return None;
            };
            if items_requested && space_image(id, Some(payload)) {
                return Some((item, None));
            }
            let room = bookmarked_room(state, id, Some(payload))?;
            if room.as_str() != id {
                return None;
            }
            let dependency = gate
                .local_space_room_dependency(&room, account, incarnation)
                .await?;
            Some((item, Some((room.to_string(), dependency))))
        })
        .buffered(GRANT_READS_AT_ONCE)
        .collect::<Vec<_>>();
    let checked = tokio::time::timeout_at(deadline, decisions).await.ok()?;
    let mut items = Vec::new();
    let mut rooms = std::collections::BTreeMap::new();
    for (item, dependency) in checked.into_iter().flatten() {
        if let Some((room, dependency)) = dependency {
            if rooms.contains_key(&room) {
                continue;
            }
            rooms.insert(room, dependency);
        }
        items.push(PubSubItem {
            publisher: None,
            ..item
        });
    }
    let mut response = if let Some(query) = children {
        let children = rooms
            .keys()
            .map(|room| Item {
                jid: service.clone().into(),
                node: None,
                name: Some(room.clone()),
            })
            .collect();
        let (children, mut rsm) = page_listing(
            children,
            query.rsm.as_ref(),
            limit as usize,
            false,
            |item| item.name.as_deref().unwrap_or_default(),
        );
        if query.rsm.is_none() {
            rsm = None;
        } else if let Some(first) = rsm.as_mut().and_then(|page| page.first.as_mut()) {
            first.index = None;
        }
        rooms.retain(|room, _| children.iter().any(|item| item.name.as_ref() == Some(room)));
        Iq::Result {
            from: Some(service.into()),
            to: None,
            id: iq.id().to_owned(),
            payload: Some(
                DiscoItemsResult {
                    node: Some(node),
                    items: children,
                    rsm,
                }
                .into(),
            ),
        }
    } else {
        build_pubsub_items_result(iq, &node, &items)
    };
    let Iq::Result { to, .. } = &mut response else {
        return None;
    };
    *to = Some(requester.clone().into());
    let grant = grant.with_room_dependencies(rooms.into_values().collect())?;
    ResponseFrame::protected_space_frame(super::iq_to_xml(response), grant).ok()
}

pub(super) fn guard_legacy_replies(
    iq: &Iq,
    state: &WebSocketState,
    requester: Option<&jid::FullJid>,
    replies: Vec<String>,
) -> ResponseBatch {
    let service = state.deps.service_domains.spaces.as_str();
    if !matches!(iq, Iq::Get { .. }) || iq.to().is_none_or(|to| to.as_str() != service) {
        return replies.into();
    }
    let mut frames = Vec::with_capacity(replies.len());
    for xml in replies {
        match authorize_legacy_reply(xml, requester, service, &state.deps.app_state.host_grants) {
            Some(LegacyReplay::Ordinary(xml)) => frames.push(ResponseFrame::from(xml)),
            Some(LegacyReplay::Fenced(frame)) => frames.push(frame),
            None => {
                return ResponseBatch::from_frames(vec![super::iq_to_xml(Iq::Error {
                    from: iq.to().cloned(),
                    to: requester.map(|jid| jid.clone().into()),
                    id: iq.id().to_owned(),
                    payload: None,
                    error: super::item_not_found_iq_error("Requested item not found."),
                })])
            }
        }
    }
    ResponseBatch::from_frames(frames)
}
