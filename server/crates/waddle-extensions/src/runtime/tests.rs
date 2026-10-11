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
        &super::http::HttpRuntime::new().expect("HTTP runtime"),
        &crate::config::RuntimeLimits::default(),
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
        &super::http::HttpRuntime::new().expect("HTTP runtime"),
        &crate::config::RuntimeLimits::default(),
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
        &super::http::HttpRuntime::new().expect("HTTP runtime"),
        &crate::config::RuntimeLimits::default(),
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
        crate::config::RuntimeLimits::default(),
        super::http::HttpRuntime::new().expect("HTTP runtime"),
    )
}

fn unsupported() -> HostToolError {
    HostToolError {
        code: HostToolErrorCode::Unsupported,
        message: DisplayText::new("unsupported").expect("display text"),
    }
}

#[tokio::test]
async fn observation_send_is_denied_even_with_a_command_send_grant() {
    let tools = Arc::new(MockHostTools::default());
    let mut state = host_state_with_kind(
        tools.clone(),
        HashSet::from([ExtensionCapability::HostMessageSend]),
        InvocationKind::RoomMessageObserve,
    );
    let result = HostToolsHost::send_message(
        &mut state,
        wit_types::SendMessageRequest {
            target: wit_types::MessageTarget::Muc(wit_types::RoomJid {
                value: "room@muc.example.com".into(),
            }),
            body: wit_types::DisplayText {
                value: "must not send".into(),
            },
            thread_id: None,
            reply_to: None,
            markup: vec![],
            extensions: None,
        },
    )
    .await
    .expect("host call");
    assert!(matches!(
        result.expect_err("observation must not mutate").code,
        wit_types::HostToolErrorCode::Denied
    ));
    assert_eq!(tools.send_message_calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn runtime_http_uses_the_configured_body_limit_before_network() {
    let limits = crate::config::RuntimeLimits {
        http_max_request_bytes: 4,
        ..Default::default()
    };
    let error = execute_runtime_http_request(
        wit_types::OutgoingHttpRequest {
            method: wit_types::HttpMethod::Post,
            url: wit_types::Url {
                value: "https://api.example.test/".into(),
            },
            headers: vec![],
            body: Some("12345".into()),
        },
        &["https://api.example.test".into()],
        &super::http::HttpRuntime::new().expect("HTTP runtime"),
        &limits,
    )
    .await
    .expect_err("configured request limit");
    assert_eq!(error.code, HostToolErrorCode::InvalidRequest);
}

struct ScopedDelivery {
    active: std::sync::atomic::AtomicBool,
}

#[async_trait]
impl host_domain::ExtensionDeliveryCapability for ScopedDelivery {
    async fn validate(
        &self,
        context: &InvocationContext,
        source: &crate::types::RoomMessageSource,
    ) -> Result<(), HostToolError> {
        if self.active.load(Ordering::SeqCst)
            && context.kind == InvocationKind::RoomMessageObserve
            && context.plugin_id.as_str() == "test-extension"
            && context.source_room.as_ref() == Some(&source.room)
        {
            Ok(())
        } else {
            Err(HostToolError::denied(
                DisplayText::new("stale or foreign delivery").expect("text"),
            ))
        }
    }
}

fn delivery_source() -> crate::types::RoomMessageSource {
    use crate::types::*;
    RoomMessageSource {
        room: "room@muc.example.com".parse().expect("room"),
        stanza_id: StanzaId::new("source").expect("id"),
        revision_stanza_id: StanzaId::new("revision").expect("id"),
        origin_id: Some(OriginId::new("origin").expect("id")),
        sender: "alice@example.com".parse().expect("sender"),
        revision: MessageRevision::new(0),
        body_digest: Sha256Digest::new("a".repeat(64)).expect("digest"),
        observed_at: Timestamp::new("2026-10-09T00:00:00Z").expect("time"),
    }
}

#[tokio::test]
async fn delivery_resource_revalidates_authority_before_host_effects() {
    let capability = Arc::new(ScopedDelivery {
        active: true.into(),
    });
    let mut state = host_state_with_kind(
        Arc::new(MockHostTools::default()),
        HashSet::from([ExtensionCapability::OutboundHttpRequest]),
        InvocationKind::RoomMessageObserve,
    );
    state
        .bind_delivery(capability.clone(), delivery_source())
        .await
        .expect("issue delivery");
    state.validate_delivery().await.expect("current delivery");
    capability.active.store(false, Ordering::SeqCst);
    let denied = state.validate_delivery().await.expect_err("lease expired");
    assert_eq!(denied.code, HostToolErrorCode::Denied);
    use super::waddle::extension::runtime::Host;
    let response = state
        .http_request(wit_types::OutgoingHttpRequest {
            method: wit_types::HttpMethod::Post,
            url: wit_types::Url {
                value: "https://provider.example/request".into(),
            },
            headers: vec![],
            body: None,
        })
        .await
        .expect("host call");
    assert_eq!(
        response
            .expect_err("stale delivery denies before HTTP")
            .code,
        wit_types::HostToolErrorCode::Denied
    );
}

#[tokio::test]
async fn delivery_resource_rejects_foreign_source_or_invocation() {
    let capability = Arc::new(ScopedDelivery {
        active: true.into(),
    });
    let mut state = host_state_with_kind(
        Arc::new(MockHostTools::default()),
        HashSet::new(),
        InvocationKind::Command,
    );
    assert!(state
        .bind_delivery(capability.clone(), delivery_source())
        .await
        .is_err());
    let mut state = host_state_with_kind(
        Arc::new(MockHostTools::default()),
        HashSet::new(),
        InvocationKind::RoomMessageObserve,
    );
    let mut source = delivery_source();
    source.room = "foreign@muc.example.com".parse().expect("room");
    assert!(state.bind_delivery(capability, source).await.is_err());
}

#[tokio::test]
async fn wasm_delivery_resource_is_issued_borrowed_and_cannot_be_forged_or_retained() {
    use super::DeliveryKey;
    use wasmtime::component::{Component, HasSelf, Linker, Resource};
    use wasmtime::Store;
    let runtime = super::WasmRuntime::new().expect("runtime");
    let component = Component::new(
        runtime.engine(),
        include_bytes!("../../tests/fixtures/delivery_resource.wat"),
    )
    .expect("resource component");
    let mut linker = Linker::<HostState>::new(runtime.engine());
    super::waddle::extension::delivery::add_to_linker::<_, HasSelf<_>>(&mut linker, |state| state)
        .expect("link delivery");
    let capability = Arc::new(ScopedDelivery {
        active: true.into(),
    });
    let mut state = host_state_with_kind(
        Arc::new(MockHostTools::default()),
        HashSet::new(),
        InvocationKind::RoomMessageObserve,
    );
    state
        .bind_delivery(capability.clone(), delivery_source())
        .await
        .expect("host issues key");
    let mut store = Store::new(runtime.engine(), state);
    store.set_fuel(1_000_000).expect("fuel");
    let instance = linker
        .instantiate_async(&mut store, &component)
        .await
        .expect("instantiate");
    let check = instance
        .get_typed_func::<(Resource<DeliveryKey>,), (bool,)>(&mut store, "check")
        .expect("typed check");
    let issued = store.data().delivery_resource().expect("key");
    assert_eq!(
        check
            .call_async(&mut store, (issued,))
            .await
            .expect("issued check"),
        (true,)
    );
    capability.active.store(false, Ordering::SeqCst);
    let issued = store.data().delivery_resource().expect("key");
    assert_eq!(
        check
            .call_async(&mut store, (issued,))
            .await
            .expect("expired check"),
        (false,)
    );
    let reuse = instance
        .get_typed_func::<(), (bool,)>(&mut store, "reuse")
        .expect("reuse");
    assert!(
        reuse.call_async(&mut store, ()).await.is_err(),
        "copied guest borrow cannot outlive invocation"
    );
    // A trap poisons a component instance. A fresh instance proves a fabricated
    // integer cannot become a resource of the host-defined nominal type.
    let instance = linker
        .instantiate_async(&mut store, &component)
        .await
        .expect("fresh instance");
    let forge = instance
        .get_typed_func::<(), (bool,)>(&mut store, "forge")
        .expect("forge");
    assert!(
        forge.call_async(&mut store, ()).await.is_err(),
        "guest cannot forge a delivery key"
    );
}

#[tokio::test]
async fn wasm_observer_receives_delivery_without_claiming_provider_acceptance() {
    let runtime = super::WasmRuntime::new().expect("runtime");
    let extension = super::LoadedExtension::load(
        &runtime,
        std::path::Path::new(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/message_hook.wat"
        )),
    )
    .expect("load fixture");
    let result = extension
        .call_handle_event_typed_with_delivery(
            crate::types::ExtensionEvent::RoomMessageObserve(crate::types::RoomMessageObserve {
                source: delivery_source(),
                body: DisplayText::new("hello").expect("body"),
            }),
            Arc::new(MockHostTools::default()),
            host_domain::DeliveryInvocation {
                context: InvocationContext {
                    waddle_id: WaddleId::new("test").expect("waddle"),
                    plugin_id: PluginId::new("test-extension").expect("plugin"),
                    requester: None,
                    source_room: Some(delivery_source().room),
                    kind: InvocationKind::RoomMessageObserve,
                    provider_room_grants: Vec::new(),
                },
                capability: Arc::new(ScopedDelivery {
                    active: true.into(),
                }),
            },
            "0".into(),
            HashSet::new(),
            Vec::new(),
        )
        .await;
    assert!(
        result.is_ok(),
        "host-issued authority survives actual framework invocation: {result:?}"
    );
}
