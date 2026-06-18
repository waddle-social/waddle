use std::collections::HashSet;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use async_trait::async_trait;

use super::http::{
    apply_runtime_http_headers, execute_runtime_http_request, is_disallowed_extension_http_header,
    normalize_http_origin,
};
use super::waddle::extension::host_tools::Host as HostToolsHost;
use super::waddle::extension::types as wit_types;
use super::HostState;
use crate::host_tools as host_domain;
use crate::host_tools::{
    ExtensionHostTools, HostToolError, HostToolErrorCode, InvocationContext, InvocationKind,
};
use crate::types::{DisplayText, ExtensionCapability, PluginId, StanzaId, WaddleId};

#[derive(Debug, Default)]
struct MockHostTools {
    list_channels_calls: AtomicUsize,
    send_message_calls: AtomicUsize,
}

#[async_trait]
impl ExtensionHostTools for MockHostTools {
    async fn list_channels(
        &self,
        context: &InvocationContext,
        _request: host_domain::ListChannelsRequest,
    ) -> std::result::Result<host_domain::ListChannelsResponse, HostToolError> {
        self.list_channels_calls.fetch_add(1, Ordering::SeqCst);
        assert_eq!(
            context
                .requester
                .as_ref()
                .expect("trusted invocation requester")
                .to_string(),
            "alice@example.com"
        );
        Ok(host_domain::ListChannelsResponse {
            channels: vec![host_domain::ChannelSummary {
                room: "room@muc.example.com".parse().expect("room jid"),
                name: Some(DisplayText::new("Room").expect("display text")),
                description: None,
            }],
        })
    }

    async fn list_spaces(
        &self,
        _context: &InvocationContext,
        _request: host_domain::ListSpacesRequest,
    ) -> std::result::Result<host_domain::ListSpacesResponse, HostToolError> {
        Err(unsupported())
    }

    async fn list_room_members(
        &self,
        _context: &InvocationContext,
        _request: host_domain::ListRoomMembersRequest,
    ) -> std::result::Result<host_domain::ListRoomMembersResponse, HostToolError> {
        Err(unsupported())
    }

    async fn get_presence(
        &self,
        _context: &InvocationContext,
        _request: host_domain::GetPresenceRequest,
    ) -> std::result::Result<host_domain::GetPresenceResponse, HostToolError> {
        Err(unsupported())
    }

    async fn get_roster(
        &self,
        _context: &InvocationContext,
        _request: host_domain::GetRosterRequest,
    ) -> std::result::Result<host_domain::GetRosterResponse, HostToolError> {
        Err(unsupported())
    }

    async fn query_mam(
        &self,
        _context: &InvocationContext,
        _query: host_domain::MamQuery,
    ) -> std::result::Result<host_domain::MamQueryResponse, HostToolError> {
        Err(unsupported())
    }

    async fn send_message(
        &self,
        context: &InvocationContext,
        request: host_domain::SendMessageRequest,
    ) -> std::result::Result<host_domain::SendMessageResponse, HostToolError> {
        self.send_message_calls.fetch_add(1, Ordering::SeqCst);
        assert_eq!(
            context
                .requester
                .as_ref()
                .expect("trusted invocation requester")
                .to_string(),
            "alice@example.com"
        );
        assert_eq!(request.body.as_str(), "hello from extension");
        assert_eq!(
            request.markup,
            vec![
                host_domain::MessageMarkupSpan {
                    kind: host_domain::MessageMarkupKind::Blockquote,
                    start: 0,
                    end: 5,
                },
                host_domain::MessageMarkupSpan {
                    kind: host_domain::MessageMarkupKind::Blockquote,
                    start: 5,
                    end: 10,
                },
            ]
        );
        Ok(host_domain::SendMessageResponse {
            stanza_id: StanzaId::new("extension-stanza").expect("stanza id"),
        })
    }

    async fn pubsub_get_items(
        &self,
        _context: &InvocationContext,
        _request: host_domain::PubSubGetItemsRequest,
    ) -> std::result::Result<host_domain::PubSubGetItemsResponse, HostToolError> {
        Err(unsupported())
    }
}

#[tokio::test]
async fn denied_capability_fails_closed_before_delegating() {
    let tools = Arc::new(MockHostTools::default());
    let mut state = host_state(Arc::clone(&tools), HashSet::new());

    let result = HostToolsHost::list_channels(
        &mut state,
        wit_types::ListChannelsRequest { reserved: None },
    )
    .await
    .expect("host import does not trap");

    let error = result.expect_err("missing capability is denied");
    assert!(matches!(error.code, wit_types::HostToolErrorCode::Denied));
    assert_eq!(tools.list_channels_calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn granted_host_import_delegates_to_trait() {
    let tools = Arc::new(MockHostTools::default());
    let mut grants = HashSet::new();
    grants.insert(ExtensionCapability::HostChannelsRead);
    let mut state = host_state(Arc::clone(&tools), grants);

    let response = HostToolsHost::list_channels(
        &mut state,
        wit_types::ListChannelsRequest { reserved: None },
    )
    .await
    .expect("host import does not trap")
    .expect("capability grant allows delegation");

    assert_eq!(tools.list_channels_calls.load(Ordering::SeqCst), 1);
    assert_eq!(response.channels[0].room.value, "room@muc.example.com");
}

#[tokio::test]
async fn granted_send_message_import_delegates_to_trait() {
    let tools = Arc::new(MockHostTools::default());
    let mut grants = HashSet::new();
    grants.insert(ExtensionCapability::HostMessageSend);
    let mut state = host_state(Arc::clone(&tools), grants);

    let response = HostToolsHost::send_message(
        &mut state,
        wit_types::SendMessageRequest {
            target: wit_types::MessageTarget::Muc(wit_types::RoomJid {
                value: "room@muc.example.com".to_string(),
            }),
            body: wit_types::DisplayText {
                value: "hello from extension".to_string(),
            },
            thread_id: None,
            reply_to: None,
            markup: vec![
                wit_types::MessageMarkupSpan {
                    kind: wit_types::MessageMarkupKind::Blockquote,
                    start: 0,
                    end: 5,
                },
                wit_types::MessageMarkupSpan {
                    kind: wit_types::MessageMarkupKind::Blockquote,
                    start: 5,
                    end: 10,
                },
            ],
            extensions: None,
        },
    )
    .await
    .expect("host import does not trap")
    .expect("capability grant allows delegation");

    assert_eq!(tools.send_message_calls.load(Ordering::SeqCst), 1);
    assert_eq!(response.stanza_id.value, "extension-stanza");
}

#[tokio::test]
async fn invalid_send_message_markup_range_fails_before_delegating() {
    let tools = Arc::new(MockHostTools::default());
    let mut grants = HashSet::new();
    grants.insert(ExtensionCapability::HostMessageSend);
    let mut state = host_state(Arc::clone(&tools), grants);

    let result = HostToolsHost::send_message(
        &mut state,
        wit_types::SendMessageRequest {
            target: wit_types::MessageTarget::Muc(wit_types::RoomJid {
                value: "room@muc.example.com".to_string(),
            }),
            body: wit_types::DisplayText {
                value: "hi".to_string(),
            },
            thread_id: None,
            reply_to: None,
            markup: vec![wit_types::MessageMarkupSpan {
                kind: wit_types::MessageMarkupKind::Blockquote,
                start: 0,
                end: 3,
            }],
            extensions: None,
        },
    )
    .await
    .expect("host import does not trap");

    let error = result.expect_err("invalid markup range is rejected");
    assert!(matches!(
        error.code,
        wit_types::HostToolErrorCode::InvalidRequest
    ));
    assert_eq!(tools.send_message_calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn crossing_send_message_markup_ranges_fail_before_delegating() {
    let tools = Arc::new(MockHostTools::default());
    let mut grants = HashSet::new();
    grants.insert(ExtensionCapability::HostMessageSend);
    let mut state = host_state(Arc::clone(&tools), grants);

    let result = HostToolsHost::send_message(
        &mut state,
        wit_types::SendMessageRequest {
            target: wit_types::MessageTarget::Muc(wit_types::RoomJid {
                value: "room@muc.example.com".to_string(),
            }),
            body: wit_types::DisplayText {
                value: "0123456789abcdef".to_string(),
            },
            thread_id: None,
            reply_to: None,
            markup: vec![
                wit_types::MessageMarkupSpan {
                    kind: wit_types::MessageMarkupKind::Blockquote,
                    start: 0,
                    end: 10,
                },
                wit_types::MessageMarkupSpan {
                    kind: wit_types::MessageMarkupKind::Blockquote,
                    start: 5,
                    end: 15,
                },
            ],
            extensions: None,
        },
    )
    .await
    .expect("host import does not trap");

    let error = result.expect_err("crossing markup ranges are rejected");
    assert!(matches!(
        error.code,
        wit_types::HostToolErrorCode::InvalidRequest
    ));
    assert_eq!(tools.send_message_calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn contained_send_message_markup_ranges_fail_before_delegating() {
    let tools = Arc::new(MockHostTools::default());
    let mut grants = HashSet::new();
    grants.insert(ExtensionCapability::HostMessageSend);
    let mut state = host_state(Arc::clone(&tools), grants);

    let result = HostToolsHost::send_message(
        &mut state,
        wit_types::SendMessageRequest {
            target: wit_types::MessageTarget::Muc(wit_types::RoomJid {
                value: "room@muc.example.com".to_string(),
            }),
            body: wit_types::DisplayText {
                value: "0123456789abcdef".to_string(),
            },
            thread_id: None,
            reply_to: None,
            markup: vec![
                wit_types::MessageMarkupSpan {
                    kind: wit_types::MessageMarkupKind::Blockquote,
                    start: 0,
                    end: 10,
                },
                wit_types::MessageMarkupSpan {
                    kind: wit_types::MessageMarkupKind::Blockquote,
                    start: 2,
                    end: 8,
                },
            ],
            extensions: None,
        },
    )
    .await
    .expect("host import does not trap");

    let error = result.expect_err("contained markup ranges are rejected");
    assert!(matches!(
        error.code,
        wit_types::HostToolErrorCode::InvalidRequest
    ));
    assert_eq!(tools.send_message_calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn requester_private_tools_are_command_only() {
    let tools = Arc::new(MockHostTools::default());
    let mut grants = HashSet::new();
    grants.insert(ExtensionCapability::HostPresenceRead);
    let mut state = host_state_with_kind(Arc::clone(&tools), grants, InvocationKind::MessageHook);

    let result = HostToolsHost::get_presence(
        &mut state,
        wit_types::GetPresenceRequest {
            subject: wit_types::BareJid {
                value: "alice@example.com".to_string(),
            },
        },
    )
    .await
    .expect("host import does not trap");

    let error = result.expect_err("message hooks cannot read requester-private presence");
    assert!(matches!(error.code, wit_types::HostToolErrorCode::Denied));
}

#[tokio::test]
async fn runtime_http_denies_unconfigured_origin_before_network() {
    let error = execute_runtime_http_request(
        wit_types::OutgoingHttpRequest {
            method: wit_types::HttpMethod::Get,
            url: wit_types::Url {
                value: "https://api.example.test/v1/chat".to_string(),
            },
            headers: Vec::new(),
            body: None,
        },
        &[],
    )
    .await
    .expect_err("origin allowlist is enforced");

    assert!(matches!(error.code, HostToolErrorCode::Denied));
}

#[tokio::test]
async fn runtime_http_caps_request_body_before_network() {
    let error = execute_runtime_http_request(
        wit_types::OutgoingHttpRequest {
            method: wit_types::HttpMethod::Post,
            url: wit_types::Url {
                value: "https://api.example.test/v1/chat".to_string(),
            },
            headers: Vec::new(),
            body: Some("x".repeat(256 * 1024 + 1)),
        },
        &["https://api.example.test".to_string()],
    )
    .await
    .expect_err("request body cap is enforced");

    assert!(matches!(error.code, HostToolErrorCode::InvalidRequest));
}

#[tokio::test]
async fn runtime_http_rejects_accept_encoding_before_network() {
    let error = execute_runtime_http_request(
        wit_types::OutgoingHttpRequest {
            method: wit_types::HttpMethod::Post,
            url: wit_types::Url {
                value: "https://api.example.test/v1/chat".to_string(),
            },
            headers: vec![wit_types::HttpHeader {
                name: "accept-encoding".to_string(),
                value: "gzip".to_string(),
            }],
            body: None,
        },
        &["https://api.example.test".to_string()],
    )
    .await
    .expect_err("accept-encoding is host-controlled");

    assert!(matches!(error.code, HostToolErrorCode::InvalidRequest));
}

#[test]
fn runtime_http_sets_identity_accept_encoding() {
    let client = reqwest::Client::new();
    let request = apply_runtime_http_headers(
        client.post("https://api.example.test/v1/chat"),
        vec![wit_types::HttpHeader {
            name: "accept".to_string(),
            value: "application/json".to_string(),
        }],
    )
    .expect("headers are valid")
    .build()
    .expect("request builds");

    assert_eq!(
        request.headers().get("accept-encoding").unwrap(),
        "identity"
    );
    assert_eq!(request.headers().get("accept").unwrap(), "application/json");
}

#[test]
fn runtime_http_normalizes_allowed_origins() {
    assert_eq!(
        normalize_http_origin("https://API.example.test/"),
        Some("https://api.example.test".to_string())
    );
    assert_eq!(
        normalize_http_origin("https://api.example.test:8443/path"),
        Some("https://api.example.test:8443".to_string())
    );
    assert_eq!(normalize_http_origin("http://api.example.test"), None);
}

#[test]
fn runtime_http_rejects_host_controlled_headers() {
    assert!(is_disallowed_extension_http_header("Host"));
    assert!(is_disallowed_extension_http_header("content-length"));
    assert!(is_disallowed_extension_http_header("Transfer-Encoding"));
    assert!(is_disallowed_extension_http_header("Accept-Encoding"));
    assert!(!is_disallowed_extension_http_header("authorization"));
    assert!(!is_disallowed_extension_http_header("content-type"));
}

fn host_state(tools: Arc<MockHostTools>, grants: HashSet<ExtensionCapability>) -> HostState {
    host_state_with_kind(tools, grants, InvocationKind::Command)
}

fn host_state_with_kind(
    tools: Arc<MockHostTools>,
    grants: HashSet<ExtensionCapability>,
    kind: InvocationKind,
) -> HostState {
    HostState::new(
        tools,
        InvocationContext {
            waddle_id: WaddleId::new("test").expect("waddle id"),
            plugin_id: PluginId::new("test-extension").expect("plugin id"),
            requester: Some("alice@example.com".parse().expect("requester jid")),
            source_room: Some("room@muc.example.com".parse().expect("room jid")),
            kind,
            provider_room_grants: Vec::new(),
        },
        "{}".to_string(),
        grants,
        Vec::new(),
    )
}

fn unsupported() -> HostToolError {
    HostToolError {
        code: HostToolErrorCode::Unsupported,
        message: DisplayText::new("unsupported").expect("display text"),
    }
}
