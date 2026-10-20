//! Authorization for actions a bot takes.

use kameo::actor::ActorRef;
use kameo::error::SendError;

use super::tokens::BotPrincipal;
use crate::permissions::{
    CheckPermission, Object, Permission, PermissionActor, PermissionError, Subject,
};

/// D25: allowed only if both the bot and its owner hold `permission` on
/// `object`. A bot never exceeds its owner, and its owner's access never
/// lends it anything: grants stay explicit on the bot's own subject.
///
/// The bot is checked first; a denial stops before the owner check.
pub async fn authorize_bot(
    permissions: &ActorRef<PermissionActor>,
    principal: &BotPrincipal,
    permission: Permission,
    object: &Object,
) -> Result<bool, PermissionError> {
    for subject in [&principal.bot_jid, &principal.owner_jid] {
        let response = permissions
            .ask(CheckPermission {
                subject: Subject::user(subject.to_string()),
                permission: permission.clone(),
                object: object.clone(),
            })
            .await
            .map_err(|error| match error {
                SendError::HandlerError(error) => error,
                other => {
                    PermissionError::SpiceDbError(format!("permission actor unavailable: {other}"))
                }
            })?;
        if !response.allowed {
            return Ok(false);
        }
    }
    Ok(true)
}
