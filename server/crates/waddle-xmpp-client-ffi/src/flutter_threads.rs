use uuid::Uuid;
use waddle_xmpp_client::xep::threads::bindings::fetch_threads_query_from_options;
use waddle_xmpp_client::xep::threads::{build_fetch_threads_iq, parse_threads_response};

use crate::error::client_error_to_waddle;
use crate::messaging_verbs::send_iq_with_timeout;
use crate::{WaddleClient, WaddleError, WaddleFetchThreadsOptions, WaddleThreadsPage};

impl WaddleClient {
    /// Fetch the signed-in account's threads across rooms and direct conversations.
    pub async fn fetch_threads(
        &self,
        opts: WaddleFetchThreadsOptions,
    ) -> Result<WaddleThreadsPage, WaddleError> {
        let query =
            fetch_threads_query_from_options(&opts).map_err(|_| WaddleError::InvalidArgument)?;
        let handle = self.clone_handle().await.ok_or(WaddleError::NotConnected)?;
        let result = send_iq_with_timeout(
            &handle,
            build_fetch_threads_iq(&Uuid::new_v4().to_string(), &query),
        )
        .await
        .map_err(|err| client_error_to_waddle(&err))?;
        parse_threads_response(&result)
            .map(WaddleThreadsPage::from)
            .ok_or(WaddleError::MalformedResponse)
    }
}
