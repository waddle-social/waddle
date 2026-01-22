//! XMPP Extension Protocols (XEPs) Implementation
//!
//! This module contains implementations of various XMPP Extension Protocols
//! that extend the core XMPP functionality.
//!
//! ## Implemented XEPs
//!
//! - **XEP-0054**: vcard-temp - User profile information via vCard format.
//! - **XEP-0077**: In-Band Registration - Allows users to register accounts
//!   directly through the XMPP connection before authentication.
//! - **XEP-0115**: Entity Capabilities - Efficient service discovery caching
//!   via capability hashes included in presence stanzas.

pub mod xep0054;
pub mod xep0077;
pub mod xep0115;

pub use xep0054::{
    is_vcard_query, is_vcard_get, is_vcard_set, parse_vcard_from_iq, parse_vcard_element,
    build_vcard_element, build_vcard_response, build_empty_vcard_response, build_vcard_success,
    build_vcard_error, VCard, VCardPhoto, VCardError, NS_VCARD,
};

pub use xep0077::{
    parse_registration_iq, build_registration_fields_response, build_registration_success,
    build_registration_error, RegistrationRequest, RegistrationError, is_registration_query,
};

pub use xep0115::{
    Caps, CapsCache, CachedDiscoInfo, compute_caps_hash, build_caps_element,
    extract_caps_from_presence, is_caps_node_query, parse_caps_node,
    NS_CAPS, WADDLE_CAPS_NODE,
};
