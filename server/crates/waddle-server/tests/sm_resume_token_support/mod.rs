//! Map a client-visible XEP-0198 resume token to the storage key of its
//! durable session.
//!
//! The `id` of `<enabled/>` is an opaque resume token; the store keeps only
//! its SHA-256 (`sm_durable_sessions.resume_token_sha256`). The session's
//! `SmSessionKey` is the `stream_id` of its `sm_sessions` and `sm_unacked`
//! rows and the `flushed_in_session` of the pending rows it claims, so
//! inspecting those rows starts from that key, never from the token.

use waddle_server::db::Database;
use waddle_xmpp::pending_delivery::SmSessionId;
use waddle_xmpp::stream_management::durable::SmResumeToken;

/// The storage key of the session the resume token `resume_id` names.
///
/// The durable authority commits the session before it sends
/// `<enabled/>`, so the row exists once the client has read the token.
pub async fn session_stream(db: &Database, resume_id: &str) -> SmSessionId {
    let digest = SmResumeToken::from_wire(resume_id)
        .expect("the enabled id is a resume token")
        .digest();
    let guard = db.guard().await.expect("inspection database");
    let mut rows = guard
        .query(
            "SELECT session_id FROM sm_durable_sessions WHERE resume_token_sha256 = ?",
            waddle_server::db_params![hex::encode(digest.as_bytes())],
        )
        .await
        .expect("durable session by token digest");
    let row = rows
        .next()
        .await
        .expect("durable session row")
        .expect("the resume token names a committed durable session");
    SmSessionId::new(row.get::<String>(0).expect("session key"))
}
