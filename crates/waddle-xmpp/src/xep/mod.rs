//! XMPP Extension Protocols (XEPs) Implementation
//!
//! This module contains implementations of various XMPP Extension Protocols
//! that extend the core XMPP functionality.
//!
//! ## Implemented XEPs
//!
//! - **XEP-0077**: In-Band Registration - Allows users to register accounts
//!   directly through the XMPP connection before authentication.

pub mod xep0077;

pub use xep0077::{
    parse_registration_iq, build_registration_fields_response, build_registration_success,
    build_registration_error, RegistrationRequest, RegistrationError, is_registration_query,
};
