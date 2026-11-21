//! Domains, addresses and deterministic ports of the harness.
//!
//! Every listener binds `127.0.0.1` at `base + offset`, so a run can be
//! repeated and its logs compared. Every S2S path between two servers goes
//! through its own relay (see [`crate::net::relay`]), which records the
//! plaintext part of the stream and can cut the path for the partition
//! cases; each relay serves one directed source-to-destination pair, so
//! cutting one pair leaves the others alone.

use std::net::{Ipv4Addr, SocketAddr};

/// The account domains of the two Waddle deployments.
pub const HOME_A: &str = "home-a.test";
pub const HOME_B: &str = "home-b.test";

/// The account domain of the pinned Prosody.
pub const PROSODY: &str = "prosody.test";
/// A Prosody host whose TLS trust excludes the `.test` CA, so it cannot
/// authenticate Waddle by certificate and negotiates dialback instead.
pub const PROSODY_INSECURE: &str = "insecure.prosody.test";
pub const PROSODY_MUC: &str = "conference.prosody.test";
pub const PROSODY_PUBSUB: &str = "pubsub.prosody.test";

/// The account domain of the pinned ejabberd cluster.
pub const EJABBERD: &str = "ejabberd.test";
/// An ejabberd host whose S2S trust (`s2s_cafile`) excludes the `.test`
/// CA, for the same reason as [`PROSODY_INSECURE`].
pub const EJABBERD_INSECURE: &str = "insecure.ejabberd.test";
pub const EJABBERD_MUC: &str = "conference.ejabberd.test";
pub const EJABBERD_PUBSUB: &str = "pubsub.ejabberd.test";

/// A domain the harness itself authenticates as, with a leaf from the
/// `.test` CA, for the cases no stock server can drive.
pub const RAW_PEER: &str = "peer-raw.test";
/// A domain whose receiving server answers with `see-other-host`.
pub const REDIRECT: &str = "redirect.test";
/// A domain admitted without a private-CA profile, so the public-WebPKI
/// profile applies to it.
pub const PUBLIC_PROFILE: &str = "public-profile.test";
/// A domain on the denylist of the admission cases.
pub const DENIED: &str = "deny.test";
/// The denylisted domain of the label-form case, in U-label and A-label.
pub const DENIED_ULABEL: &str = "bücher.test";
pub const DENIED_ALABEL: &str = "xn--bcher-kva.test";
/// Domains of the slot-exhaustion case in `open` mode.
pub const FLOOD_PEERS: [&str; 6] = [
    "flood-1.test",
    "flood-2.test",
    "flood-3.test",
    "flood-4.test",
    "flood-5.test",
    "flood-6.test",
];

/// The MUC and Spaces domains of a Waddle deployment, derived as Waddle
/// derives them when `WADDLE_MUC_DOMAIN` and `WADDLE_SPACES_JID` are unset.
pub fn muc_domain(home: &str) -> String {
    ["muc", home].join(".")
}

pub fn spaces_domain(home: &str) -> String {
    ["spaces", home].join(".")
}

/// The three domains a Waddle deployment serves.
pub fn waddle_domains(home: &str) -> [String; 3] {
    [home.to_owned(), muc_domain(home), spaces_domain(home)]
}

/// Which deployment a Waddle-side item belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Home {
    A,
    B,
}

impl Home {
    pub fn domain(self) -> &'static str {
        match self {
            Self::A => HOME_A,
            Self::B => HOME_B,
        }
    }

    pub fn other(self) -> Self {
        match self {
            Self::A => Self::B,
            Self::B => Self::A,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::A => "home-a",
            Self::B => "home-b",
        }
    }
}

/// The servers that originate S2S connections, for relay selection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Server {
    Waddle(Home),
    Prosody,
    /// One node of the two-node ejabberd cluster.
    Ejabberd(u8),
}

impl Server {
    /// All domains served by a test node share its source address.
    pub fn relay_source(self) -> Ipv4Addr {
        let host = match self {
            Self::Waddle(Home::A) => 2,
            Self::Waddle(Home::B) => 3,
            Self::Prosody => 4,
            Self::Ejabberd(node) => node + 4,
        };
        Ipv4Addr::new(127, 0, 0, host)
    }

    pub fn label(self) -> String {
        match self {
            Self::Waddle(home) => home.label().to_owned(),
            Self::Prosody => "prosody".to_owned(),
            Self::Ejabberd(node) => ["ejabberd", &node.to_string()].concat(),
        }
    }
}

/// One directed S2S path through a relay.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Link {
    pub from: Server,
    pub to: Server,
}

impl Link {
    pub fn label(self) -> String {
        [self.from.label(), self.to.label()].join("->")
    }
}

/// Port offsets from the base port.
#[derive(Debug, Clone, Copy)]
pub struct Ports {
    pub base: u16,
}

impl Ports {
    pub const DEFAULT_BASE: u16 = 47100;

    fn at(self, offset: u16) -> u16 {
        self.base + offset
    }

    pub fn address(port: u16) -> SocketAddr {
        SocketAddr::from(([127, 0, 0, 1], port))
    }

    /// The stub DNS server Prosody resolves through.
    pub fn dns_for_prosody(self) -> u16 {
        self.at(1)
    }

    /// The stub DNS server the ejabberd nodes resolve through.
    pub fn dns_for_ejabberd(self) -> u16 {
        self.at(2)
    }

    pub fn waddle_http(self, home: Home) -> u16 {
        self.at(home_offset(home))
    }

    /// The TLS front that gives clients a WSS endpoint for the deployment.
    pub fn waddle_wss(self, home: Home) -> u16 {
        self.at(home_offset(home) + 1)
    }

    pub fn waddle_s2s(self, home: Home) -> u16 {
        self.at(home_offset(home) + 2)
    }

    pub fn spicedb(self, home: Home) -> u16 {
        self.at(home_offset(home) + 3)
    }

    pub fn enrolled_space_spicedb(self) -> u16 {
        self.at(home_offset(Home::A) + 5)
    }

    pub fn postgres(self, home: Home) -> u16 {
        self.at(home_offset(home) + 4)
    }

    pub fn prosody_c2s(self) -> u16 {
        self.at(30)
    }

    pub fn prosody_s2s(self) -> u16 {
        self.at(31)
    }

    /// Client and server ports of ejabberd node 1 or 2.
    pub fn ejabberd_c2s(self, node: u8) -> u16 {
        self.at(40 + 2 * u16::from(node.saturating_sub(1)))
    }

    pub fn ejabberd_s2s(self, node: u8) -> u16 {
        self.at(41 + 2 * u16::from(node.saturating_sub(1)))
    }

    pub fn epmd(self) -> u16 {
        self.at(44)
    }

    /// The Erlang distribution port window of node 1 or 2. The window
    /// holds ten ports because every `ejabberdctl` command starts a helper
    /// node with the same window while the server node holds one port.
    pub fn ejabberd_dist(self, node: u8) -> (u16, u16) {
        let start = self.at(90 + 10 * u16::from(node.saturating_sub(1)));
        (start, start + 9)
    }

    /// The relay of one directed path.
    pub fn relay(self, link: Link) -> u16 {
        let index = RELAYS
            .iter()
            .position(|candidate| *candidate == link)
            .and_then(|index| u16::try_from(index).ok())
            .unwrap_or(u16::MAX - self.base);
        self.at(50 + index)
    }

    /// The harness's receiving server for [`RAW_PEER`].
    pub fn raw_receiver(self) -> u16 {
        self.at(70)
    }

    /// The receiving server for [`REDIRECT`] and the address it redirects
    /// to, which records whether anything follows the redirect.
    pub fn redirect_receiver(self) -> u16 {
        self.at(71)
    }

    pub fn redirect_target(self) -> u16 {
        self.at(72)
    }

    /// The HTTP listener that records fetches of avatar and banner URLs.
    pub fn fetch_recorder(self) -> u16 {
        self.at(80)
    }

    /// The bridge between a STARTTLS negotiation and `openssl s_client`.
    pub fn tls_bridge(self) -> u16 {
        self.at(81)
    }
}

fn home_offset(home: Home) -> u16 {
    match home {
        Home::A => 10,
        Home::B => 20,
    }
}

/// Every relayed path, in port order.
pub const RELAYS: [Link; 12] = [
    Link {
        from: Server::Waddle(Home::B),
        to: Server::Waddle(Home::A),
    },
    Link {
        from: Server::Prosody,
        to: Server::Waddle(Home::A),
    },
    Link {
        from: Server::Ejabberd(1),
        to: Server::Waddle(Home::A),
    },
    Link {
        from: Server::Waddle(Home::A),
        to: Server::Waddle(Home::B),
    },
    Link {
        from: Server::Prosody,
        to: Server::Waddle(Home::B),
    },
    Link {
        from: Server::Ejabberd(1),
        to: Server::Waddle(Home::B),
    },
    Link {
        from: Server::Waddle(Home::A),
        to: Server::Prosody,
    },
    Link {
        from: Server::Waddle(Home::B),
        to: Server::Prosody,
    },
    Link {
        from: Server::Waddle(Home::A),
        to: Server::Ejabberd(1),
    },
    Link {
        from: Server::Waddle(Home::A),
        to: Server::Ejabberd(2),
    },
    Link {
        from: Server::Waddle(Home::B),
        to: Server::Ejabberd(1),
    },
    Link {
        from: Server::Waddle(Home::B),
        to: Server::Ejabberd(2),
    },
];

/// The listening port of a server's S2S service, which a relay forwards to.
pub fn s2s_port(ports: Ports, server: Server) -> u16 {
    match server {
        Server::Waddle(home) => ports.waddle_s2s(home),
        Server::Prosody => ports.prosody_s2s(),
        Server::Ejabberd(node) => ports.ejabberd_s2s(node),
    }
}

/// The relay a source uses to reach a destination, if the harness routes
/// that path. Both ejabberd nodes share node 1's relay towards Waddle: the
/// stub DNS cannot tell the nodes apart, and the relay counts streams per
/// pair, which is what the clustered-streams case observes.
pub fn relay_for(from: Server, to: Server) -> Option<Link> {
    let from = match from {
        Server::Ejabberd(_) => Server::Ejabberd(1),
        other => other,
    };
    RELAYS
        .iter()
        .copied()
        .find(|link| link.from == from && link.to == to)
}
