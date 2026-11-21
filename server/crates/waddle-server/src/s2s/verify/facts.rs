//! Bounded certificate metadata.
//!
//! webpki authenticates a chain but exposes neither key usage bits, the
//! presence of an extended key usage extension, validity instants nor
//! `OtherName` identifiers. This module reads them with `x509-parser`, after
//! the size and count ceilings in [`super::limits`] have been checked on the
//! raw DER, and keeps only small owned values. Parsing authenticates
//! nothing: the facts of a certificate matter only once webpki has verified
//! the path that contains it.

use x509_parser::certificate::X509Certificate;
use x509_parser::extensions::{GeneralName, ParsedExtension};
use x509_parser::prelude::FromDer;

use super::der::{
    certificate_shape, name_constraint_forms, other_name_string, single, NameConstraintForms,
    IA5_STRING, OID_NAME_CONSTRAINTS, OID_SUBJECT_ALT_NAME, SEQUENCE, UTF8_STRING,
};
use super::limits::{
    MAX_CERTIFICATE_DER_BYTES, MAX_CERTIFICATE_EXTENSIONS, MAX_NAME_CONSTRAINT_SUBTREES,
    MAX_SUBJECT_ALT_NAMES,
};

/// id-ce-subjectKeyIdentifier, 2.5.29.14.
const OID_SUBJECT_KEY_IDENTIFIER: &[u8] = &[0x55, 0x1d, 0x0e];
/// id-ce-keyUsage, 2.5.29.15.
const OID_KEY_USAGE: &[u8] = &[0x55, 0x1d, 0x0f];
/// id-ce-basicConstraints, 2.5.29.19.
const OID_BASIC_CONSTRAINTS: &[u8] = &[0x55, 0x1d, 0x13];
/// id-ce-authorityKeyIdentifier, 2.5.29.35.
const OID_AUTHORITY_KEY_IDENTIFIER: &[u8] = &[0x55, 0x1d, 0x23];
/// id-ce-extKeyUsage, 2.5.29.37.
const OID_EXTENDED_KEY_USAGE: &[u8] = &[0x55, 0x1d, 0x25];
/// id-on-xmppAddr, 1.3.6.1.5.5.7.8.5 (RFC 6120 §13.7.1.4).
const OID_XMPP_ADDR: &[u8] = &[0x2b, 0x06, 0x01, 0x05, 0x05, 0x07, 0x08, 0x05];
/// id-on-dnsSRV, 1.3.6.1.5.5.7.8.7 (RFC 4985).
const OID_SRV_NAME: &[u8] = &[0x2b, 0x06, 0x01, 0x05, 0x05, 0x07, 0x08, 0x07];

/// Why a certificate's metadata could not be read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum FactsError {
    TooLarge,
    Malformed,
    TooManyExtensions,
    TooManyNames,
}

/// Why the XMPP identifiers of a certificate cannot be used.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum IdentifierError {
    Malformed,
    Duplicate,
}

/// The key usage bits the verifier acts on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct KeyUsageFacts {
    pub(super) digital_signature: bool,
    pub(super) key_cert_sign: bool,
    pub(super) crl_sign: bool,
}

/// The extended key usages the verifier acts on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct ExtendedKeyUsageFacts {
    pub(super) server_auth: bool,
    pub(super) client_auth: bool,
}

/// An SRV-ID (RFC 4985 SRVName): a service label and the domain it serves.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct SrvName {
    /// The service label with its underscore, in lower case.
    pub(super) service: String,
    /// The domain name, in lower case.
    pub(super) name: String,
}

impl SrvName {
    /// Split `_service.name`. The service is the first label and the name
    /// is everything after it: no further label is ever stripped.
    fn parse(text: &str) -> Option<Self> {
        let (service, name) = text.split_once('.')?;
        if service.len() < 2 || !service.starts_with('_') || name.is_empty() {
            return None;
        }
        Some(Self {
            service: service.to_ascii_lowercase(),
            name: name.to_ascii_lowercase(),
        })
    }
}

/// The XMPP-specific identifiers of an end-entity certificate. DNS-IDs are
/// not collected: webpki matches them on the certificate itself.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(super) struct XmppIdentifiers {
    pub(super) srv_names: Vec<SrvName>,
    pub(super) xmpp_addrs: Vec<String>,
}

/// Owned metadata of one certificate.
#[derive(Debug, Clone)]
pub(super) struct CertificateFacts {
    /// The DER of the subject name, tag and length included.
    pub(super) subject: Vec<u8>,
    /// The DER of the issuer name, tag and length included.
    pub(super) issuer: Vec<u8>,
    /// The serial number as encoded, without tag and length.
    pub(super) serial: Vec<u8>,
    pub(super) not_before: i64,
    pub(super) not_after: i64,
    /// Whether basicConstraints is present and asserts cA.
    pub(super) is_ca: bool,
    pub(super) path_len: Option<u32>,
    pub(super) key_usage: Option<KeyUsageFacts>,
    pub(super) extended_key_usage: Option<ExtendedKeyUsageFacts>,
    pub(super) name_constraints: Option<NameConstraintForms>,
    pub(super) subject_key_identifier: Option<Vec<u8>>,
    pub(super) authority_key_identifier: Option<Vec<u8>>,
    /// The SRV-ID and XmppAddr entries, or why they are unusable. Only the
    /// end-entity certificate's identifiers are ever consulted.
    pub(super) identifiers: Result<XmppIdentifiers, IdentifierError>,
}

impl CertificateFacts {
    /// Read the metadata of one DER certificate. The ceilings are enforced
    /// on the raw bytes before the allocating parser runs.
    pub(super) fn read(der: &[u8]) -> Result<Self, FactsError> {
        if der.len() > MAX_CERTIFICATE_DER_BYTES {
            return Err(FactsError::TooLarge);
        }
        let shape = certificate_shape(der).map_err(|_| FactsError::Malformed)?;
        if shape.extensions > MAX_CERTIFICATE_EXTENSIONS {
            return Err(FactsError::TooManyExtensions);
        }
        if shape.subject_alt_names > MAX_SUBJECT_ALT_NAMES
            || shape.name_constraint_subtrees > MAX_NAME_CONSTRAINT_SUBTREES
        {
            return Err(FactsError::TooManyNames);
        }

        let (rest, certificate) =
            X509Certificate::from_der(der).map_err(|_| FactsError::Malformed)?;
        if !rest.is_empty() {
            return Err(FactsError::Malformed);
        }

        let mut facts = Self {
            subject: certificate.subject().as_raw().to_vec(),
            issuer: certificate.issuer().as_raw().to_vec(),
            serial: certificate.raw_serial().to_vec(),
            not_before: certificate.validity().not_before.timestamp(),
            not_after: certificate.validity().not_after.timestamp(),
            is_ca: false,
            path_len: None,
            key_usage: None,
            extended_key_usage: None,
            name_constraints: None,
            subject_key_identifier: None,
            authority_key_identifier: None,
            identifiers: Ok(XmppIdentifiers::default()),
        };

        let extensions = certificate.extensions();
        for (index, extension) in extensions.iter().enumerate() {
            let identifier = extension.oid.as_bytes();
            // RFC 5280 §4.2: a certificate must not carry an extension twice.
            if extensions[..index]
                .iter()
                .any(|earlier| earlier.oid.as_bytes() == identifier)
            {
                return Err(FactsError::Malformed);
            }
            match (identifier, extension.parsed_extension()) {
                (OID_KEY_USAGE, ParsedExtension::KeyUsage(usage)) => {
                    facts.key_usage = Some(KeyUsageFacts {
                        digital_signature: usage.digital_signature(),
                        key_cert_sign: usage.key_cert_sign(),
                        crl_sign: usage.crl_sign(),
                    });
                }
                (OID_EXTENDED_KEY_USAGE, ParsedExtension::ExtendedKeyUsage(usage)) => {
                    facts.extended_key_usage = Some(ExtendedKeyUsageFacts {
                        server_auth: usage.server_auth,
                        client_auth: usage.client_auth,
                    });
                }
                (OID_BASIC_CONSTRAINTS, ParsedExtension::BasicConstraints(constraints)) => {
                    facts.is_ca = constraints.ca;
                    facts.path_len = constraints.path_len_constraint;
                }
                (OID_SUBJECT_ALT_NAME, ParsedExtension::SubjectAlternativeName(names)) => {
                    facts.identifiers = xmpp_identifiers(&names.general_names);
                }
                (OID_NAME_CONSTRAINTS, _) => {
                    let forms = single(extension.value, SEQUENCE)
                        .and_then(name_constraint_forms)
                        .map_err(|_| FactsError::Malformed)?;
                    facts.name_constraints = Some(forms);
                }
                (OID_SUBJECT_KEY_IDENTIFIER, ParsedExtension::SubjectKeyIdentifier(key)) => {
                    facts.subject_key_identifier = Some(key.0.to_vec());
                }
                (OID_AUTHORITY_KEY_IDENTIFIER, ParsedExtension::AuthorityKeyIdentifier(key)) => {
                    facts.authority_key_identifier = key
                        .key_identifier
                        .as_ref()
                        .map(|identifier| identifier.0.to_vec());
                }
                (
                    OID_KEY_USAGE
                    | OID_EXTENDED_KEY_USAGE
                    | OID_BASIC_CONSTRAINTS
                    | OID_SUBJECT_ALT_NAME
                    | OID_SUBJECT_KEY_IDENTIFIER
                    | OID_AUTHORITY_KEY_IDENTIFIER,
                    _,
                ) => return Err(FactsError::Malformed),
                _ => {}
            }
        }
        Ok(facts)
    }
}

/// Collect the SRV-ID and XmppAddr entries of a subjectAltName.
///
/// An entry of either type that is not exactly one correctly wrapped string,
/// or that appears twice, makes the whole set unusable: a later well-formed
/// entry never hides an invalid one. Entries of other types are not XMPP
/// identifiers and are left to webpki.
fn xmpp_identifiers(names: &[GeneralName<'_>]) -> Result<XmppIdentifiers, IdentifierError> {
    let mut identifiers = XmppIdentifiers::default();
    for name in names {
        match name {
            GeneralName::OtherName(type_id, value) if type_id.as_bytes() == OID_SRV_NAME => {
                let text = other_name_string(value, IA5_STRING)
                    .ok()
                    .filter(|text| text.is_ascii())
                    .and_then(|text| std::str::from_utf8(text).ok())
                    .ok_or(IdentifierError::Malformed)?;
                let srv_name = SrvName::parse(text).ok_or(IdentifierError::Malformed)?;
                if identifiers.srv_names.contains(&srv_name) {
                    return Err(IdentifierError::Duplicate);
                }
                identifiers.srv_names.push(srv_name);
            }
            GeneralName::OtherName(type_id, value) if type_id.as_bytes() == OID_XMPP_ADDR => {
                let text = other_name_string(value, UTF8_STRING)
                    .ok()
                    .and_then(|text| std::str::from_utf8(text).ok())
                    .filter(|text| !text.is_empty())
                    .ok_or(IdentifierError::Malformed)?;
                if identifiers.xmpp_addrs.iter().any(|addr| addr == text) {
                    return Err(IdentifierError::Duplicate);
                }
                identifiers.xmpp_addrs.push(text.to_owned());
            }
            // An otherName whose type-id cannot be read might be either
            // identifier.
            GeneralName::Invalid(tag, _) if tag.0 == 0 => return Err(IdentifierError::Malformed),
            _ => {}
        }
    }
    Ok(identifiers)
}
