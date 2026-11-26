//! T1-F08-03: `CommandRegistry` isolates multi-step sessions by `(TenantId, session_id)`.

use std::sync::Arc;
use std::time::Duration;

use waddle_broker_core::boot_clock::{BootClock, FakeBootClock};
use waddle_broker_core::identity::TenantId;
use waddle_xmpp::commands::{Action, Command, CommandContext, CommandRegistry, CommandResult};
use waddle_xmpp::xep::xep0004::{DataForm, FormType};
use waddle_xmpp::xep::xep0050::AdHocCommandCondition;
use waddle_xmpp::XmppError;
use xmpp_parsers::iq::Iq;

fn command_ctx(tenant_id: TenantId, action: Action, session_id: Option<String>) -> CommandContext {
    let mut cmd = Command::new("urn:waddle:wizard").with_action(action);
    if let Some(sid) = session_id {
        cmd = cmd.with_session_id(sid);
    }
    CommandContext {
        from: "alice@example.org/web".parse().expect("valid jid"),
        authenticated_user_id: Some("alice@example.org".to_owned()),
        iq: Iq::Set {
            from: None,
            to: None,
            id: "iq-cmd-1".to_owned(),
            payload: "<query xmlns='urn:test'/>".parse().expect("xml"),
        },
        command: cmd,
        tenant_id: Some(tenant_id),
        placement_generation: Some(1),
        cancel_rx: None,
        chunk_tx: None,
    }
}

#[tokio::test]
async fn command_registry_isolates_multistep_sessions_by_tenant_and_session_id() {
    let clock = Arc::new(FakeBootClock::new(Duration::from_secs(100)));
    let tenant_acme = TenantId::parse("acme.example").expect("valid acme tenant");
    let tenant_globex = TenantId::parse("globex.example").expect("valid globex tenant");

    let reg_acme = CommandRegistry::for_tenant_with_clock(
        tenant_acme.clone(),
        Some(1),
        Arc::clone(&clock) as Arc<dyn BootClock>,
    );
    let reg_globex = CommandRegistry::for_tenant_with_clock(
        tenant_globex.clone(),
        Some(1),
        clock as Arc<dyn BootClock>,
    );

    for registry in [&reg_acme, &reg_globex] {
        registry
            .register(
                "urn:waddle:wizard",
                "Wizard",
                |ctx: CommandContext| async move {
                    match ctx.command.action {
                        Some(Action::Execute) => CommandResult::Executing {
                            form: DataForm::new(FormType::Form),
                            session_id: ctx.command.session_id.unwrap_or_default(),
                            notes: vec![],
                            actions: None,
                        },
                        _ => CommandResult::Completed {
                            session_id: ctx.command.session_id,
                            form: None,
                            notes: vec![],
                        },
                    }
                },
            )
            .await;
    }

    let sid_acme = match reg_acme
        .dispatch(command_ctx(tenant_acme.clone(), Action::Execute, None))
        .await
    {
        CommandResult::Executing { session_id, .. } => session_id,
        _ => panic!("expected acme executing session"),
    };
    let sid_globex = match reg_globex
        .dispatch(command_ctx(tenant_globex.clone(), Action::Execute, None))
        .await
    {
        CommandResult::Executing { session_id, .. } => session_id,
        _ => panic!("expected globex executing session"),
    };

    // Wrong tenant context rejected even with valid session ID on acme's registry.
    let cross_ctx = reg_acme
        .dispatch(command_ctx(
            tenant_globex.clone(),
            Action::Next,
            Some(sid_acme.clone()),
        ))
        .await;
    assert!(
        matches!(cross_ctx, CommandResult::Error(_)),
        "expected cross-tenant context error"
    );

    // Acme's session ID does not exist in Globex's registry.
    let cross_reg = reg_globex
        .dispatch(command_ctx(
            tenant_globex.clone(),
            Action::Next,
            Some(sid_acme.clone()),
        ))
        .await;
    assert!(matches!(
        cross_reg,
        CommandResult::Error(XmppError::AdHocCommand {
            condition: AdHocCommandCondition::BadSessionId,
            ..
        })
    ));

    // Evicting Acme removes Acme's session while leaving Globex's session intact.
    assert_eq!(reg_acme.evict_tenant(&tenant_acme).await, 1);
    let acme_after_evict = reg_acme
        .dispatch(command_ctx(tenant_acme, Action::Next, Some(sid_acme)))
        .await;
    assert!(matches!(
        acme_after_evict,
        CommandResult::Error(XmppError::AdHocCommand {
            condition: AdHocCommandCondition::SessionExpired,
            ..
        })
    ));

    let globex_finish = reg_globex
        .dispatch(command_ctx(tenant_globex, Action::Next, Some(sid_globex)))
        .await;
    assert!(matches!(globex_finish, CommandResult::Completed { .. }));
}
