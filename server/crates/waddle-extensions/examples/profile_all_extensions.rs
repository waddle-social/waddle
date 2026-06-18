//! Heap profile for the full extension runtime with all 5 extensions
//! loaded. Drives N invocations of each extension's primary hot path:
//!
//! - ai-chatbot / link-board / github-enricher → message-enrich hook
//! - github → provider-webhook
//!
//! Run:
//!   cargo run --release --example profile_all_extensions \
//!       -p waddle-extensions -- [ITERS_PER_STAGE]
//!
//! Outputs:
//!   ./dhat-heap.json         (open in https://nnethercote.github.io/dh_view/dh_view.html)
//!   ./profile_extensions.log (per-stage RSS + dhat totals)

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use serde_json::json;
use waddle_extensions::{
    ExtensionCapability, ExtensionConfig, ExtensionManager, ExtensionModuleConfig, ProviderField,
    ProviderFieldText, ProviderFieldValue, ProviderPayload, ProviderWebhook,
};
use xmpp_parsers::jid::Jid;
use xmpp_parsers::message::{Id, Lang, Message, MessageType};

#[global_allocator]
static ALLOC: dhat::Alloc = dhat::Alloc;

#[derive(Debug, Clone)]
struct ExtSpec {
    name: &'static str,
    namespace: &'static str,
    wasm_filename: &'static str,
    grants: &'static [ExtensionCapability],
    provider_room_grants: &'static [&'static str],
    allowed_http_origins: &'static [&'static str],
    config: fn() -> serde_json::Value,
}

fn ai_chatbot_config() -> serde_json::Value {
    json!({
        "endpoint": "https://openrouter.ai/api/v1/chat/completions",
        "api_key": "sk-profiling-dummy",
        "model": "anthropic/claude-opus-4-7"
    })
}

fn empty_config() -> serde_json::Value {
    json!({})
}

const EXTENSIONS: &[ExtSpec] = &[
    ExtSpec {
        name: "ai-chatbot",
        namespace: "urn:waddle:ai-chatbot:1",
        wasm_filename: "ai_chatbot.wasm",
        grants: &[
            ExtensionCapability::MessageEnrich,
            ExtensionCapability::HostMamRead,
            ExtensionCapability::HostMembersRead,
            ExtensionCapability::HostPresenceRead,
            ExtensionCapability::HostRosterRead,
            ExtensionCapability::HostChannelsRead,
            ExtensionCapability::HostSpacesRead,
            ExtensionCapability::HostMessageSend,
            ExtensionCapability::OutboundHttpRequest,
            ExtensionCapability::Commands,
        ],
        provider_room_grants: &[],
        allowed_http_origins: &["https://openrouter.ai"],
        config: ai_chatbot_config,
    },
    ExtSpec {
        name: "link-board",
        namespace: "urn:waddle:link-board:1",
        wasm_filename: "link_board.wasm",
        grants: &[
            ExtensionCapability::MessageEnrich,
            ExtensionCapability::Launch,
            ExtensionCapability::PubSubPublish,
            ExtensionCapability::UiDeclarative,
        ],
        provider_room_grants: &[],
        allowed_http_origins: &[],
        config: empty_config,
    },
    // NOTE: github-enricher has no source in this workspace; the
    // .wasm in target/ is an orphaned artifact. Omitting it.
    ExtSpec {
        name: "github",
        namespace: "urn:waddle:web-integration:1",
        wasm_filename: "github.wasm",
        grants: &[
            ExtensionCapability::MessageEnrich,
            ExtensionCapability::HostMessageSend,
            ExtensionCapability::Commands,
            ExtensionCapability::PubSubPublish,
        ],
        provider_room_grants: &["general@muc.waddle.social"],
        allowed_http_origins: &[],
        config: empty_config,
    },
    ExtSpec {
        name: "decision-polls",
        namespace: "urn:waddle:decision-polls:1",
        wasm_filename: "decision_polls.wasm",
        grants: &[
            ExtensionCapability::MessageEnrich,
            ExtensionCapability::Commands,
            ExtensionCapability::Launch,
            ExtensionCapability::PubSubPublish,
            ExtensionCapability::HostMessageSend,
            ExtensionCapability::UiDeclarative,
        ],
        provider_room_grants: &[],
        allowed_http_origins: &[],
        config: empty_config,
    },
    ExtSpec {
        name: "stargate-quotes",
        namespace: "urn:waddle:stargate-quotes:1",
        wasm_filename: "stargate_quotes.wasm",
        grants: &[
            ExtensionCapability::Commands,
            ExtensionCapability::HostMessageSend,
            ExtensionCapability::MessageEnrich,
        ],
        provider_room_grants: &[],
        allowed_http_origins: &[],
        config: empty_config,
    },
];

fn wasm_root() -> PathBuf {
    let server_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|p| p.parent())
        .expect("crate is two levels below server root")
        .to_path_buf();
    server_dir.join("target/wasm32-wasip2/release")
}

fn build_module(spec: &ExtSpec, root: &std::path::Path) -> ExtensionModuleConfig {
    let path = root.join(spec.wasm_filename);
    if !path.exists() {
        panic!(
            "missing built wasm at {}; run `just build-extensions` or `cargo build --target wasm32-wasip2 --release` for the extension crate",
            path.display(),
        );
    }
    ExtensionModuleConfig {
        name: spec.name.to_string(),
        registry: String::new(),
        digest: None,
        tag: None,
        namespace: spec.namespace.to_string(),
        config: (spec.config)(),
        capability_grants: spec.grants.to_vec(),
        allowed_http_origins: spec
            .allowed_http_origins
            .iter()
            .map(|s| s.to_string())
            .collect(),
        provider_room_grants: spec
            .provider_room_grants
            .iter()
            .map(|s| s.to_string())
            .collect(),
        config_secret_files: Default::default(),
        local_path: Some(path.to_string_lossy().into_owned()),
    }
}

fn rss_bytes() -> usize {
    memory_stats::memory_stats()
        .map(|s| s.physical_mem)
        .unwrap_or(0)
}

fn fmt_mb(b: usize) -> String {
    format!("{:.1} MB", b as f64 / (1024.0 * 1024.0))
}

fn build_message(idx: usize) -> Message {
    let to: Jid = "general@muc.waddle.social/test".parse().unwrap();
    let from: Jid = format!("user{}@waddle.social/r1", idx % 50)
        .parse()
        .unwrap();
    let body = format!(
        "msg {} https://github.com/waddle-social/waddle/pull/{} check this out: https://example.com/a/{}",
        idx, idx, idx
    );
    let mut msg = Message::new(Some(to));
    msg.from = Some(from);
    msg.type_ = MessageType::Groupchat;
    msg.bodies.insert(Lang(String::new()), body);
    msg.id = Some(Id(format!("origin-{idx}")));
    msg
}

fn build_provider_webhook(idx: usize) -> ProviderWebhook {
    use waddle_extensions::{
        ProviderDeliveryId, ProviderEventType, ProviderFieldName, ProviderId, WaddleId,
    };
    ProviderWebhook {
        waddle_id: WaddleId::new("local").unwrap(),
        provider: ProviderId::new("github").unwrap(),
        event_type: ProviderEventType::new("issues").unwrap(),
        delivery_id: ProviderDeliveryId::new(format!("delivery-{idx}")).unwrap(),
        payload: ProviderPayload {
            fields: vec![
                ProviderField {
                    path: vec![ProviderFieldName::new("action").unwrap()],
                    value: ProviderFieldValue::Text(ProviderFieldText::new("opened").unwrap()),
                },
                ProviderField {
                    path: vec![
                        ProviderFieldName::new("issue").unwrap(),
                        ProviderFieldName::new("title").unwrap(),
                    ],
                    value: ProviderFieldValue::Text(
                        ProviderFieldText::new(format!("issue #{idx}: something")).unwrap(),
                    ),
                },
                ProviderField {
                    path: vec![
                        ProviderFieldName::new("issue").unwrap(),
                        ProviderFieldName::new("html_url").unwrap(),
                    ],
                    value: ProviderFieldValue::Text(
                        ProviderFieldText::new(format!(
                            "https://github.com/waddle-social/waddle/issues/{idx}"
                        ))
                        .unwrap(),
                    ),
                },
            ],
        },
    }
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> anyhow::Result<()> {
    let iters: usize = std::env::args()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(500);

    let _profiler = dhat::Profiler::builder()
        .file_name("dhat-heap.json")
        .build();

    let rss_start = rss_bytes();
    eprintln!("[start] rss={}", fmt_mb(rss_start));

    let root = wasm_root();
    eprintln!("[load ] wasm root: {}", root.display());

    let cfg = ExtensionConfig {
        enabled: true,
        cache_dir: "/tmp/waddle-ext-cache-profile".to_string(),
        modules: EXTENSIONS.iter().map(|s| build_module(s, &root)).collect(),
    };
    let t0 = Instant::now();
    let manager = Arc::new(ExtensionManager::from_config(cfg).await?);
    let rss_after_load = rss_bytes();
    eprintln!(
        "[load ] {} extensions loaded in {:.2}s rss={} (Δ {})",
        EXTENSIONS.len(),
        t0.elapsed().as_secs_f64(),
        fmt_mb(rss_after_load),
        fmt_mb(rss_after_load.saturating_sub(rss_start)),
    );

    let waddle_id = waddle_extensions::WaddleId::new("local").unwrap();
    let requester: Option<xmpp_parsers::jid::BareJid> =
        Some("alice@waddle.social".parse().unwrap());

    // Stage 1: message-enrich hot path (the most-called code path in production)
    let t1 = Instant::now();
    for i in 0..iters {
        let mut msg = build_message(i);
        let _ = manager
            .process_message_for_waddle_with_requester(
                &mut msg,
                waddle_id.clone(),
                requester.clone(),
            )
            .await;
        if i > 0 && i % 100 == 0 {
            eprintln!(
                "  enrich  i={i:>5}/{iters} rss={} t={:.1}s",
                fmt_mb(rss_bytes()),
                t1.elapsed().as_secs_f64()
            );
        }
    }
    let rss_after_enrich = rss_bytes();
    eprintln!(
        "[stage] enrich  x{iters} done in {:.2}s rss={} (Δ {} since load)",
        t1.elapsed().as_secs_f64(),
        fmt_mb(rss_after_enrich),
        fmt_mb(rss_after_enrich.saturating_sub(rss_after_load)),
    );

    // Stage 2: provider webhook (github extension)
    let t2 = Instant::now();
    for i in 0..iters {
        let event = build_provider_webhook(i);
        let _ = manager.invoke_provider_webhook("github", event).await;
        if i > 0 && i % 100 == 0 {
            eprintln!(
                "  webhook i={i:>5}/{iters} rss={} t={:.1}s",
                fmt_mb(rss_bytes()),
                t2.elapsed().as_secs_f64()
            );
        }
    }
    let rss_after_webhook = rss_bytes();
    eprintln!(
        "[stage] webhook x{iters} done in {:.2}s rss={} (Δ {} since enrich)",
        t2.elapsed().as_secs_f64(),
        fmt_mb(rss_after_webhook),
        fmt_mb(rss_after_webhook.saturating_sub(rss_after_enrich)),
    );

    drop(manager);
    let rss_after_drop = rss_bytes();
    eprintln!(
        "[final] after drop(manager) rss={} (Δ {} since webhook)",
        fmt_mb(rss_after_drop),
        fmt_mb(rss_after_drop.saturating_sub(rss_after_webhook)),
    );

    let summary = json!({
        "iters_per_stage": iters,
        "rss_bytes": {
            "start": rss_start,
            "after_load": rss_after_load,
            "after_enrich": rss_after_enrich,
            "after_webhook": rss_after_webhook,
            "after_drop": rss_after_drop,
        },
        "rss_growth_bytes": {
            "load_step": rss_after_load.saturating_sub(rss_start),
            "enrich_total": rss_after_enrich.saturating_sub(rss_after_load),
            "enrich_per_call": (rss_after_enrich.saturating_sub(rss_after_load)) / iters.max(1),
            "webhook_total": rss_after_webhook.saturating_sub(rss_after_enrich),
            "webhook_per_call": (rss_after_webhook.saturating_sub(rss_after_enrich)) / iters.max(1),
            "freed_on_drop": rss_after_webhook.saturating_sub(rss_after_drop),
        }
    });
    std::fs::write(
        "profile_extensions.log",
        serde_json::to_string_pretty(&summary).unwrap(),
    )?;
    eprintln!("wrote profile_extensions.log and dhat-heap.json");

    Ok(())
}
