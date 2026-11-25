//! XEP-0494: Client Access Management
//!
//! Lets an account owner list the clients that hold access to the account and
//! revoke it (XEP-0493 requires servers to offer this: "Server implementations
//! MUST NOT support this specification without giving account owners a method
//! to manage and revoke access").
//!
//! ```xml
//! <iq type='get' id='1'><list xmlns='urn:xmpp:cam:0'/></iq>
//!
//! <iq type='result' id='1'>
//!   <clients xmlns='urn:xmpp:cam:0'>
//!     <client connected='false' id='HjEEr45_LQr' type='access'>
//!       <first-seen>2023-03-27T15:16:09Z</first-seen>
//!       <last-seen>2023-03-27T15:37:24Z</last-seen>
//!       <auth><grant/></auth>
//!       <permission status='normal'/>
//!     </client>
//!   </clients>
//! </iq>
//!
//! <iq type='set' id='2'><revoke xmlns='urn:xmpp:cam:0' id='HjEEr45_LQr'/></iq>
//! <iq type='result' id='2'/>
//! ```

use chrono::{DateTime, SecondsFormat, Utc};
use minidom::Element;
use xmpp_parsers::iq::Iq;

/// Namespace of XEP-0494 Client Access Management.
pub const NS_CAM: &str = "urn:xmpp:cam:0";

/// A request in the CAM namespace.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CamRequest {
    /// `<list/>` in an IQ get.
    List,
    /// `<revoke id=.../>` in an IQ set.
    Revoke { id: String },
}

/// A CAM request that is not a well-formed `list` get or `revoke` set.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("malformed client access management request")]
pub struct CamMalformed;

/// Whether `iq` carries a payload in the CAM namespace.
pub fn is_cam_query(iq: &Iq) -> bool {
    match iq {
        Iq::Get { payload, .. } | Iq::Set { payload, .. } => payload.ns() == NS_CAM,
        Iq::Result { .. } | Iq::Error { .. } => false,
    }
}

/// Parse a CAM request: `<list/>` in a get, or `<revoke id=.../>` with a
/// non-empty id in a set. Anything else in the namespace is malformed.
pub fn parse_cam_request(iq: &Iq) -> Result<CamRequest, CamMalformed> {
    match iq {
        Iq::Get { payload, .. }
            if payload.is("list", NS_CAM) && payload.children().count() == 0 =>
        {
            Ok(CamRequest::List)
        }
        Iq::Set { payload, .. } if payload.is("revoke", NS_CAM) => payload
            .attr("id")
            .filter(|id| !id.is_empty())
            .map(|id| CamRequest::Revoke { id: id.to_owned() })
            .ok_or(CamMalformed),
        _ => Err(CamMalformed),
    }
}

/// The `type` attribute of a listed client.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CamClientType {
    /// The client is known to have an active or inactive session.
    Session,
    /// The client holds access but has established no session.
    Access,
}

impl CamClientType {
    fn as_str(self) -> &'static str {
        match self {
            Self::Session => "session",
            Self::Access => "access",
        }
    }
}

/// The `status` of a client's `<permission/>`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CamPermission {
    /// Full unlimited access.
    Unrestricted,
    /// General access without security-relevant features (what
    /// `xmpp:client:normal` grants).
    Normal,
}

impl CamPermission {
    fn as_str(self) -> &'static str {
        match self {
            Self::Unrestricted => "unrestricted",
            Self::Normal => "normal",
        }
    }
}

/// One client in a listing. Every listed client authenticated through an
/// authorization grant, so its `<auth/>` holds `<grant/>`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CamClient {
    /// Opaque reference that [`CamRequest::Revoke`] takes.
    pub id: String,
    /// Whether the client has a session that is, or may yet be, connected.
    pub connected: bool,
    pub client_type: CamClientType,
    /// When access was granted.
    pub first_seen: DateTime<Utc>,
    /// When the access was last used.
    pub last_seen: DateTime<Utc>,
    pub permission: CamPermission,
}

impl CamClient {
    fn to_element(&self) -> Element {
        let timestamp = |name: &str, at: &DateTime<Utc>| {
            Element::builder(name, NS_CAM)
                .append(at.to_rfc3339_opts(SecondsFormat::Secs, true))
                .build()
        };
        Element::builder("client", NS_CAM)
            .attr(
                minidom::rxml::xml_ncname!("connected").to_owned(),
                if self.connected { "true" } else { "false" },
            )
            .attr(minidom::rxml::xml_ncname!("id").to_owned(), &self.id)
            .attr(
                minidom::rxml::xml_ncname!("type").to_owned(),
                self.client_type.as_str(),
            )
            .append(timestamp("first-seen", &self.first_seen))
            .append(timestamp("last-seen", &self.last_seen))
            .append(
                Element::builder("auth", NS_CAM)
                    .append(Element::builder("grant", NS_CAM).build())
                    .build(),
            )
            .append(
                Element::builder("permission", NS_CAM)
                    .attr(
                        minidom::rxml::xml_ncname!("status").to_owned(),
                        self.permission.as_str(),
                    )
                    .build(),
            )
            .build()
    }
}

/// The result of a `<list/>` request.
pub fn build_clients_result(request: &Iq, clients: &[CamClient]) -> Iq {
    let mut payload = Element::builder("clients", NS_CAM).build();
    for client in clients {
        payload.append_child(client.to_element());
    }
    Iq::Result {
        from: request.to().cloned(),
        to: request.from().cloned(),
        id: request.id().to_string(),
        payload: Some(payload),
    }
}

/// The empty result that confirms a revocation.
pub fn build_revoke_result(request: &Iq) -> Iq {
    Iq::Result {
        from: request.to().cloned(),
        to: request.from().cloned(),
        id: request.id().to_string(),
        payload: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn get(payload: Element) -> Iq {
        Iq::Get {
            from: None,
            to: None,
            id: "q1".into(),
            payload,
        }
    }

    fn set(payload: Element) -> Iq {
        Iq::Set {
            from: None,
            to: None,
            id: "q2".into(),
            payload,
        }
    }

    #[test]
    fn list_and_revoke_parse() {
        let list = get(Element::builder("list", NS_CAM).build());
        assert!(is_cam_query(&list));
        assert_eq!(parse_cam_request(&list), Ok(CamRequest::List));

        let revoke = set(Element::builder("revoke", NS_CAM)
            .attr(minidom::rxml::xml_ncname!("id").to_owned(), "abc")
            .build());
        assert_eq!(
            parse_cam_request(&revoke),
            Ok(CamRequest::Revoke { id: "abc".into() })
        );
    }

    #[test]
    fn malformed_requests_are_refused() {
        let no_id = set(Element::builder("revoke", NS_CAM).build());
        assert_eq!(parse_cam_request(&no_id), Err(CamMalformed));
        let empty_id = set(Element::builder("revoke", NS_CAM)
            .attr(minidom::rxml::xml_ncname!("id").to_owned(), "")
            .build());
        assert_eq!(parse_cam_request(&empty_id), Err(CamMalformed));
        // list is a get, revoke is a set.
        let list_set = set(Element::builder("list", NS_CAM).build());
        assert_eq!(parse_cam_request(&list_set), Err(CamMalformed));
        let revoke_get = get(Element::builder("revoke", NS_CAM)
            .attr(minidom::rxml::xml_ncname!("id").to_owned(), "abc")
            .build());
        assert_eq!(parse_cam_request(&revoke_get), Err(CamMalformed));
        let other = get(Element::builder("list", "urn:example:other").build());
        assert!(!is_cam_query(&other));
    }

    #[test]
    fn clients_result_matches_the_xep_shape() {
        let request = get(Element::builder("list", NS_CAM).build());
        let seen = Utc.with_ymd_and_hms(2023, 3, 27, 15, 16, 9).unwrap();
        let result = build_clients_result(
            &request,
            &[CamClient {
                id: "HjEEr45_LQr".into(),
                connected: false,
                client_type: CamClientType::Access,
                first_seen: seen,
                last_seen: seen,
                permission: CamPermission::Normal,
            }],
        );
        let Iq::Result {
            payload: Some(clients),
            id,
            ..
        } = result
        else {
            panic!("expected a result with a payload");
        };
        assert_eq!(id, "q1");
        assert!(clients.is("clients", NS_CAM));
        let client = clients.get_child("client", NS_CAM).expect("client");
        assert_eq!(client.attr("id"), Some("HjEEr45_LQr"));
        assert_eq!(client.attr("connected"), Some("false"));
        assert_eq!(client.attr("type"), Some("access"));
        assert_eq!(
            client
                .get_child("first-seen", NS_CAM)
                .map(|e| e.text())
                .as_deref(),
            Some("2023-03-27T15:16:09Z")
        );
        assert!(client
            .get_child("auth", NS_CAM)
            .and_then(|auth| auth.get_child("grant", NS_CAM))
            .is_some());
        assert_eq!(
            client
                .get_child("permission", NS_CAM)
                .and_then(|p| p.attr("status")),
            Some("normal")
        );
    }

    #[test]
    fn revoke_result_is_empty() {
        let request = set(Element::builder("revoke", NS_CAM).build());
        assert!(matches!(
            build_revoke_result(&request),
            Iq::Result { payload: None, .. }
        ));
    }
}
