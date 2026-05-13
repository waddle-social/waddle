use kameo::actor::ActorRef;
use tracing::{debug, warn};
use waddle_xmpp::xep::xep0363::{effective_content_type, sanitize_filename};
use waddle_xmpp::{UploadSlotInfo, XmppError};

use crate::db::actor::{DbActor, DbExecute};
use crate::db::Value;

pub(crate) async fn create_upload_slot(
    global_db_actor: &ActorRef<DbActor>,
    domain: &str,
    requester_jid: &jid::BareJid,
    filename: &str,
    size: u64,
    content_type: Option<&str>,
) -> Result<UploadSlotInfo, XmppError> {
    debug!(
        jid = %requester_jid,
        filename = %filename,
        size = size,
        content_type = ?content_type,
        "Creating upload slot"
    );

    let max_size = crate::server::routes::uploads::max_upload_size();
    if size > max_size {
        warn!(
            jid = %requester_jid,
            size = size,
            max_size = max_size,
            "File too large for upload"
        );
        return Err(XmppError::not_acceptable(Some(format!(
            "File too large. Maximum size is {} bytes.",
            max_size
        ))));
    }
    let size_bytes = crate::server::routes::uploads::upload_size_to_i64(size).ok_or_else(|| {
        XmppError::not_acceptable(Some(format!(
            "File too large. Maximum size is {} bytes.",
            crate::server::routes::uploads::MAX_DATABASE_UPLOAD_SIZE
        )))
    })?;

    let safe_filename = sanitize_filename(filename);
    let effective_type = effective_content_type(content_type).to_string();
    let slot_id = uuid::Uuid::new_v4().to_string();
    let expires_at = chrono::Utc::now() + chrono::Duration::minutes(15);
    let base_url = upload_base_url(domain);
    let put_url = format!("{}/api/upload/{}", base_url, slot_id);
    let get_url = format!("{}/api/files/{}/{}", base_url, slot_id, safe_filename);

    global_db_actor
        .ask(DbExecute {
            sql: "INSERT INTO upload_slots (id, requester_jid, filename, size_bytes, content_type, status, expires_at) VALUES (?, ?, ?, ?, ?, 'pending', ?)".to_string(),
            params: vec![
                Value::from(slot_id.clone()),
                Value::from(requester_jid.to_string()),
                Value::from(safe_filename),
                Value::from(size_bytes),
                Value::from(effective_type.clone()),
                Value::from(expires_at.to_rfc3339()),
            ],
        })
        .await
        .map_err(|e| {
            warn!(error = %e, "Failed to create upload slot in database");
            XmppError::internal(format!("Database actor error: {}", e))
        })?;

    debug!(
        slot_id = %slot_id,
        put_url = %put_url,
        get_url = %get_url,
        "Created upload slot"
    );

    Ok(UploadSlotInfo {
        put_url,
        get_url,
        put_headers: vec![("Content-Type".to_string(), effective_type)],
    })
}

fn upload_base_url(domain: &str) -> String {
    std::env::var("WADDLE_BASE_URL")
        .unwrap_or_else(|_| format!("https://{}", domain))
        .trim_end_matches('/')
        .to_string()
}
