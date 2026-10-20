//! Bounded operator push inventory and explicit retirement over XEP-0050.
//! Both commands require the community-owner role and one exact local owner
//! and node. Retirement never replays or automatically discards uncertain work.

use std::sync::Arc;

use jid::{BareJid, DomainPart};
use waddle_xmpp::commands::{CommandContext, CommandRegistry, CommandResult};
use waddle_xmpp::xep::xep0004::{DataForm, Field, FieldType, FormType};
use waddle_xmpp::xep::xep0050::{Action, AdHocCommandCondition, AllowedActions};
use waddle_xmpp::XmppError;

use crate::push_service::{DatabasePushServiceStore, PushOperatorInventory, PushOperatorNode};
use crate::server::AppState;

pub const INSPECT_NODE: &str = waddle_xmpp::admin::NS_ADMIN_PUSH_INSPECT;
pub const RETIRE_NODE: &str = waddle_xmpp::admin::NS_ADMIN_PUSH_RETIRE;

#[derive(Clone, Copy)]
enum Operation {
    Inspect,
    Retire,
}

impl Operation {
    fn node(self) -> &'static str {
        match self {
            Self::Inspect => INSPECT_NODE,
            Self::Retire => RETIRE_NODE,
        }
    }
}

struct Target {
    owner: BareJid,
    node: PushOperatorNode,
}

pub async fn register(
    registry: &CommandRegistry,
    state: Arc<AppState>,
    push: Arc<DatabasePushServiceStore>,
    domain: DomainPart,
) {
    for (operation, name) in [
        (Operation::Inspect, "Admin · Inspect push node"),
        (Operation::Retire, "Admin · Retire push node"),
    ] {
        let state = Arc::clone(&state);
        let push = Arc::clone(&push);
        let domain = domain.clone();
        registry
            .register(operation.node(), name, move |ctx| {
                let state = Arc::clone(&state);
                let push = Arc::clone(&push);
                let domain = domain.clone();
                async move { handle(ctx, &state, &push, &domain, operation).await }
            })
            .await;
    }
}

async fn handle(
    ctx: CommandContext,
    state: &AppState,
    push: &DatabasePushServiceStore,
    domain: &DomainPart,
    operation: Operation,
) -> CommandResult {
    // ctx.from is the transport-bound authenticated JID, never the wire 'from'.
    let actor = ctx.from.to_bare();
    if !super::is_community_owner(state, &actor).await {
        return CommandResult::Error(XmppError::forbidden(Some(
            "Admin commands require the community owner role".into(),
        )));
    }
    if matches!(ctx.command.action, Some(Action::Next | Action::Prev)) {
        return CommandResult::Error(XmppError::ad_hoc_command(
            AdHocCommandCondition::BadAction,
            Some("This command only supports execute and complete".into()),
        ));
    }
    let Some(form) = ctx.command.form.as_ref() else {
        if ctx.command.action == Some(Action::Complete) {
            return CommandResult::Error(invalid_form("Complete requires a submitted form"));
        }
        return CommandResult::Executing {
            form: request_form(operation),
            session_id: ctx.command.session_id.unwrap_or_default(),
            notes: vec![],
            actions: Some(AllowedActions::new(Action::Complete).with_complete()),
        };
    };
    let target = match parse_target(form, domain, operation) {
        Ok(target) => target,
        Err(error) => return CommandResult::Error(error),
    };
    let mut result = DataForm::new(FormType::Result)
        .add_field(Field::form_type(operation.node()))
        .add_field(Field::new("owner", FieldType::JidSingle).with_value(target.owner.to_string()))
        .add_field(Field::text_single("node", target.node.as_str()));
    match operation {
        Operation::Inspect => match push.operator_inventory(&target.owner, &target.node).await {
            Ok(inventory) => {
                tracing::info!(%actor, owner = %target.owner, node = target.node.as_str(),
                    status = inventory.status.as_str(), "operator inspected push node inventory");
                result = inventory_form(result, inventory);
            }
            Err(error) => {
                tracing::warn!(%actor, owner = %target.owner, node = target.node.as_str(), %error,
                    "operator push node inventory failed");
                return CommandResult::Error(error);
            }
        },
        Operation::Retire => {
            match push
                .disable_nodes_for_owner(&target.owner, Some(target.node.as_str()))
                .await
            {
                Ok(disabled_devices) => {
                    tracing::info!(%actor, owner = %target.owner, node = target.node.as_str(), disabled_devices,
                        "operator explicitly retired push node");
                    result = result
                        .add_field(Field::text_single("status", "retired"))
                        .add_field(Field::text_single(
                            "disabled_devices",
                            disabled_devices.to_string(),
                        ));
                }
                Err(error) => {
                    tracing::warn!(%actor, owner = %target.owner, node = target.node.as_str(), %error,
                        "operator push retirement rolled back; retry after contention clears");
                    return CommandResult::Error(error);
                }
            }
        }
    }
    CommandResult::Completed {
        session_id: ctx.command.session_id,
        form: Some(result),
        notes: vec![],
    }
}

fn request_form(operation: Operation) -> DataForm {
    let mut form = DataForm::new(FormType::Form)
        .add_field(Field::form_type(operation.node()))
        .add_field(Field::new("owner", FieldType::JidSingle).with_required())
        .add_field(Field::new("node", FieldType::TextSingle).with_required());
    if matches!(operation, Operation::Retire) {
        form = form.add_instructions("Retirement disables every device on this exact node and releases pending notification custody. It cannot replay those notifications.")
            .add_field(Field::boolean("confirm", false).with_required());
    }
    form
}

fn parse_target(
    form: &DataForm,
    domain: &DomainPart,
    operation: Operation,
) -> Result<Target, XmppError> {
    if form.form_type != FormType::Submit || scalar(form, "FORM_TYPE")? != operation.node() {
        return Err(invalid_form("A matching submit FORM_TYPE is required"));
    }
    let owner: BareJid = scalar(form, "owner")?
        .parse()
        .map_err(|_| invalid_form("owner must be a bare JID"))?;
    if owner.node().is_none() || owner.domain() != &**domain {
        return Err(invalid_form("owner must be a local user bare JID"));
    }
    let node = PushOperatorNode::parse(scalar(form, "node")?)?;
    if matches!(operation, Operation::Retire) && !matches!(scalar(form, "confirm")?, "true" | "1") {
        return Err(invalid_form(
            "Explicit confirm=true is required for retirement",
        ));
    }
    Ok(Target { owner, node })
}

fn scalar<'a>(form: &'a DataForm, name: &str) -> Result<&'a str, XmppError> {
    let mut fields = form
        .fields
        .iter()
        .filter(|field| field.var.as_deref() == Some(name));
    let field = fields
        .next()
        .ok_or_else(|| invalid_form("A required field is missing"))?;
    if fields.next().is_some() || field.values.len() != 1 {
        return Err(invalid_form(
            "Required scalar fields must occur once with one value",
        ));
    }
    field
        .value()
        .ok_or_else(|| invalid_form("A required field has no value"))
}

fn invalid_form(message: &str) -> XmppError {
    XmppError::bad_request(Some(message.to_owned()))
}

fn inventory_form(mut form: DataForm, inventory: PushOperatorInventory) -> DataForm {
    form = form.add_field(Field::text_single("status", inventory.status.as_str()));
    for (name, count) in [
        ("provider_retained", inventory.provider_retained),
        ("provider_parked", inventory.provider_parked),
        ("provider_at_budget", inventory.provider_at_budget),
        (
            "provider_unresolved_failed",
            inventory.provider_unresolved_failed,
        ),
        ("outbox_retained", inventory.outbox_retained),
        ("outbox_parked", inventory.outbox_parked),
        ("outbox_at_budget", inventory.outbox_at_budget),
        (
            "outbox_unresolved_failed",
            inventory.outbox_unresolved_failed,
        ),
        ("provider_capacity", inventory.provider_capacity),
    ] {
        form = form.add_field(Field::text_single(name, count.to_string()));
    }
    form
}
