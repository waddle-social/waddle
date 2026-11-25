use std::sync::Arc;

use jid::BareJid;
use waddle_xmpp::commands::{CommandContext, CommandRegistry, CommandResult};
use waddle_xmpp::xep::xep0004::{DataForm, Field, FormType};
use waddle_xmpp::xep::xep0050::{Action, AdHocCommandCondition};
use waddle_xmpp::XmppError;
use waddle_xmpp_core::calendar_subscription::{ISSUE, REVOKE, URL_FIELD};

use super::routes::calendar_feed::{CalendarFeedState, CalendarSubscriptionError};

#[derive(Clone, Copy)]
enum Operation {
    Issue,
    Revoke,
}

pub(super) async fn register(registry: &CommandRegistry, state: Arc<CalendarFeedState>) {
    for (node, title, operation) in [
        (ISSUE, "Get private calendar subscription", Operation::Issue),
        (
            REVOKE,
            "Revoke private calendar subscription",
            Operation::Revoke,
        ),
    ] {
        let state = Arc::clone(&state);
        registry
            .register(node, title, move |ctx| {
                let state = Arc::clone(&state);
                async move { execute(operation, &state, ctx).await }
            })
            .await;
    }
}

async fn execute(
    operation: Operation,
    state: &CalendarFeedState,
    ctx: CommandContext,
) -> CommandResult {
    // The C2S dispatcher supplies the bound sender and the authenticated session independently.
    let owner = ctx.from.to_bare();
    if owner.node().is_none()
        || ctx
            .authenticated_user_id
            .as_deref()
            .and_then(|s| s.parse::<BareJid>().ok())
            .as_ref()
            != Some(&owner)
    {
        return CommandResult::Error(XmppError::not_authorized(None));
    }
    if !ctx.iq.to().is_some_and(|to| {
        to.node().is_none() && to.resource().is_none() && to.domain() == owner.domain()
    }) {
        return CommandResult::Error(XmppError::service_unavailable(None));
    }
    if !matches!(ctx.command.action, None | Some(Action::Execute)) {
        return CommandResult::Error(XmppError::ad_hoc_command(
            AdHocCommandCondition::BadAction,
            None,
        ));
    }
    if ctx.command.status.is_some()
        || ctx.command.form.is_some()
        || ctx.command.actions.is_some()
        || !ctx.command.notes.is_empty()
    {
        return CommandResult::Error(XmppError::bad_request(Some(
            "This command takes no arguments".into(),
        )));
    }
    let result = match operation {
        Operation::Issue => state.subscription_url(&owner).await.map(|url| {
            Some(
                DataForm::new(FormType::Result)
                    .add_field(Field::form_type(ISSUE))
                    .add_field(Field::text_single(URL_FIELD, url.as_str())),
            )
        }),
        Operation::Revoke => state.revoke_subscription(&owner).await.map(|()| None),
    };
    match result {
        Ok(form) => CommandResult::Completed {
            session_id: ctx.command.session_id,
            form,
            notes: Vec::new(),
        },
        Err(CalendarSubscriptionError::Forbidden) => {
            CommandResult::Error(XmppError::forbidden(None))
        }
        Err(CalendarSubscriptionError::Unavailable) => CommandResult::Error(
            XmppError::internal_server_error(Some("Calendar subscription unavailable".into())),
        ),
    }
}
