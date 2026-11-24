//! Publications of this server to a remote XMPP Push Service (XEP-0357
//! §5): the `<publish/>` IQ the server sends on behalf of a local account,
//! and the answer that decides whether the registration stays enabled.
//!
//! The IQ leaves from the bare server domain, as in the XEP's example, so
//! the push service's answer reaches the server rather than a client. Its
//! id names the notification outbox job it publishes ([`id`]), so whichever
//! replica the answer reaches can tell the job, and through it the
//! registration, from the id alone; the pending request lives in the shared
//! correlation table like any other.

use async_trait::async_trait;
use jid::BareJid;
use uuid::Uuid;

/// What an id of a publication starts with. The ids of this server's room
/// verifications start with something else, which is how an answer to the
/// server domain is told apart.
pub const PUBLICATION_ID_PREFIX: &str = "push-";

/// The id of the publication of the outbox job `job`, for its `attempt`th
/// attempt: a new id for each attempt, so that an attempt never collides
/// with the pending request of an earlier one.
pub fn id(job: Uuid, attempt: u32) -> String {
    format!("{PUBLICATION_ID_PREFIX}{}-{attempt}", job.simple())
}

/// The outbox job a publication id names.
pub fn job_of(id: &str) -> Option<Uuid> {
    let rest = id.strip_prefix(PUBLICATION_ID_PREFIX)?;
    let (job, attempt) = rest.split_once('-')?;
    if attempt.is_empty() || !attempt.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    Uuid::try_parse(job).ok()
}

/// How a push service answered a publication.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PublicationOutcome {
    /// An IQ result: the push service took the notification.
    Published,
    /// An IQ error that no later attempt is expected to change (XEP-0357
    /// §5.1: "the server SHOULD consider the particular JID and node
    /// combination to be disabled").
    Refused,
    /// An IQ error of type `wait`, or an answer this server cannot use: the
    /// registration stays enabled (XEP-0357 §5.1: "a server MAY choose to
    /// keep a service enabled if the error is deemed recoverable or
    /// transient").
    Transient,
}

/// The answer to one publication.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PublicationAnswer {
    /// The outbox job the publication was for.
    pub job: Uuid,
    /// The push service that answered, as the request named it.
    pub service: BareJid,
    pub outcome: PublicationOutcome,
}

/// What the router tells when a push service answers a publication. It is
/// called once per publication, on the replica the answer reached.
#[async_trait]
pub trait PublicationAnswers: Send + Sync {
    async fn answered(&self, answer: PublicationAnswer);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_id_names_its_job() {
        let job = Uuid::new_v4();
        assert_eq!(job_of(&id(job, 3)), Some(job));
    }

    #[test]
    fn ids_that_are_not_publications_name_no_job() {
        assert_eq!(job_of("room-0123"), None);
        assert_eq!(job_of("push-nonsense-1"), None);
        assert_eq!(job_of(&format!("push-{}", Uuid::new_v4().simple())), None);
        assert_eq!(job_of(&format!("push-{}-x", Uuid::new_v4().simple())), None);
    }
}
