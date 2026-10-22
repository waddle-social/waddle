//! Authorization for actions a bot takes.

use jid::BareJid;
use kameo::actor::ActorRef;
use kameo::error::SendError;

use super::tokens::BotPrincipal;
use crate::permissions::{
    CheckPermission, Object, ObjectType, Permission, PermissionActor, PermissionError, Subject,
};

/// D25: allowed only if both the bot and its owner hold `permission` on
/// `object`. A bot never exceeds its owner, and its owner's access never
/// lends it anything: grants stay explicit on the bot's own subject.
///
/// On a channel, either subject being outcast denies too. The channel's
/// `read`, `view` and `send_message` include `parent->view`, which an
/// outcast keeps through the parent space, so the outcast relation is
/// checked separately, as every other channel access path does.
///
/// The bot is checked first; a denial stops before the owner check.
pub async fn authorize_bot(
    permissions: &ActorRef<PermissionActor>,
    principal: &BotPrincipal,
    permission: Permission,
    object: &Object,
) -> Result<bool, PermissionError> {
    for subject in [&principal.bot_jid, &principal.owner_jid] {
        if !check(permissions, subject, permission.clone(), object).await? {
            return Ok(false);
        }
        if object.object_type == ObjectType::Channel
            && check(
                permissions,
                subject,
                Permission::Custom("outcast".into()),
                object,
            )
            .await?
        {
            return Ok(false);
        }
    }
    Ok(true)
}

async fn check(
    permissions: &ActorRef<PermissionActor>,
    subject: &BareJid,
    permission: Permission,
    object: &Object,
) -> Result<bool, PermissionError> {
    let response = permissions
        .ask(CheckPermission {
            subject: Subject::user(subject.to_string()),
            permission,
            object: object.clone(),
        })
        .await
        .map_err(|error| match error {
            SendError::HandlerError(error) => error,
            other => {
                PermissionError::SpiceDbError(format!("permission actor unavailable: {other}"))
            }
        })?;
    Ok(response.allowed)
}
