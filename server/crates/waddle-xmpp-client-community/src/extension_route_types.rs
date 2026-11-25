//! Serializable extension route descriptors and room items.

use serde::{Deserialize, Serialize};

/// Discovered extension route and its backing PubSub state node.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WaddleExtensionRoute {
    /// XMPP service address that processes this request or route.
    pub service_jid: String,
    /// Identifier of the extension plugin owning the route or action.
    pub plugin_id: String,
    /// Identifier of the discovered extension route.
    pub route_id: String,
    /// Human-readable display label.
    pub label: String,
    /// Context scope advertised by the extension route.
    pub scope: String,
    /// UI surface on which this route is intended to appear.
    pub surface: String,
    /// Backing PubSub node, potentially containing a `{room}` placeholder.
    pub state_node: String,
    /// XML namespace identifying this extension's payload format.
    pub payload_namespace: String,
}

/// Renderable extension item with optional display metadata and actions.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WaddleExtensionRouteItem {
    /// PubSub item identifier, when supplied by the route.
    pub id: Option<String>,
    /// Human-readable title supplied by the producer.
    pub title: Option<String>,
    /// Optional secondary display text.
    pub subtitle: Option<String>,
    /// Optional navigation link associated with the item.
    pub link: Option<WaddleExtensionRouteLink>,
    /// Optional descriptive text for display.
    pub description: Option<String>,
    /// Named display fields supplied by the route item.
    pub fields: Vec<WaddleExtensionRouteItemField>,
    /// Selectable options supplied by the route item.
    pub options: Vec<WaddleExtensionRouteItemOption>,
    /// Invocable actions supplied by the route item.
    pub actions: Vec<WaddleExtensionRouteItemAction>,
}

/// Navigation target supplied by an extension route item.
#[derive(Debug, Clone, Serialize)]
pub struct WaddleExtensionRouteLink {
    /// Navigation URL supplied by the route item.
    pub href: String,
}

/// Named textual field displayed on an extension route item.
#[derive(Debug, Clone, Serialize)]
pub struct WaddleExtensionRouteItemField {
    /// Machine-readable field name supplied by the extension.
    pub name: String,
    /// Human-readable display label.
    pub label: Option<String>,
    /// Textual content displayed for the named field.
    pub value: String,
}

/// Selectable option offered by an extension route item.
#[derive(Debug, Clone, Serialize)]
pub struct WaddleExtensionRouteItemOption {
    /// Machine-readable identifier of the selectable option.
    pub id: String,
    /// Human-readable display label.
    pub label: String,
}

/// Invocable action exposed by an extension route item.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WaddleExtensionRouteItemAction {
    /// Signed launch parameters for the action.
    pub launch: WaddleExtensionRouteItemLaunch,
}

/// Signed launch parameters for a discovered extension route action.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WaddleExtensionRouteItemLaunch {
    /// Identifier of this signed route-item launch.
    pub id: String,
    /// Identifier of the extension plugin owning the route or action.
    pub plugin_id: String,
    /// Identifier of the extension action to invoke.
    pub action_id: String,
    /// XMPP ad-hoc command node used to invoke the action.
    pub command_node: String,
    /// Human-readable display label.
    pub label: String,
    /// Signed authorization token required when invoking the action.
    pub launch_token: String,
    /// RFC 3339 expiry time of the action authorization.
    pub expires_at: String,
    /// Community identifier associated with the action.
    pub waddle_id: String,
    /// Room address associated with the request or action.
    pub room_jid: Option<String>,
    /// Stable stanza identifier of the source message, when available.
    pub source_stanza_id: Option<String>,
}
