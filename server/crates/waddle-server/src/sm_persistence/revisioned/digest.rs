use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use waddle_xmpp::auth::AuthenticatedPrincipalRef;
use waddle_xmpp::stream_management::persistence::{
    IngressCustodyDisposition, PersistedIngressAppend, PersistedSession, PersistedUnackedStanza,
    SmAcknowledgementMode,
};
use waddle_xmpp::tombstone::TombstoneTarget;

use super::super::codec;
use super::types::{storage, Mutation, Request, Revision, RevisionError};

/// Versioned, ordered encodings bind the operation receipt to every input,
/// including typed stanza serialization and independent custody payloads.
pub(super) fn digest(request: &Request<'_>) -> Result<Vec<u8>, RevisionError> {
    if request.id.is_nil() || request.mutation.generation().is_nil() {
        return Err(RevisionError::OperationIdentity);
    }
    let body = match &request.mutation {
        Mutation::Create {
            generation,
            principal,
            session,
            unacked,
            drained,
        } => json!([
            "create",
            generation.to_string(),
            principal_value(principal),
            session_value(session)?,
            frames_value(unacked)?,
            appends_value(drained)?
        ]),
        Mutation::Snapshot {
            revision,
            session,
            unacked,
            drained,
        } => json!([
            "snapshot",
            revision_value(revision),
            session_value(session)?,
            frames_value(unacked)?,
            appends_value(drained)?
        ]),
        Mutation::Append { revision, append } => json!([
            "append",
            revision_value(revision),
            session_value(&append.session)?,
            frame_value(&append.stanza)?,
            append.evicted_sequences,
            append
                .ingress_append
                .as_ref()
                .map(append_value)
                .transpose()?,
            append.new_delivery
        ]),
        Mutation::Expose { revision, ordinal } => {
            json!(["expose", revision_value(revision), ordinal])
        }
        Mutation::Acknowledge { revision, h, mode } => json!([
            "acknowledge",
            revision_value(revision),
            h,
            match mode {
                SmAcknowledgementMode::PreserveResumability => "preserve",
                SmAcknowledgementMode::RetireForPromotion => "retire_for_promotion",
            }
        ]),
        Mutation::ConfirmAcknowledgement { revision, window } => json!([
            "confirm_acknowledgement",
            revision_value(revision),
            window.from_exclusive,
            window.to_inclusive
        ]),
        Mutation::Tombstone {
            revision,
            target,
            through,
            sequences,
        } => json!([
            "tombstone",
            revision_value(revision),
            tombstone_value(target),
            through.timestamp(),
            through.timestamp_subsec_nanos(),
            sequences
        ]),
        Mutation::Prune {
            revision,
            sequences,
        } => json!(["prune", revision_value(revision), sequences]),
        Mutation::Retire { revision } => json!(["retire", revision_value(revision)]),
    };
    let bytes =
        serde_json::to_vec(&json!(["waddle-sm-revision-operation-v1", body])).map_err(storage)?;
    Ok(Sha256::digest(bytes).to_vec())
}

fn tombstone_value(target: &TombstoneTarget) -> Value {
    match target {
        TombstoneTarget::Groupchat { stanza_id, room } => {
            json!(["groupchat", stanza_id, room.to_string()])
        }
        TombstoneTarget::Direct {
            wire_id,
            author,
            archive,
        } => {
            json!(["direct", wire_id, author.to_string(), archive.to_string()])
        }
    }
}

fn principal_value(principal: &AuthenticatedPrincipalRef) -> Value {
    json!([
        principal.bare_jid().to_string(),
        principal.auth_context_id().as_uuid().to_string(),
        principal.auth_context_version().get(),
        principal.auth_epoch().get()
    ])
}

fn revision_value(revision: &Revision) -> Value {
    json!([
        revision.stream.as_str(),
        revision.generation.to_string(),
        revision.value,
        principal_value(&revision.principal),
        revision.jid.to_string(),
        revision.occupancy.to_string()
    ])
}

fn session_value(session: &PersistedSession) -> Result<Value, RevisionError> {
    Ok(json!([
        session.stream_id.as_str(),
        session.user_id,
        session.jid.to_string(),
        session.occupancy_session.to_string(),
        session.inbound_count,
        session.outbound_count,
        session.last_acked,
        session.replay_gap_through,
        session.max_resume_time,
        session.detached_at.timestamp(),
        session.detached_at.timestamp_subsec_nanos(),
        session.max_resume_duration.as_secs(),
        session.max_resume_duration.subsec_nanos(),
        session.carbons_enabled,
        session.roster_interested,
        session.blocklist_interested,
        session.presence_available,
        session.presence_show.as_ref().map(codec::show_wire_str),
        session.presence_status,
        session.presence_priority,
        codec::serialize_presence_payloads(&session.presence_payloads).map_err(storage)?
    ]))
}

fn frame_value(frame: &PersistedUnackedStanza) -> Result<Value, RevisionError> {
    Ok(json!([
        frame.stream_id.as_str(),
        frame.sequence,
        frame.stanza.wire(),
        frame.original_receipt_at.timestamp(),
        frame.original_receipt_at.timestamp_subsec_nanos(),
        codec::encode_ingress_receipts(&frame.ingress_receipts),
        frame.provenance.encode()
    ]))
}

fn frames_value(frames: &[PersistedUnackedStanza]) -> Result<Vec<Value>, RevisionError> {
    frames.iter().map(frame_value).collect()
}

fn append_value(append: &PersistedIngressAppend) -> Result<Value, RevisionError> {
    Ok(json!([
        append.key.message_key.to_storage().to_string(),
        append.key.kind.to_storage(),
        append.key.semantic_identity_hash,
        append.key.resource.to_string(),
        append.accepting_stream.as_str(),
        append.sequence,
        append.appended_at.timestamp(),
        append.appended_at.timestamp_subsec_nanos(),
        codec::serialize_stanza(&append.payload).map_err(storage)?,
        append.original_receipt_at.timestamp(),
        append.original_receipt_at.timestamp_subsec_nanos(),
        match append.disposition {
            IngressCustodyDisposition::Pending => 0,
            IngressCustodyDisposition::Acknowledged => 1,
            IngressCustodyDisposition::Promoted => 2,
            IngressCustodyDisposition::Tombstoned => 3,
        }
    ]))
}

fn appends_value(appends: &[PersistedIngressAppend]) -> Result<Vec<Value>, RevisionError> {
    appends.iter().map(append_value).collect()
}
