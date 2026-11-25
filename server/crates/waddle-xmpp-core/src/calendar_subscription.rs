//! Single-stage XEP-0050 commands offered by the account's home server.

use url::{Host, Url};

/// Return the account's private, live iCalendar feed URL in a result data form.
pub const ISSUE: &str = "urn:waddle:calendar:subscription:1:issue";
/// Invalidate the current URL for this account.
pub const REVOKE: &str = "urn:waddle:calendar:subscription:1:revoke";
/// The text-single field containing the URL in the issue command's result form.
pub const URL_FIELD: &str = "url";

/// Cleartext calendar links are only suitable for loopback development endpoints.
pub fn is_loopback_url(url: &Url) -> bool {
    match url.host() {
        Some(Host::Domain(host)) => host == "localhost",
        Some(Host::Ipv4(ip)) => ip.is_loopback(),
        Some(Host::Ipv6(ip)) => ip.is_loopback(),
        None => false,
    }
}
