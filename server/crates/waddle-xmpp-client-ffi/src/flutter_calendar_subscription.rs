//! Native client access to the home server's private calendar subscription.
use crate::{WaddleClient, WaddleError};
use jid::Jid;
pub use waddle_xmpp_client_community::calendar_subscription::WaddleCalendarSubscription;
use waddle_xmpp_client_community::calendar_subscription::{unsupported, SubscriptionCommand};

impl WaddleClient {
    pub async fn calendar_subscription(
        &self,
        revoke: bool,
    ) -> Result<WaddleCalendarSubscription, WaddleError> {
        let own = self
            .config
            .jid
            .parse::<Jid>()
            .map_err(|_| WaddleError::InvalidJid)?;
        let command = SubscriptionCommand::new(&own, revoke);
        match self.community_iq(command.request()).await {
            Ok(result) => command
                .result(&result, &self.config.server_url)
                .ok_or(WaddleError::MalformedResponse),
            Err(WaddleError::Stanza { condition, .. }) if unsupported(&condition) => {
                Ok(WaddleCalendarSubscription::Unsupported)
            }
            Err(WaddleError::Stanza { condition, .. }) => Err(WaddleError::Stanza {
                condition,
                text: None,
            }),
            Err(e) => Err(e),
        }
    }
}
