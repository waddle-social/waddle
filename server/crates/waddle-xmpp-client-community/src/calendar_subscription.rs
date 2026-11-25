//! Authenticated, single-stage XEP-0050 calendar subscription commands.
use jid::Jid;
use minidom::Element;
use serde::{Deserialize, Serialize};
use url::Url;
use waddle_xmpp_client::xep::xep0050::{
    build_xep0050_command_request_with_id, AdHocAction, AdHocStatus, NS_COMMANDS, NS_DATA_FORMS,
};
use waddle_xmpp_core::calendar_subscription::{ISSUE, REVOKE, URL_FIELD, is_loopback_url};

#[derive(Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum WaddleCalendarSubscription {
    Available { url: String },
    Revoked,
    Unsupported,
}

pub struct SubscriptionCommand {
    server: String,
    id: String,
    revoke: bool,
}
impl SubscriptionCommand {
    pub fn new(account: &Jid, revoke: bool) -> Self {
        Self {
            server: account.domain().as_str().to_owned(),
            id: uuid::Uuid::new_v4().to_string(),
            revoke,
        }
    }
    fn node(&self) -> &'static str {
        if self.revoke {
            REVOKE
        } else {
            ISSUE
        }
    }
    pub fn request(&self) -> Element {
        build_xep0050_command_request_with_id(
            &self.server,
            self.node(),
            &self.id,
            AdHocAction::Execute,
            None,
        )
    }
    pub fn result(&self, iq: &Element, transport_url: &str) -> Option<WaddleCalendarSubscription> {
        if !iq.is("iq", "jabber:client")
            || iq.attr("type") != Some("result")
            || iq.attr("from") != Some(&self.server)
            || iq.attr("id") != Some(&self.id)
        {
            return None;
        }
        let mut commands = iq.children().filter(|e| e.is("command", NS_COMMANDS));
        let command = commands.next()?;
        if commands.next().is_some()
            || command.attr("node") != Some(self.node())
            || AdHocStatus::parse(command.attr("status")?) != Some(AdHocStatus::Completed)
        {
            return None;
        }
        let mut forms = command.children().filter(|e| e.is("x", NS_DATA_FORMS));
        if self.revoke {
            return forms
                .next()
                .is_none()
                .then_some(WaddleCalendarSubscription::Revoked);
        }
        let form = forms.next()?;
        if forms.next().is_some()
            || form.attr("type") != Some("result")
            || field(form, "FORM_TYPE", "hidden")? != ISSUE
        {
            return None;
        }
        let url = field(form, URL_FIELD, "text-single")?;
        safe_url(&url, transport_url).then_some(WaddleCalendarSubscription::Available { url })
    }
}
/// The XEP-0004 default for a missing field type is text-single.
fn field(form: &Element, key: &str, kind: &str) -> Option<String> {
    let mut fields = form
        .children()
        .filter(|f| f.is("field", NS_DATA_FORMS) && f.attr("var") == Some(key));
    let field = fields.next()?;
    if fields.next().is_some() || field.attr("type").unwrap_or("text-single") != kind {
        return None;
    }
    let mut values = field.children().filter(|v| v.is("value", NS_DATA_FORMS));
    let value = values.next()?;
    (values.next().is_none() && value.children().next().is_none()).then(|| value.text())
}
fn safe_url(raw: &str, transport: &str) -> bool {
    if !raw.split_once("://").is_some_and(|(scheme, _)| {
        scheme.eq_ignore_ascii_case("https") || scheme.eq_ignore_ascii_case("http")
    }) || raw.len() > 8192
        || raw
            .chars()
            .any(|c| c.is_control() || c.is_whitespace() || c == '\\')
    {
        return false;
    }
    let Ok(url) = Url::parse(raw) else {
        return false;
    };
    if !url.has_host()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.fragment().is_some()
    {
        return false;
    }
    url.scheme() == "https"
        || url.scheme() == "http"
            && is_loopback_url(&url)
            && Url::parse(transport)
                .is_ok_and(|ws| matches!(ws.scheme(), "ws" | "wss") && is_loopback_url(&ws))
}
pub fn unsupported(condition: &str) -> bool {
    matches!(
        condition,
        "item-not-found" | "feature-not-implemented" | "service-unavailable"
    )
}

#[cfg(test)]
#[path = "calendar_subscription_tests.rs"]
mod tests;
