//! Browser access to the same single-stage subscription commands as native.
use super::{
    future_to_promise, js_error, send_iq_command_stanza_aware, to_js_value, wasm_bindgen, Promise,
    WaddleClient,
};
use jid::Jid;
use waddle_xmpp_client_community::calendar_subscription::{
    unsupported, SubscriptionCommand, WaddleCalendarSubscription,
};

#[wasm_bindgen]
impl WaddleClient {
    /// Create or revoke the private calendar subscription belonging to the authenticated account.
    pub fn calendar_subscription(&self, revoke: bool) -> Promise {
        let inner = self.inner.clone();
        future_to_promise(async move {
            let (command, transport) = {
                let state = inner.borrow();
                let own = state
                    .config
                    .jid
                    .parse::<Jid>()
                    .map_err(|_| js_error("Invalid account JID"))?;
                (
                    SubscriptionCommand::new(&own, revoke),
                    state.config.server_url.clone(),
                )
            };
            let value = match send_iq_command_stanza_aware(inner, command.request()).await? {
                Ok(result) => command
                    .result(&result, &transport)
                    .ok_or_else(|| js_error("Invalid calendar subscription reply"))?,
                Err(e) if unsupported(&e.condition) => WaddleCalendarSubscription::Unsupported,
                Err(_) => {
                    return Err(js_error(
                        "The server could not change the calendar subscription",
                    ))
                }
            };
            to_js_value(&value)
        })
    }
}
