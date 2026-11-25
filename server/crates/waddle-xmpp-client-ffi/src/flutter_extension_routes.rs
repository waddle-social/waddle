//! Flutter bindings for channel extension route discovery and items.
use crate::{WaddleClient, WaddleError};
use jid::Jid;
pub use waddle_xmpp_client_community::extension_route_types::{
    WaddleExtensionRoute, WaddleExtensionRouteItem,
};
use waddle_xmpp_client_community::extension_routes::{self, RouteError};

fn route_error(error: RouteError<WaddleError>) -> WaddleError {
    match error {
        RouteError::Transport(e) => e,
        RouteError::Invalid => WaddleError::InvalidArgument,
        RouteError::Malformed => WaddleError::MalformedResponse,
        RouteError::Untrusted => WaddleError::UntrustedReply,
    }
}

impl WaddleClient {
    pub async fn discover_extension_routes(
        &self,
        user_jid: Option<String>,
    ) -> Result<Vec<WaddleExtensionRoute>, WaddleError> {
        let id: Jid = user_jid
            .as_deref()
            .unwrap_or(&self.config.jid)
            .parse()
            .map_err(|_| WaddleError::InvalidJid)?;
        extension_routes::discover(|iq| self.community_iq(iq), id.domain().as_str())
            .await
            .map_err(route_error)
    }

    pub async fn fetch_extension_route_items(
        &self,
        route: WaddleExtensionRoute,
        room_jid: String,
    ) -> Result<Vec<WaddleExtensionRouteItem>, WaddleError> {
        let room = self.require_bare_jid(&room_jid)?;
        extension_routes::items(|iq| self.community_iq(iq), &route, &room)
            .await
            .map_err(route_error)
    }
}

#[cfg(test)]
mod tests {
    use jid::BareJid;
    use minidom::Element;
    use waddle_xmpp_client_community::extension_route_types::WaddleExtensionRoute;
    use waddle_xmpp_client_community::extension_routes::{self, RouteError};

    fn route() -> WaddleExtensionRoute {
        WaddleExtensionRoute {
            service_jid: "extensions.example.test".into(),
            plugin_id: "polls".into(),
            route_id: "list".into(),
            label: "Polls".into(),
            scope: "channel".into(),
            surface: "list".into(),
            state_node: "urn:poll:{room}".into(),
            payload_namespace: "urn:waddle:extension:1".into(),
        }
    }

    #[tokio::test]
    async fn xep0060_room_template_uses_the_requested_room_and_refuses_foreign_results() {
        let room: BareJid = "one@muc.example.test".parse().unwrap();
        let call = |request: Element| async move {
            assert_eq!(request.attr("to"), Some("extensions.example.test"));
            let items = request
                .get_child("pubsub", "http://jabber.org/protocol/pubsub")
                .unwrap()
                .get_child("items", "http://jabber.org/protocol/pubsub")
                .unwrap();
            assert_eq!(items.attr("node"), Some("urn:poll:one@muc.example.test"));
            let response: Element =
                "<iq xmlns='jabber:client' type='result' from='other.example.test'/>"
                    .parse()
                    .unwrap();
            Ok::<_, ()>(response)
        };
        assert!(matches!(
            extension_routes::items(call, &route(), &room).await,
            Err(RouteError::Untrusted)
        ));
        let wrong_node = |_: Element| async {
            Ok::<Element,()>(
            "<iq xmlns='jabber:client' from='extensions.example.test'><pubsub xmlns='http://jabber.org/protocol/pubsub'><items node='urn:poll:two@muc.example.test'/></pubsub></iq>".parse().unwrap())
        };
        assert!(matches!(
            extension_routes::items(wrong_node, &route(), &room).await,
            Err(RouteError::Malformed)
        ));
    }
}
