//! IQ handler glue for `urn:waddle:threads:0`.
//!
//! Mirrors the inbox handler shape: parse → ACL → storage page →
//! build response.

use jid::BareJid;
use minidom::Element;
use xmpp_parsers::iq::Iq;

use super::query::{ThreadsError, DEFAULT_PAGE_SIZE, NS_THREADS};
use super::storage::ThreadsStorage;
use super::wire::{build_threads_response, parse_threads_query};

/// Discriminator: does this IQ carry the `urn:waddle:threads:0` query?
pub fn is_threads_iq(iq: &Iq) -> bool {
    let elem = match iq {
        Iq::Get { payload: e, .. } | Iq::Set { payload: e, .. } => e,
        _ => return false,
    };
    elem.is("query", NS_THREADS)
}

/// Outcome of running the threads-IQ handler.
pub enum ThreadsIqOutcome {
    /// Built response payload — caller wraps in `<iq type='result'>`.
    Result(Element),
    /// Storage error; caller should respond with internal-server-error.
    InternalError(String),
    /// Caller should respond with bad-request.
    BadRequest(ThreadsError),
    /// Caller should respond with forbidden.
    Forbidden,
    /// Caller should respond with not-authorized.
    NotAuthorized,
}

/// Run the threads IQ. The caller stamps the request id / from / to and
/// converts the outcome to an `<iq>` envelope.
pub async fn handle_threads_iq(
    storage: &dyn ThreadsStorage,
    iq: &Iq,
    requester_bare: Option<&BareJid>,
) -> ThreadsIqOutcome {
    let Some(requester) = requester_bare else {
        return ThreadsIqOutcome::NotAuthorized;
    };

    // ACL: when the IQ is addressed (`to`), it MUST be the requester's
    // bare JID. Cross-user threads queries are refused with <forbidden/>.
    if let Some(to) = iq.to() {
        if to.to_bare() != *requester {
            return ThreadsIqOutcome::Forbidden;
        }
    }

    let query = match parse_threads_query(iq) {
        Ok(q) => q,
        Err(err) => return ThreadsIqOutcome::BadRequest(err),
    };

    let page_size = query.page_size.unwrap_or(DEFAULT_PAGE_SIZE);

    match storage
        .page(requester, page_size, query.after_cursor.as_deref())
        .await
    {
        Ok(page) => ThreadsIqOutcome::Result(build_threads_response(&page)),
        Err(error) => ThreadsIqOutcome::InternalError(error.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::threads::query::ThreadsPage;
    use crate::threads::storage::InboxBackedThreadsStorage;
    use std::sync::Arc;
    use waddle_xmpp::inbox::storage::InMemoryInboxStorage;
    use waddle_xmpp::inbox::storage::InboxStorage;
    use waddle_xmpp::inbox::{ConversationKind, InboxEntry};

    fn jid(value: &str) -> BareJid {
        value.parse().expect("valid JID")
    }

    fn empty_query_iq() -> Iq {
        Iq::Get {
            from: None,
            to: None,
            id: "th-1".into(),
            payload: Element::builder("query", NS_THREADS).build(),
        }
    }

    #[tokio::test]
    async fn empty_inbox_yields_empty_page_result() {
        let inbox: Arc<dyn InboxStorage> = Arc::new(InMemoryInboxStorage::new());
        let storage = InboxBackedThreadsStorage::new(inbox);
        let outcome =
            handle_threads_iq(&storage, &empty_query_iq(), Some(&jid("me@example.com"))).await;
        match outcome {
            ThreadsIqOutcome::Result(elem) => {
                assert_eq!(elem.name(), "threads");
                assert_eq!(elem.attr("total"), Some("0"));
            }
            _ => panic!("expected Result outcome"),
        }
    }

    #[tokio::test]
    async fn cross_user_addressed_iq_is_forbidden() {
        let inbox: Arc<dyn InboxStorage> = Arc::new(InMemoryInboxStorage::new());
        let storage = InboxBackedThreadsStorage::new(inbox);
        let mut iq = empty_query_iq();
        *iq.to_mut() = Some(jid::Jid::from(jid("bob@example.com")));
        let outcome = handle_threads_iq(&storage, &iq, Some(&jid("alice@example.com"))).await;
        assert!(matches!(outcome, ThreadsIqOutcome::Forbidden));
    }

    #[tokio::test]
    async fn missing_requester_is_not_authorized() {
        let inbox: Arc<dyn InboxStorage> = Arc::new(InMemoryInboxStorage::new());
        let storage = InboxBackedThreadsStorage::new(inbox);
        let outcome = handle_threads_iq(&storage, &empty_query_iq(), None).await;
        assert!(matches!(outcome, ThreadsIqOutcome::NotAuthorized));
    }

    #[tokio::test]
    async fn populated_inbox_returns_entries() {
        let inbox: Arc<dyn InboxStorage> = Arc::new(InMemoryInboxStorage::new());
        let user = jid("me@example.com");
        inbox
            .upsert(
                &user,
                InboxEntry::new(
                    jid("room@muc.example"),
                    ConversationKind::MucRoom,
                    "s0",
                    100,
                ),
                true,
            )
            .await
            .expect("upsert channel");
        inbox
            .upsert(
                &user,
                InboxEntry::new(
                    jid("room@muc.example"),
                    ConversationKind::MucRoom,
                    "s1",
                    110,
                )
                .with_thread("t1"),
                true,
            )
            .await
            .expect("upsert thread");

        let storage = InboxBackedThreadsStorage::new(inbox);
        let outcome = handle_threads_iq(&storage, &empty_query_iq(), Some(&user)).await;
        let elem = match outcome {
            ThreadsIqOutcome::Result(e) => e,
            _ => panic!("expected Result outcome"),
        };
        assert_eq!(elem.attr("total"), Some("1"));
        let threads_count = elem.children().filter(|c| c.name() == "thread").count();
        assert_eq!(threads_count, 1);
    }

    #[test]
    fn is_threads_iq_recognises_query() {
        assert!(is_threads_iq(&empty_query_iq()));
    }

    #[test]
    fn is_threads_iq_rejects_other_namespaces() {
        let iq = Iq::Get {
            from: None,
            to: None,
            id: "x".into(),
            payload: Element::builder("query", "urn:xmpp:inbox:0").build(),
        };
        assert!(!is_threads_iq(&iq));
    }

    /// `ThreadsPage::default()` is the only path that satisfies the
    /// type contract for [`ThreadsIqOutcome::Result`] outside `handle_threads_iq`.
    /// Smoke-test the builder via a default page.
    #[test]
    fn builder_produces_threads_element_for_default_page() {
        let elem = build_threads_response(&ThreadsPage::default());
        assert_eq!(elem.name(), "threads");
    }
}
