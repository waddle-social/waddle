use std::sync::Arc;

use kameo::actor::Spawn;
use waddle_extensions::manager::CommandInvocationRequest;
use waddle_extensions::{CommandAction, ExtensionEffect, ExtensionManager, FullJidValue, WaddleId};
use waddle_xmpp::commands::{CommandContext, CommandRegistry, CommandResult, Note};
use waddle_xmpp::registry::{
    ConnectionEntry, ConnectionRegistry, LocalConnectionBindingId, OutboundStanza,
    RegisterUserResource, UserRegistryActor,
};
use waddle_xmpp::stream_management::persistence::InMemorySmPersistence;
use waddle_xmpp::stream_management::InMemorySmSessionRegistry;
use waddle_xmpp::xep::{build_command_result, Command, CommandStatus};
use waddle_xmpp::Stanza;
use xmpp_parsers::iq::Iq;
use xmpp_parsers::minidom::Element;

use crate::server::routes::interpret::deliver_direct_to_full;
use crate::server::routes::websocket::frame_backstop::{
    run_with_backstop, spawn_decoupled_streaming_inference, StanzaBackstop, StanzaTimeout,
    StreamingInferenceSession,
};

/// Exercises the live XEP-0050 decoupled extension command path
/// (`CommandRegistry` -> `ExtensionManager::invoke_command_decoupled` ->
/// `spawn_decoupled_streaming_inference` -> `deliver_direct_to_full`) inside
/// the 15s `STANZA_HANDLER_WEDGE_TIMEOUT` backstop.
#[doc(hidden)]
pub async fn run_extension_command_decoupled_for_test(
    extension_manager: Arc<ExtensionManager>,
    node: String,
    requester: jid::FullJid,
    shutdown: tokio_util::sync::CancellationToken,
) -> (
    Result<Vec<String>, StanzaTimeout>,
    tokio::sync::mpsc::Receiver<OutboundStanza>,
    StreamingInferenceSession,
) {
    let registry = Arc::new(CommandRegistry::new());
    let manager_for_cmd = Arc::clone(&extension_manager);
    let node_for_cmd = node.clone();
    registry
        .register(node.clone(), "Streaming AI", move |ctx| {
            let mgr = Arc::clone(&manager_for_cmd);
            let n = node_for_cmd.clone();
            async move {
                let (_tx, cancel_rx) = tokio::sync::watch::channel(false);
                let effects = mgr
                    .invoke_command_decoupled(
                        CommandInvocationRequest {
                            node: &n,
                            waddle_id: WaddleId::try_new(ctx.from.to_string()).expect("waddle"),
                            room: None,
                            requester: FullJidValue::try_new(ctx.from.to_string()).expect("full"),
                            session_id: None,
                            action: Some(CommandAction::Execute),
                            fields: Vec::new(),
                            form: None,
                        },
                        cancel_rx,
                    )
                    .await;
                let notes = effects
                    .into_iter()
                    .filter_map(|e| match e {
                        ExtensionEffect::HostWarning(t) => Some(Note::info(t.as_str().to_owned())),
                        _ => None,
                    })
                    .collect();
                CommandResult::Completed {
                    session_id: None,
                    form: None,
                    notes,
                }
            }
        })
        .await;

    let conn_registry = Arc::new(ConnectionRegistry::new());
    let (outbound_tx, outbound_rx) = tokio::sync::mpsc::channel(16);
    let _binding_id: LocalConnectionBindingId =
        conn_registry.register(requester.clone(), outbound_tx);
    let entry: ConnectionEntry = conn_registry.entry(&requester).expect("entry");
    let user_registry = UserRegistryActor::spawn(UserRegistryActor::new());
    user_registry
        .ask(RegisterUserResource {
            jid: requester.clone(),
            entry,
        })
        .await
        .expect("register user resource");
    let sm_registry = Arc::new(
        InMemorySmSessionRegistry::new().with_persistence(Arc::new(InMemorySmPersistence::new())),
    );

    let req_iq = Iq::Set {
        id: "decoupled-iq-1".to_owned(),
        from: Some(jid::Jid::from(requester.clone())),
        to: Some("extensions.acme.example".parse::<jid::Jid>().expect("to")),
        payload: Element::builder("command", "http://jabber.org/protocol/commands").build(),
    };

    let mut session_slot: Option<StreamingInferenceSession> = None;
    let backstop = StanzaBackstop::capture(&Stanza::Iq(Box::new(req_iq.clone())), Some(&requester));

    let dispatch_res = run_with_backstop(backstop, async {
        let ctx = CommandContext {
            from: jid::Jid::from(requester.clone()),
            authenticated_user_id: Some(requester.to_bare().to_string()),
            iq: req_iq.clone(),
            command: waddle_xmpp::xep::Command::new(node.clone()),
            tenant_id: None,
            placement_generation: None,
        };
        let reg = Arc::clone(&registry);
        let ur = user_registry.clone();
        let smr = Arc::clone(&sm_registry);
        let req_for_task = req_iq.clone();
        let requester_for_task = requester.clone();
        let node_for_task = node.clone();
        let (session, _handle) = spawn_decoupled_streaming_inference(
            format!("test-ext-cmd:{}", req_iq.id()),
            16,
            shutdown,
            move |_chunk_tx, mut cancel_rx| async move {
                let result = tokio::select! {
                    biased;
                    _ = cancel_rx.changed() => CommandResult::Canceled {
                        session_id: None,
                        notes: vec![Note::info("Canceled".to_owned())],
                    },
                    res = reg.dispatch(ctx) => res,
                };
                let mut cmd = Command::new(node_for_task);
                match result {
                    CommandResult::Completed { notes, .. } => {
                        cmd.status = Some(CommandStatus::Completed);
                        cmd.notes = notes;
                    }
                    CommandResult::Canceled { notes, .. } => {
                        cmd.status = Some(CommandStatus::Canceled);
                        cmd.notes = notes;
                    }
                    _ => {
                        cmd.status = Some(CommandStatus::Completed);
                    }
                }
                let reply = Stanza::Iq(Box::new(build_command_result(&req_for_task, &cmd)));
                let _ = deliver_direct_to_full(
                    Some(&ur),
                    Some(&smr),
                    &requester_for_task,
                    &reply,
                    None,
                )
                .await;
            },
        );
        session_slot = Some(session);
        Vec::new()
    })
    .await;

    (
        dispatch_res,
        outbound_rx,
        session_slot.expect("session created"),
    )
}
