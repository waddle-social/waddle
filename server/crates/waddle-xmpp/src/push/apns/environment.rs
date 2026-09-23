//! APNs environment → provider API host selection.

/// Production provider API host.
pub const APNS_PRODUCTION_HOST: &str = "api.push.apple.com";
/// Development (sandbox) provider API host.
pub const APNS_SANDBOX_HOST: &str = "api.sandbox.push.apple.com";

/// Which APNs environment a device token belongs to. A token minted by
/// a development build is only valid against the sandbox host and a
/// production token only against the production host; sending to the
/// wrong one yields `400 BadDeviceToken`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ApnsEnvironment {
    Production,
    Sandbox,
}

impl ApnsEnvironment {
    /// Provider API host for this environment.
    pub fn host(self) -> &'static str {
        match self {
            Self::Production => APNS_PRODUCTION_HOST,
            Self::Sandbox => APNS_SANDBOX_HOST,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn production_uses_the_production_host() {
        assert_eq!(ApnsEnvironment::Production.host(), "api.push.apple.com");
    }

    #[test]
    fn sandbox_uses_the_sandbox_host() {
        assert_eq!(
            ApnsEnvironment::Sandbox.host(),
            "api.sandbox.push.apple.com"
        );
    }
}
