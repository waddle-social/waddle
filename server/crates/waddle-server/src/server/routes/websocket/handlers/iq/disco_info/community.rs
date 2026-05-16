use super::*;

const FEED_NODE_LABEL: &str = "Community Feed";
const STORIES_NODE_LABEL: &str = "Community Stories";
const EVENTS_NODE_LABEL: &str = "Community Events";

pub(super) fn handle_community_disco_info<'a>(
    req: &'a DiscoInfoRequest<'a>,
) -> Option<DiscoInfoResponse<'a>> {
    if req.target_to != Some(req.community_domain) {
        return None;
    }

    // Per-node disco#info — clients ask for `community.<domain>?node=<feed
    // or stories node URI>` to learn which features each node supports.
    if let Some(node) = req.node {
        let (label, ns_feature) = match node {
            waddle_xmpp_core::xep0472::PUBSUB_NODE_FEED => {
                (FEED_NODE_LABEL, Feature::social_feed())
            }
            waddle_xmpp_core::xep0501::PUBSUB_NODE_STORIES => {
                (STORIES_NODE_LABEL, Feature::stories())
            }
            waddle_xmpp_core::xep0471::PUBSUB_NODE_EVENTS => {
                (EVENTS_NODE_LABEL, Feature::calendar())
            }
            _ => {
                return Some(DiscoInfoResponse::error(
                    req.id,
                    None,
                    None,
                    item_not_found_iq_error("Requested item not found."),
                ));
            }
        };
        let identities = vec![Identity::pubsub_leaf(Some(label))];
        let features = vec![
            Feature::disco_info(),
            Feature::pubsub(),
            Feature::pubsub_retrieve_items(),
            ns_feature,
        ];
        let response =
            build_disco_info_response(req.request_iq, &identities, &features, Some(node));
        return Some(DiscoInfoResponse::iq(response));
    }

    let identities = vec![Identity::community_service(Some("Community"))];
    let features = community_service_features();
    let response = build_disco_info_response(req.request_iq, &identities, &features, None);
    Some(DiscoInfoResponse::iq(response))
}
