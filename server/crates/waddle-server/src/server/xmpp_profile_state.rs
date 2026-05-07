use kameo::actor::ActorRef;
use tracing::{debug, warn};
use waddle_xmpp::XmppError;

use crate::db::actor::{DbActor, DbQueryOne};
use crate::db::{Value, ValueExt, row_value};
use crate::vcard::VCardStore;

pub(crate) async fn get_vcard(
    vcard_store: &VCardStore,
    jid: &jid::BareJid,
) -> Result<Option<String>, XmppError> {
    debug!(jid = %jid, "Getting vCard");

    match vcard_store.get(jid).await {
        Ok(vcard) => Ok(vcard),
        Err(e) => {
            warn!(jid = %jid, error = %e, "Failed to get vCard");
            Err(XmppError::internal(format!("Database error: {}", e)))
        }
    }
}

pub(crate) async fn set_vcard(
    vcard_store: &VCardStore,
    jid: &jid::BareJid,
    vcard_xml: &str,
) -> Result<(), XmppError> {
    debug!(jid = %jid, "Setting vCard");

    match vcard_store.set(jid, vcard_xml).await {
        Ok(()) => Ok(()),
        Err(e) => {
            warn!(jid = %jid, error = %e, "Failed to set vCard");
            Err(XmppError::internal(format!("Database error: {}", e)))
        }
    }
}

pub(crate) async fn get_user_avatar_url(
    global_db_actor: &ActorRef<DbActor>,
    jid: &jid::BareJid,
) -> Result<Option<String>, XmppError> {
    let Some(localpart) = jid.node().map(|n| n.to_string()) else {
        return Ok(None);
    };

    let row = global_db_actor
        .ask(DbQueryOne {
            sql: "SELECT avatar_url FROM users WHERE xmpp_localpart = ? LIMIT 1".to_string(),
            params: vec![Value::from(localpart)],
        })
        .await
        .map_err(|e| {
            warn!(jid = %jid, error = %e, "avatar_url query failed");
            XmppError::internal(format!("Database actor error: {}", e))
        })?;

    let Some(row) = row else {
        return Ok(None);
    };

    let url = row_value(&row, 0)
        .and_then(ValueExt::as_optional_string)
        .map_err(|e| {
            warn!(jid = %jid, error = %e, "avatar_url column decode failed");
            XmppError::internal(format!("Database error: {}", e))
        })?;

    Ok(url.filter(|s| !s.is_empty()))
}
