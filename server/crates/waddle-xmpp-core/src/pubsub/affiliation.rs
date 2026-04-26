//! XEP-0060 §4.1 affiliations.

use std::{fmt, str::FromStr};

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum Affiliation {
    #[default]
    None,
    Owner,
    Publisher,
    PublishOnly,
    Member,
    Outcast,
}

impl Affiliation {
    /// Whether this affiliation is allowed to publish to a node by default.
    /// Note: still subject to `PublishModel` overrides at the node level.
    pub fn can_publish_default(self) -> bool {
        matches!(
            self,
            Affiliation::Owner | Affiliation::Publisher | Affiliation::PublishOnly
        )
    }

    /// Whether this affiliation bars all interactions with the node.
    pub fn is_outcast(self) -> bool {
        matches!(self, Affiliation::Outcast)
    }

    /// Whether the row should be persisted. We do not persist `None` rows.
    pub fn is_persisted(self) -> bool {
        !matches!(self, Affiliation::None)
    }
}

impl fmt::Display for Affiliation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            Affiliation::None => "none",
            Affiliation::Owner => "owner",
            Affiliation::Publisher => "publisher",
            Affiliation::PublishOnly => "publish-only",
            Affiliation::Member => "member",
            Affiliation::Outcast => "outcast",
        };
        f.write_str(s)
    }
}

impl FromStr for Affiliation {
    type Err = ();
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "none" => Ok(Affiliation::None),
            "owner" => Ok(Affiliation::Owner),
            "publisher" => Ok(Affiliation::Publisher),
            "publish-only" => Ok(Affiliation::PublishOnly),
            "member" => Ok(Affiliation::Member),
            "outcast" => Ok(Affiliation::Outcast),
            _ => Err(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn affiliation_round_trips() {
        for a in [
            Affiliation::None,
            Affiliation::Owner,
            Affiliation::Publisher,
            Affiliation::PublishOnly,
            Affiliation::Member,
            Affiliation::Outcast,
        ] {
            assert_eq!(a.to_string().parse::<Affiliation>().unwrap(), a);
        }
    }

    #[test]
    fn outcast_is_outcast() {
        assert!(Affiliation::Outcast.is_outcast());
        assert!(!Affiliation::Owner.is_outcast());
    }

    #[test]
    fn none_is_not_persisted() {
        assert!(!Affiliation::None.is_persisted());
        assert!(Affiliation::Owner.is_persisted());
    }

    #[test]
    fn publish_default_capabilities() {
        assert!(Affiliation::Owner.can_publish_default());
        assert!(Affiliation::Publisher.can_publish_default());
        assert!(Affiliation::PublishOnly.can_publish_default());
        assert!(!Affiliation::Member.can_publish_default());
        assert!(!Affiliation::None.can_publish_default());
        assert!(!Affiliation::Outcast.can_publish_default());
    }
}
