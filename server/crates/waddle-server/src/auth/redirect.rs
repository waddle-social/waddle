//! Where a completed sign-in may be sent.
//!
//! Both sign-in flows that end in a browser redirect carry a credential in
//! that redirect: the browser flow puts the session id in the fragment (or
//! sets the session cookie), and the XEP-0493 flow puts an authorization
//! code in the query. A redirect target the server does not control hands
//! that credential to whoever owns it, so every target is checked here:
//!
//! - Browser flow: the return URL must be on the server's own origin or on
//!   an origin listed in `WADDLE_CORS_ORIGINS` (the web apps already trusted
//!   to call this server).
//! - XEP-0493 flow: Waddle does not implement RFC 7591 client registration,
//!   so there is no registered redirect URI to compare against. Only
//!   redirect URIs that a remote attacker cannot receive are accepted:
//!   RFC 8252 §7.3 loopback (`http://127.0.0.1`, `http://[::1]`) and
//!   RFC 8252 §7.1 private-use (reverse-domain) schemes. PKCE with S256 is
//!   mandatory (XEP-0493 §"Authorization"; RFC 7636).

use std::net::{Ipv4Addr, Ipv6Addr};

use thiserror::Error;
use url::{Host, Origin, Position, Url};

/// Why a redirect target or PKCE challenge was refused.
#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum RedirectError {
    #[error("WADDLE_BASE_URL must be an absolute http(s) URL")]
    InvalidBaseUrl,
    #[error("WADDLE_CORS_ORIGINS entry {entry:?} is not an http(s) origin")]
    InvalidTrustedOrigin { entry: String },
    #[error("sign-in return URL is not a valid URL")]
    UnparseableReturnUrl,
    #[error(
        "sign-in return URL must be on this server or on an origin listed in WADDLE_CORS_ORIGINS"
    )]
    UntrustedReturnUrl,
    #[error("redirect_uri is not a valid absolute URI without a fragment")]
    UnparseableRedirectUri,
    #[error(
        "redirect_uri must be an RFC 8252 loopback URI (http://127.0.0.1 or http://[::1]) \
         or a private-use reverse-domain scheme; registered https redirects are not supported"
    )]
    UnsupportedRedirectUri,
    #[error("code_challenge is required and must be an S256 PKCE challenge")]
    InvalidCodeChallenge,
    #[error("code_challenge_method must be S256")]
    UnsupportedChallengeMethod,
}

/// Origins a browser sign-in may return to: the server's own origin plus
/// every origin in `WADDLE_CORS_ORIGINS`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrustedWebOrigins {
    base_url: Url,
    origins: Vec<Origin>,
}

impl TrustedWebOrigins {
    /// Build the set from `WADDLE_BASE_URL` and the comma-separated
    /// `WADDLE_CORS_ORIGINS` value (absent or blank means the server's own
    /// origin only).
    pub fn new(base_url: &str, cors_origins: Option<&str>) -> Result<Self, RedirectError> {
        let base_url = Url::parse(base_url).map_err(|_| RedirectError::InvalidBaseUrl)?;
        if !is_http_scheme(&base_url) || base_url.cannot_be_a_base() {
            return Err(RedirectError::InvalidBaseUrl);
        }

        let mut origins = vec![base_url.origin()];
        for entry in cors_origins
            .unwrap_or_default()
            .split(',')
            .map(str::trim)
            .filter(|entry| !entry.is_empty())
        {
            let invalid = || RedirectError::InvalidTrustedOrigin {
                entry: entry.to_string(),
            };
            let url = Url::parse(entry).map_err(|_| invalid())?;
            let is_bare_origin = (url.path().is_empty() || url.path() == "/")
                && url.query().is_none()
                && url.fragment().is_none()
                && url.username().is_empty()
                && url.password().is_none();
            if !is_http_scheme(&url) || !is_bare_origin {
                return Err(invalid());
            }
            let origin = url.origin();
            if !origins.contains(&origin) {
                origins.push(origin);
            }
        }

        Ok(Self { base_url, origins })
    }

    /// Resolve a browser flow's `next` value to an absolute URL on a trusted
    /// origin. A missing value returns to the server root; a path is resolved
    /// against `WADDLE_BASE_URL`.
    pub fn resolve_return_url(&self, next: Option<&str>) -> Result<Url, RedirectError> {
        let target = next.unwrap_or("/");
        let resolved = if target.starts_with('/') {
            self.base_url.join(target)
        } else {
            Url::parse(target)
        }
        .map_err(|_| RedirectError::UnparseableReturnUrl)?;

        // Check the resolved origin, never the input string: `/\host`,
        // `//host` and userinfo tricks all resolve to another origin.
        if !is_http_scheme(&resolved) || !self.origins.contains(&resolved.origin()) {
            return Err(RedirectError::UntrustedReturnUrl);
        }
        Ok(resolved)
    }

    /// Whether an `Origin` request header names a trusted web origin. The
    /// value must be a bare http(s) origin (no path, query or fragment);
    /// the opaque `null` origin is never trusted.
    pub fn is_trusted_origin(&self, origin: &str) -> bool {
        let Ok(url) = Url::parse(origin) else {
            return false;
        };
        // `Url` normalises an empty path to `/`; a bare origin header has
        // no path at all.
        let is_bare_origin = !origin.ends_with('/')
            && url.path() == "/"
            && url.query().is_none()
            && url.fragment().is_none()
            && url.username().is_empty()
            && url.password().is_none();
        is_http_scheme(&url) && is_bare_origin && self.origins.contains(&url.origin())
    }

    /// The `Location` value for a return URL from [`Self::resolve_return_url`].
    ///
    /// A target on the server's own origin is sent as a path, as before, so
    /// a deployment whose `WADDLE_BASE_URL` is an internal address behind a
    /// TLS terminator keeps working for same-origin sign-ins. A path that
    /// starts with `//` would be read as another host, so it stays absolute.
    pub fn location(&self, return_url: &Url) -> String {
        let path = &return_url[Position::BeforePath..];
        if return_url.origin() == self.base_url.origin() && !path.starts_with("//") {
            path.to_string()
        } else {
            return_url.to_string()
        }
    }
}

/// A validated XEP-0493 client redirect URI.
///
/// Keeps the client's exact string: RFC 6749 §4.1.3 requires the token
/// request's `redirect_uri` to be identical to the authorization request's,
/// and URL serialization can normalize (for example by adding a `/` path).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct XmppClientRedirectUri {
    raw: String,
    url: Url,
}

impl XmppClientRedirectUri {
    pub fn parse(raw: &str) -> Result<Self, RedirectError> {
        let url = Url::parse(raw).map_err(|_| RedirectError::UnparseableRedirectUri)?;
        // RFC 6749 §3.1.2: the redirection endpoint MUST NOT include a
        // fragment component.
        if url.fragment().is_some() {
            return Err(RedirectError::UnparseableRedirectUri);
        }
        if !url.username().is_empty() || url.password().is_some() {
            return Err(RedirectError::UnsupportedRedirectUri);
        }

        let accepted = match url.scheme() {
            // RFC 8252 §7.3: loopback IP literals. `localhost` is NOT
            // RECOMMENDED (§8.3) because it can resolve elsewhere.
            "http" => {
                matches!(
                    url.host(),
                    Some(Host::Ipv4(ip)) if ip == Ipv4Addr::LOCALHOST
                ) || matches!(
                    url.host(),
                    Some(Host::Ipv6(ip)) if ip == Ipv6Addr::LOCALHOST
                )
            }
            // An https redirect could belong to anyone; accepting one needs
            // RFC 7591 registration, which Waddle does not implement.
            "https" => false,
            // RFC 8252 §7.1: private-use schemes are reverse domain names,
            // so they always contain a period. This also excludes
            // `javascript:`, `data:`, `file:` and friends.
            scheme => scheme.contains('.'),
        };
        if !accepted {
            return Err(RedirectError::UnsupportedRedirectUri);
        }

        Ok(Self {
            raw: raw.to_string(),
            url,
        })
    }

    /// The client's redirect URI exactly as it sent it.
    pub fn as_str(&self) -> &str {
        &self.raw
    }

    /// The URL to redirect to, with `code` (and `state`, when present)
    /// appended to the query.
    pub fn with_authorization_code(&self, code: &str, state: Option<&str>) -> Url {
        let mut url = self.url.clone();
        {
            let mut query = url.query_pairs_mut();
            query.append_pair("code", code);
            if let Some(state) = state {
                query.append_pair("state", state);
            }
        }
        url
    }
}

/// An RFC 7636 S256 code challenge: base64url (no padding) of a SHA-256
/// digest, so exactly 43 characters.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PkceS256Challenge(String);

impl PkceS256Challenge {
    const ENCODED_LEN: usize = 43;

    /// Validate the challenge and its method from an authorization request.
    /// An absent method means `plain` (RFC 7636 §4.3), which is refused.
    pub fn from_request(
        challenge: Option<&str>,
        method: Option<&str>,
    ) -> Result<Self, RedirectError> {
        let challenge = challenge.ok_or(RedirectError::InvalidCodeChallenge)?;
        if method != Some("S256") {
            return Err(RedirectError::UnsupportedChallengeMethod);
        }
        Self::parse(challenge)
    }

    /// Validate a challenge already accepted at the authorization request.
    pub fn parse(challenge: &str) -> Result<Self, RedirectError> {
        let well_formed = challenge.len() == Self::ENCODED_LEN
            && challenge
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_');
        if !well_formed {
            return Err(RedirectError::InvalidCodeChallenge);
        }
        Ok(Self(challenge.to_string()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

fn is_http_scheme(url: &Url) -> bool {
    matches!(url.scheme(), "http" | "https")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn production() -> TrustedWebOrigins {
        TrustedWebOrigins::new(
            "https://xmpp.waddle.social",
            Some("https://waddle.chat,http://localhost:4321"),
        )
        .expect("valid production origins")
    }

    #[test]
    fn return_url_defaults_to_server_root() {
        assert_eq!(
            production().resolve_return_url(None).unwrap().as_str(),
            "https://xmpp.waddle.social/"
        );
    }

    #[test]
    fn return_url_accepts_paths_on_the_server() {
        assert_eq!(
            production()
                .resolve_return_url(Some("/rooms?x=1#y"))
                .unwrap()
                .as_str(),
            "https://xmpp.waddle.social/rooms?x=1#y"
        );
    }

    #[test]
    fn return_url_accepts_configured_web_origins() {
        let origins = production();
        assert!(origins
            .resolve_return_url(Some("https://waddle.chat/c/general"))
            .is_ok());
        assert!(origins
            .resolve_return_url(Some("http://localhost:4321/"))
            .is_ok());
    }

    #[test]
    fn return_url_rejects_other_origins() {
        let origins = production();
        for target in [
            "https://attacker.example/",
            "https://waddle.chat.attacker.example/",
            "http://waddle.chat/",
            "https://waddle.chat:8443/",
            "//attacker.example/",
            "/\\attacker.example/",
            "https://xmpp.waddle.social@attacker.example/",
            "javascript:alert(1)",
            "data:text/html,hi",
        ] {
            assert!(
                origins.resolve_return_url(Some(target)).is_err(),
                "{target} must be refused"
            );
        }
    }

    #[test]
    fn same_origin_locations_stay_relative() {
        let origins = production();
        let resolve = |target| origins.resolve_return_url(Some(target)).unwrap();
        assert_eq!(origins.location(&resolve("/rooms?a=1#b")), "/rooms?a=1#b");
        assert_eq!(
            origins.location(&resolve("https://waddle.chat/c")),
            "https://waddle.chat/c"
        );
        // A same-origin path that a browser would read as another host.
        let tricky = resolve("/.//attacker.example/");
        assert!(!origins.location(&tricky).starts_with("//"));
    }

    #[test]
    fn without_cors_origins_only_the_server_origin_is_trusted() {
        let origins = TrustedWebOrigins::new("https://xmpp.waddle.social/", None).unwrap();
        assert!(origins.resolve_return_url(Some("/")).is_ok());
        assert_eq!(
            origins.resolve_return_url(Some("https://waddle.chat/")),
            Err(RedirectError::UntrustedReturnUrl)
        );
    }

    #[test]
    fn invalid_configuration_is_refused() {
        assert_eq!(
            TrustedWebOrigins::new("not a url", None),
            Err(RedirectError::InvalidBaseUrl)
        );
        assert!(matches!(
            TrustedWebOrigins::new("https://a.example", Some("https://b.example/path")),
            Err(RedirectError::InvalidTrustedOrigin { .. })
        ));
        assert!(matches!(
            TrustedWebOrigins::new("https://a.example", Some("ftp://b.example")),
            Err(RedirectError::InvalidTrustedOrigin { .. })
        ));
    }

    #[test]
    fn trusted_origin_accepts_the_server_and_cors_origins() {
        let origins = production();
        assert!(origins.is_trusted_origin("https://xmpp.waddle.social"));
        assert!(origins.is_trusted_origin("https://waddle.chat"));
        assert!(origins.is_trusted_origin("http://localhost:4321"));
    }

    #[test]
    fn trusted_origin_rejects_foreign_null_garbage_and_paths() {
        let origins = production();
        for origin in [
            "https://attacker.example",
            "http://waddle.chat",
            "https://waddle.chat:8443",
            "null",
            "",
            "not an origin",
            "https://waddle.chat/",
            "https://waddle.chat/settings",
            "https://waddle.chat?x=1",
            "https://user@waddle.chat",
            "javascript:alert(1)",
        ] {
            assert!(
                !origins.is_trusted_origin(origin),
                "{origin:?} must not be trusted"
            );
        }
    }

    #[test]
    fn xmpp_redirect_accepts_loopback_and_private_use_schemes() {
        for raw in [
            "http://127.0.0.1:51004/callback",
            "http://127.0.0.1/",
            "http://[::1]:8080/oauth",
            "social.waddle.linux:/oauth/callback",
            "im.dino.app:/cb",
        ] {
            let parsed = XmppClientRedirectUri::parse(raw).expect(raw);
            assert_eq!(parsed.as_str(), raw, "keeps the exact client string");
        }
    }

    #[test]
    fn xmpp_redirect_rejects_uris_a_remote_party_could_receive() {
        for raw in [
            "https://attacker.example/cb",
            "https://xmpp.waddle.social/cb",
            "http://attacker.example/cb",
            "http://localhost:8080/cb",
            "http://127.0.0.2/cb",
            "http://user@127.0.0.1/cb",
            "http://127.0.0.1/cb#frag",
            "javascript:alert(1)",
            "data:text/html,hi",
            "file:///etc/passwd",
            "waddle://auth/callback",
            "not a uri",
        ] {
            assert!(
                XmppClientRedirectUri::parse(raw).is_err(),
                "{raw} must be refused"
            );
        }
    }

    #[test]
    fn xmpp_redirect_appends_code_and_state() {
        let redirect = XmppClientRedirectUri::parse("http://127.0.0.1:9000/cb?app=1").unwrap();
        assert_eq!(
            redirect
                .with_authorization_code("abc", Some("xyz"))
                .as_str(),
            "http://127.0.0.1:9000/cb?app=1&code=abc&state=xyz"
        );
    }

    #[test]
    fn pkce_requires_an_s256_challenge() {
        let challenge = "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM";
        assert!(PkceS256Challenge::from_request(Some(challenge), Some("S256")).is_ok());
        assert_eq!(
            PkceS256Challenge::from_request(None, Some("S256")),
            Err(RedirectError::InvalidCodeChallenge)
        );
        assert_eq!(
            PkceS256Challenge::from_request(Some(challenge), None),
            Err(RedirectError::UnsupportedChallengeMethod)
        );
        assert_eq!(
            PkceS256Challenge::from_request(Some(challenge), Some("plain")),
            Err(RedirectError::UnsupportedChallengeMethod)
        );
        assert_eq!(
            PkceS256Challenge::from_request(Some("too-short"), Some("S256")),
            Err(RedirectError::InvalidCodeChallenge)
        );
        assert_eq!(
            PkceS256Challenge::from_request(
                Some("E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw+cM"),
                Some("S256")
            ),
            Err(RedirectError::InvalidCodeChallenge)
        );
    }
}
