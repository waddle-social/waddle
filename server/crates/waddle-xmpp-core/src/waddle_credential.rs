//! Versioned `urn:waddle:credential:1` XMPP IQ protocol for brokered inference
//! credential custody, client-pinned subkey distribution, device-key enrollment,
//! and administrative policy mutations.
//!
//! All XML construction uses typed [`minidom::Element::builder`] and
//! [`minidom::rxml::xml_ncname!`]. All parsing uses [`TryFrom<&Element>`].
//! Every response payload carries only opaque metadata or public keys—never
//! plaintext secrets or private key material—so XEP-0198 `sm_unacked` storage
//! remains secret-free.

use minidom::rxml::xml_ncname;
use minidom::Element;
use thiserror::Error;

/// Canonical namespace for version 1 of the Waddle credential IQ protocol.
pub const NS_WADDLE_CREDENTIAL: &str = "urn:waddle:credential:1";

/// Prefix matching any version of the Waddle credential IQ namespace.
pub const NS_WADDLE_CREDENTIAL_PREFIX: &str = "urn:waddle:credential:";

/// Maximum permitted serialized size (in bytes) for a credential IQ stanza.
pub const MAX_CREDENTIAL_IQ_BYTES: usize = 32 * 1024;

/// Supported protocol versions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CredentialProtocolVersion {
    /// Protocol version `"1"`.
    V1,
}

impl CredentialProtocolVersion {
    /// Returns the wire attribute value.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::V1 => "1",
        }
    }

    /// Parses a wire version token.
    ///
    /// # Errors
    /// Returns [`CredentialParseError::UnsupportedVersion`] for any unknown version.
    pub fn from_token(raw: &str) -> Result<Self, CredentialParseError> {
        match raw {
            "1" => Ok(Self::V1),
            _ => Err(CredentialParseError::UnsupportedVersion),
        }
    }
}

/// Closed-set credential purpose.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CredentialPurpose {
    /// Tenant-funded shared inference credential.
    TenantInference,
    /// Member-funded personal inference credential.
    MemberInference,
}

impl CredentialPurpose {
    /// Returns the canonical wire token.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::TenantInference => "tenant-inference",
            Self::MemberInference => "member-inference",
        }
    }

    /// Parses a wire purpose token.
    ///
    /// # Errors
    /// Returns [`CredentialParseError::InvalidAttribute`] for unknown values.
    pub fn from_token(raw: &str) -> Result<Self, CredentialParseError> {
        match raw {
            "tenant-inference" => Ok(Self::TenantInference),
            "member-inference" => Ok(Self::MemberInference),
            _ => Err(CredentialParseError::InvalidAttribute("purpose")),
        }
    }
}

/// Closed-set credential lifecycle status.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CredentialStatus {
    /// Active and eligible for brokered egress.
    Active,
    /// Superseded by a newer version.
    Rotated,
    /// Permanently revoked and recorded in the destruction ledger.
    Revoked,
    /// Expired.
    Expired,
}

impl CredentialStatus {
    /// Returns the canonical wire token.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Rotated => "rotated",
            Self::Revoked => "revoked",
            Self::Expired => "expired",
        }
    }

    /// Parses a wire status token.
    ///
    /// # Errors
    /// Returns [`CredentialParseError::InvalidAttribute`] for unknown values.
    pub fn from_token(raw: &str) -> Result<Self, CredentialParseError> {
        match raw {
            "active" => Ok(Self::Active),
            "rotated" => Ok(Self::Rotated),
            "revoked" => Ok(Self::Revoked),
            "expired" => Ok(Self::Expired),
            _ => Err(CredentialParseError::InvalidAttribute("status")),
        }
    }
}

/// Supported Broker Encryption Subkey (BESK) algorithms.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SubkeyAlg {
    /// NIST P-256 ECDH.
    P256Ecdh,
}

impl SubkeyAlg {
    /// Returns the canonical wire token.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::P256Ecdh => "p256-ecdh",
        }
    }

    /// Parses a wire subkey algorithm token.
    ///
    /// # Errors
    /// Returns [`CredentialParseError::InvalidAttribute`] for unknown values.
    pub fn from_token(raw: &str) -> Result<Self, CredentialParseError> {
        match raw {
            "p256-ecdh" => Ok(Self::P256Ecdh),
            _ => Err(CredentialParseError::InvalidAttribute("alg")),
        }
    }
}

/// Supported client-to-broker envelope sealing algorithms.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SealAlg {
    /// Ephemeral P-256 ECDH + HKDF-SHA256 + AES-256-GCM.
    P256EcdhHkdfSha256Aes256Gcm,
}

impl SealAlg {
    /// Returns the canonical wire token.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::P256EcdhHkdfSha256Aes256Gcm => "p256-ecdh-hkdf-sha256-aes256gcm",
        }
    }

    /// Parses a wire seal algorithm token.
    ///
    /// # Errors
    /// Returns [`CredentialParseError::InvalidAttribute`] for unknown values.
    pub fn from_token(raw: &str) -> Result<Self, CredentialParseError> {
        match raw {
            "p256-ecdh-hkdf-sha256-aes256gcm" => Ok(Self::P256EcdhHkdfSha256Aes256Gcm),
            _ => Err(CredentialParseError::InvalidAttribute("alg")),
        }
    }
}

/// Supported signature algorithms for Root BKSK and device keys.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SigningAlg {
    /// NIST P-256 ECDSA (SHA-256).
    P256Ecdsa,
}

impl SigningAlg {
    /// Returns the canonical wire token.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::P256Ecdsa => "p256-ecdsa",
        }
    }

    /// Parses a wire signing algorithm token.
    ///
    /// # Errors
    /// Returns [`CredentialParseError::InvalidAttribute`] for unknown values.
    pub fn from_token(raw: &str) -> Result<Self, CredentialParseError> {
        match raw {
            "p256-ecdsa" => Ok(Self::P256Ecdsa),
            _ => Err(CredentialParseError::InvalidAttribute("alg")),
        }
    }
}

/// Wire representation of an independent IdP proof (`Colony` JWT or WebAuthn).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IdpProofXml {
    /// Colony OIDC broker-audience JWT.
    ColonyJwt {
        /// Compact serialized JWT string.
        token: String,
    },
    /// WebAuthn assertion verified against IdP-attested P-256 key.
    WebAuthn {
        /// WebAuthn credential identifier.
        credential_id: String,
        /// Base64-encoded authenticatorData bytes.
        authenticator_data_b64: String,
        /// Base64-encoded clientDataJSON bytes.
        client_data_json_b64: String,
        /// Base64-encoded ECDSA signature bytes.
        signature_b64: String,
    },
}

impl IdpProofXml {
    /// Builds the `<idp-proof xmlns='urn:waddle:credential:1'/>` element.
    #[must_use]
    pub fn build_element(&self) -> Element {
        match self {
            Self::ColonyJwt { token } => Element::builder("idp-proof", NS_WADDLE_CREDENTIAL)
                .attr(xml_ncname!("kind").to_owned(), "colony-jwt")
                .append(token.as_str())
                .build(),
            Self::WebAuthn {
                credential_id,
                authenticator_data_b64,
                client_data_json_b64,
                signature_b64,
            } => Element::builder("idp-proof", NS_WADDLE_CREDENTIAL)
                .attr(xml_ncname!("kind").to_owned(), "webauthn")
                .attr(
                    xml_ncname!("credential-id").to_owned(),
                    credential_id.as_str(),
                )
                .append(
                    Element::builder("authenticator-data", NS_WADDLE_CREDENTIAL)
                        .append(authenticator_data_b64.as_str())
                        .build(),
                )
                .append(
                    Element::builder("client-data-json", NS_WADDLE_CREDENTIAL)
                        .append(client_data_json_b64.as_str())
                        .build(),
                )
                .append(
                    Element::builder("signature", NS_WADDLE_CREDENTIAL)
                        .append(signature_b64.as_str())
                        .build(),
                )
                .build(),
        }
    }
}

impl TryFrom<&Element> for IdpProofXml {
    type Error = CredentialParseError;

    fn try_from(el: &Element) -> Result<Self, Self::Error> {
        if !el.is("idp-proof", NS_WADDLE_CREDENTIAL) {
            return Err(CredentialParseError::UnexpectedElement);
        }
        match req_attr(el, "kind")? {
            "colony-jwt" => {
                let token = el.text();
                if token.trim().is_empty() {
                    return Err(CredentialParseError::MissingText("idp-proof"));
                }
                Ok(Self::ColonyJwt { token })
            }
            "webauthn" => {
                let credential_id = req_attr(el, "credential-id")?.to_owned();
                let authenticator_data_b64 = req_child_text(el, "authenticator-data")?;
                let client_data_json_b64 = req_child_text(el, "client-data-json")?;
                let signature_b64 = req_child_text(el, "signature")?;
                Ok(Self::WebAuthn {
                    credential_id,
                    authenticator_data_b64,
                    client_data_json_b64,
                    signature_b64,
                })
            }
            _ => Err(CredentialParseError::InvalidAttribute("kind")),
        }
    }
}

/// Client-sealed payload element (`<sealed-payload/>`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SealedPayloadXml {
    /// Broker encryption subkey UUID.
    pub subkey_id: String,
    /// Sealing algorithm.
    pub alg: SealAlg,
    /// Base64-encoded uncompressed SEC1 ephemeral P-256 public key.
    pub ephemeral_key_b64: String,
    /// Base64-encoded AES-256-GCM ciphertext + tag.
    pub ciphertext_b64: String,
}

impl SealedPayloadXml {
    /// Builds the `<sealed-payload/>` element.
    #[must_use]
    pub fn build_element(&self) -> Element {
        Element::builder("sealed-payload", NS_WADDLE_CREDENTIAL)
            .attr(xml_ncname!("subkey-id").to_owned(), self.subkey_id.as_str())
            .attr(xml_ncname!("alg").to_owned(), self.alg.as_str())
            .append(
                Element::builder("ephemeral-key", NS_WADDLE_CREDENTIAL)
                    .attr(xml_ncname!("encoding").to_owned(), "base64")
                    .append(self.ephemeral_key_b64.as_str())
                    .build(),
            )
            .append(
                Element::builder("ciphertext", NS_WADDLE_CREDENTIAL)
                    .attr(xml_ncname!("encoding").to_owned(), "base64")
                    .append(self.ciphertext_b64.as_str())
                    .build(),
            )
            .build()
    }
}

impl TryFrom<&Element> for SealedPayloadXml {
    type Error = CredentialParseError;

    fn try_from(el: &Element) -> Result<Self, Self::Error> {
        if !el.is("sealed-payload", NS_WADDLE_CREDENTIAL) {
            return Err(CredentialParseError::UnexpectedElement);
        }
        let subkey_id = req_attr(el, "subkey-id")?.to_owned();
        let alg = SealAlg::from_token(req_attr(el, "alg")?)?;
        let ephemeral_key_b64 = req_child_text(el, "ephemeral-key")?;
        let ciphertext_b64 = req_child_text(el, "ciphertext")?;
        Ok(Self {
            subkey_id,
            alg,
            ephemeral_key_b64,
            ciphertext_b64,
        })
    }
}

/// Optional device signature element (`<device-signature/>`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceSignatureXml {
    /// Enrolled device key UUID.
    pub device_key_id: String,
    /// Signature algorithm.
    pub alg: SigningAlg,
    /// Base64-encoded signature bytes.
    pub signature_b64: String,
}

impl DeviceSignatureXml {
    /// Builds the `<device-signature/>` element.
    #[must_use]
    pub fn build_element(&self) -> Element {
        Element::builder("device-signature", NS_WADDLE_CREDENTIAL)
            .attr(
                xml_ncname!("device-key-id").to_owned(),
                self.device_key_id.as_str(),
            )
            .attr(xml_ncname!("alg").to_owned(), self.alg.as_str())
            .attr(xml_ncname!("encoding").to_owned(), "base64")
            .append(self.signature_b64.as_str())
            .build()
    }
}

impl TryFrom<&Element> for DeviceSignatureXml {
    type Error = CredentialParseError;

    fn try_from(el: &Element) -> Result<Self, Self::Error> {
        if !el.is("device-signature", NS_WADDLE_CREDENTIAL) {
            return Err(CredentialParseError::UnexpectedElement);
        }
        let device_key_id = req_attr(el, "device-key-id")?.to_owned();
        let alg = SigningAlg::from_token(req_attr(el, "alg")?)?;
        let signature_b64 = el.text();
        if signature_b64.trim().is_empty() {
            return Err(CredentialParseError::MissingText("device-signature"));
        }
        Ok(Self {
            device_key_id,
            alg,
            signature_b64,
        })
    }
}

/// Device signing key enrollment request (`<enroll-device/>`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnrollDeviceRequest {
    /// Protocol version.
    pub version: CredentialProtocolVersion,
    /// Canonical tenant ID.
    pub tenant_id: String,
    /// Immutable account UUID.
    pub account_id: String,
    /// Client-chosen device key UUID.
    pub device_key_id: String,
    /// Signing algorithm.
    pub alg: SigningAlg,
    /// Single-use enrollment nonce.
    pub nonce: String,
    /// Expiration timestamp in Unix milliseconds.
    pub expires_at_ms: i64,
    /// Base64-encoded uncompressed SEC1 P-256 public key.
    pub device_public_key_b64: String,
    /// Base64-encoded proof-of-possession signature.
    pub pop_signature_b64: String,
    /// Mandatory independent IdP proof.
    pub idp_proof: IdpProofXml,
}

/// Credential enrollment request (`<enroll-credential/>`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnrollCredentialRequest {
    /// Protocol version.
    pub version: CredentialProtocolVersion,
    /// Canonical tenant ID.
    pub tenant_id: String,
    /// Optional immutable account UUID (present for member-funded credentials).
    pub account_id: Option<String>,
    /// Closed provider identifier.
    pub provider: String,
    /// Closed credential purpose.
    pub purpose: CredentialPurpose,
    /// Idempotent single-use XEP-0198 replay nonce.
    pub nonce: String,
    /// Expiration timestamp in Unix milliseconds.
    pub expires_at_ms: i64,
    /// Client-sealed payload.
    pub sealed_payload: SealedPayloadXml,
    /// Mandatory independent IdP proof.
    pub idp_proof: IdpProofXml,
    /// Optional enrolled device signature.
    pub device_signature: Option<DeviceSignatureXml>,
}

/// Credential status query (`<credential-status/>`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CredentialStatusQuery {
    /// Canonical tenant ID.
    pub tenant_id: String,
    /// Optional provider filter.
    pub provider: Option<String>,
    /// Optional purpose filter.
    pub purpose: Option<CredentialPurpose>,
}

/// Credential rotation request (`<rotate-credential/>`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RotateCredentialRequest {
    /// Protocol version.
    pub version: CredentialProtocolVersion,
    /// Target credential UUID.
    pub credential_id: String,
    /// Canonical tenant ID.
    pub tenant_id: String,
    /// Optional immutable account UUID.
    pub account_id: Option<String>,
    /// Closed provider identifier.
    pub provider: String,
    /// Closed credential purpose.
    pub purpose: CredentialPurpose,
    /// Single-use nonce.
    pub nonce: String,
    /// Expiration timestamp in Unix milliseconds.
    pub expires_at_ms: i64,
    /// Client-sealed replacement secret.
    pub sealed_payload: SealedPayloadXml,
    /// Mandatory independent IdP proof.
    pub idp_proof: IdpProofXml,
    /// Optional device signature.
    pub device_signature: Option<DeviceSignatureXml>,
}

/// Credential revocation request (`<revoke-credential/>`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RevokeCredentialRequest {
    /// Protocol version.
    pub version: CredentialProtocolVersion,
    /// Target credential UUID.
    pub credential_id: String,
    /// Canonical tenant ID.
    pub tenant_id: String,
    /// Optional immutable account UUID.
    pub account_id: Option<String>,
    /// Single-use nonce.
    pub nonce: String,
    /// Expiration timestamp in Unix milliseconds.
    pub expires_at_ms: i64,
    /// Optional IdP proof (either `idp_proof` or `device_signature` must be present).
    pub idp_proof: Option<IdpProofXml>,
    /// Optional device signature.
    pub device_signature: Option<DeviceSignatureXml>,
}

/// Administrative policy mutation request (`<mutate-policy/>`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MutatePolicyRequest {
    /// Protocol version.
    pub version: CredentialProtocolVersion,
    /// Canonical tenant ID.
    pub tenant_id: String,
    /// Policy scope (`"tenant"` or `"room"`).
    pub scope: String,
    /// Target identifier (tenant ID or room JID).
    pub target_id: String,
    /// Policy action (`"allow-provider"`, `"deny-provider"`, `"set-room-policy"`).
    pub action: String,
    /// Provider token.
    pub provider: String,
    /// Single-use nonce.
    pub nonce: String,
    /// Expiration timestamp in Unix milliseconds.
    pub expires_at_ms: i64,
    /// Mandatory independent IdP admin proof.
    pub idp_proof: IdpProofXml,
}

/// Top-level typed `urn:waddle:credential:1` IQ request payload.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CredentialRequest {
    /// `<get-subkey/>` (`IQ-get`).
    GetSubkey,
    /// `<enroll-device/>` (`IQ-set`).
    EnrollDevice(EnrollDeviceRequest),
    /// `<enroll-credential/>` (`IQ-set`).
    EnrollCredential(EnrollCredentialRequest),
    /// `<credential-status/>` (`IQ-get`).
    Status(CredentialStatusQuery),
    /// `<rotate-credential/>` (`IQ-set`).
    RotateCredential(RotateCredentialRequest),
    /// `<revoke-credential/>` (`IQ-set`).
    RevokeCredential(RevokeCredentialRequest),
    /// `<mutate-policy/>` (`IQ-set`).
    MutatePolicy(MutatePolicyRequest),
}

impl CredentialRequest {
    /// Serializes the typed request into a [`minidom::Element`].
    #[must_use]
    pub fn build_element(&self) -> Element {
        match self {
            Self::GetSubkey => Element::builder("get-subkey", NS_WADDLE_CREDENTIAL).build(),
            Self::EnrollDevice(req) => Element::builder("enroll-device", NS_WADDLE_CREDENTIAL)
                .attr(xml_ncname!("version").to_owned(), req.version.as_str())
                .attr(xml_ncname!("tenant-id").to_owned(), req.tenant_id.as_str())
                .attr(
                    xml_ncname!("account-id").to_owned(),
                    req.account_id.as_str(),
                )
                .attr(
                    xml_ncname!("device-key-id").to_owned(),
                    req.device_key_id.as_str(),
                )
                .attr(xml_ncname!("alg").to_owned(), req.alg.as_str())
                .attr(xml_ncname!("nonce").to_owned(), req.nonce.as_str())
                .attr(
                    xml_ncname!("expires-at-ms").to_owned(),
                    req.expires_at_ms.to_string(),
                )
                .append(
                    Element::builder("device-public-key", NS_WADDLE_CREDENTIAL)
                        .attr(xml_ncname!("encoding").to_owned(), "base64")
                        .append(req.device_public_key_b64.as_str())
                        .build(),
                )
                .append(
                    Element::builder("pop-signature", NS_WADDLE_CREDENTIAL)
                        .attr(xml_ncname!("encoding").to_owned(), "base64")
                        .append(req.pop_signature_b64.as_str())
                        .build(),
                )
                .append(req.idp_proof.build_element())
                .build(),
            Self::EnrollCredential(req) => {
                let mut builder = Element::builder("enroll-credential", NS_WADDLE_CREDENTIAL)
                    .attr(xml_ncname!("version").to_owned(), req.version.as_str())
                    .attr(xml_ncname!("tenant-id").to_owned(), req.tenant_id.as_str())
                    .attr(xml_ncname!("provider").to_owned(), req.provider.as_str())
                    .attr(xml_ncname!("purpose").to_owned(), req.purpose.as_str())
                    .attr(xml_ncname!("nonce").to_owned(), req.nonce.as_str())
                    .attr(
                        xml_ncname!("expires-at-ms").to_owned(),
                        req.expires_at_ms.to_string(),
                    );
                if let Some(acct) = &req.account_id {
                    builder = builder.attr(xml_ncname!("account-id").to_owned(), acct.as_str());
                }
                builder = builder
                    .append(req.sealed_payload.build_element())
                    .append(req.idp_proof.build_element());
                if let Some(dev_sig) = &req.device_signature {
                    builder = builder.append(dev_sig.build_element());
                }
                builder.build()
            }
            Self::Status(q) => {
                let mut builder = Element::builder("credential-status", NS_WADDLE_CREDENTIAL)
                    .attr(xml_ncname!("tenant-id").to_owned(), q.tenant_id.as_str());
                if let Some(p) = &q.provider {
                    builder = builder.attr(xml_ncname!("provider").to_owned(), p.as_str());
                }
                if let Some(purpose) = q.purpose {
                    builder = builder.attr(xml_ncname!("purpose").to_owned(), purpose.as_str());
                }
                builder.build()
            }
            Self::RotateCredential(req) => {
                let mut builder = Element::builder("rotate-credential", NS_WADDLE_CREDENTIAL)
                    .attr(xml_ncname!("version").to_owned(), req.version.as_str())
                    .attr(
                        xml_ncname!("credential-id").to_owned(),
                        req.credential_id.as_str(),
                    )
                    .attr(xml_ncname!("tenant-id").to_owned(), req.tenant_id.as_str())
                    .attr(xml_ncname!("provider").to_owned(), req.provider.as_str())
                    .attr(xml_ncname!("purpose").to_owned(), req.purpose.as_str())
                    .attr(xml_ncname!("nonce").to_owned(), req.nonce.as_str())
                    .attr(
                        xml_ncname!("expires-at-ms").to_owned(),
                        req.expires_at_ms.to_string(),
                    );
                if let Some(acct) = &req.account_id {
                    builder = builder.attr(xml_ncname!("account-id").to_owned(), acct.as_str());
                }
                builder = builder
                    .append(req.sealed_payload.build_element())
                    .append(req.idp_proof.build_element());
                if let Some(dev_sig) = &req.device_signature {
                    builder = builder.append(dev_sig.build_element());
                }
                builder.build()
            }
            Self::RevokeCredential(req) => {
                let mut builder = Element::builder("revoke-credential", NS_WADDLE_CREDENTIAL)
                    .attr(xml_ncname!("version").to_owned(), req.version.as_str())
                    .attr(
                        xml_ncname!("credential-id").to_owned(),
                        req.credential_id.as_str(),
                    )
                    .attr(xml_ncname!("tenant-id").to_owned(), req.tenant_id.as_str())
                    .attr(xml_ncname!("nonce").to_owned(), req.nonce.as_str())
                    .attr(
                        xml_ncname!("expires-at-ms").to_owned(),
                        req.expires_at_ms.to_string(),
                    );
                if let Some(acct) = &req.account_id {
                    builder = builder.attr(xml_ncname!("account-id").to_owned(), acct.as_str());
                }
                if let Some(idp) = &req.idp_proof {
                    builder = builder.append(idp.build_element());
                }
                if let Some(dev_sig) = &req.device_signature {
                    builder = builder.append(dev_sig.build_element());
                }
                builder.build()
            }
            Self::MutatePolicy(req) => Element::builder("mutate-policy", NS_WADDLE_CREDENTIAL)
                .attr(xml_ncname!("version").to_owned(), req.version.as_str())
                .attr(xml_ncname!("tenant-id").to_owned(), req.tenant_id.as_str())
                .attr(xml_ncname!("scope").to_owned(), req.scope.as_str())
                .attr(xml_ncname!("target-id").to_owned(), req.target_id.as_str())
                .attr(xml_ncname!("action").to_owned(), req.action.as_str())
                .attr(xml_ncname!("provider").to_owned(), req.provider.as_str())
                .attr(xml_ncname!("nonce").to_owned(), req.nonce.as_str())
                .attr(
                    xml_ncname!("expires-at-ms").to_owned(),
                    req.expires_at_ms.to_string(),
                )
                .append(req.idp_proof.build_element())
                .build(),
        }
    }
}

impl TryFrom<&Element> for CredentialRequest {
    type Error = CredentialParseError;

    fn try_from(el: &Element) -> Result<Self, Self::Error> {
        if el.ns() != NS_WADDLE_CREDENTIAL {
            if el.ns().starts_with(NS_WADDLE_CREDENTIAL_PREFIX) {
                return Err(CredentialParseError::UnsupportedVersion);
            }
            return Err(CredentialParseError::WrongNamespace);
        }
        match el.name() {
            "get-subkey" => Ok(Self::GetSubkey),
            "enroll-device" => {
                let version = CredentialProtocolVersion::from_token(req_attr(el, "version")?)?;
                let tenant_id = req_attr(el, "tenant-id")?.to_owned();
                let account_id = req_attr(el, "account-id")?.to_owned();
                let device_key_id = req_attr(el, "device-key-id")?.to_owned();
                let alg = SigningAlg::from_token(req_attr(el, "alg")?)?;
                let nonce = req_attr(el, "nonce")?.to_owned();
                let expires_at_ms = parse_i64_attr(el, "expires-at-ms")?;
                let device_public_key_b64 = req_child_text(el, "device-public-key")?;
                let pop_signature_b64 = req_child_text(el, "pop-signature")?;
                let idp_el = el
                    .get_child("idp-proof", NS_WADDLE_CREDENTIAL)
                    .ok_or(CredentialParseError::MissingChild("idp-proof"))?;
                let idp_proof = IdpProofXml::try_from(idp_el)?;
                Ok(Self::EnrollDevice(EnrollDeviceRequest {
                    version,
                    tenant_id,
                    account_id,
                    device_key_id,
                    alg,
                    nonce,
                    expires_at_ms,
                    device_public_key_b64,
                    pop_signature_b64,
                    idp_proof,
                }))
            }
            "enroll-credential" => {
                let version = CredentialProtocolVersion::from_token(req_attr(el, "version")?)?;
                let tenant_id = req_attr(el, "tenant-id")?.to_owned();
                let account_id = el.attr("account-id").map(str::to_owned);
                let provider = req_attr(el, "provider")?.to_owned();
                let purpose = CredentialPurpose::from_token(req_attr(el, "purpose")?)?;
                let nonce = req_attr(el, "nonce")?.to_owned();
                let expires_at_ms = parse_i64_attr(el, "expires-at-ms")?;
                let sealed_el = el
                    .get_child("sealed-payload", NS_WADDLE_CREDENTIAL)
                    .ok_or(CredentialParseError::MissingChild("sealed-payload"))?;
                let sealed_payload = SealedPayloadXml::try_from(sealed_el)?;
                let idp_el = el
                    .get_child("idp-proof", NS_WADDLE_CREDENTIAL)
                    .ok_or(CredentialParseError::MissingChild("idp-proof"))?;
                let idp_proof = IdpProofXml::try_from(idp_el)?;
                let device_signature = el
                    .get_child("device-signature", NS_WADDLE_CREDENTIAL)
                    .map(DeviceSignatureXml::try_from)
                    .transpose()?;
                Ok(Self::EnrollCredential(EnrollCredentialRequest {
                    version,
                    tenant_id,
                    account_id,
                    provider,
                    purpose,
                    nonce,
                    expires_at_ms,
                    sealed_payload,
                    idp_proof,
                    device_signature,
                }))
            }
            "credential-status" => {
                let tenant_id = req_attr(el, "tenant-id")?.to_owned();
                let provider = el.attr("provider").map(str::to_owned);
                let purpose = el
                    .attr("purpose")
                    .map(CredentialPurpose::from_token)
                    .transpose()?;
                Ok(Self::Status(CredentialStatusQuery {
                    tenant_id,
                    provider,
                    purpose,
                }))
            }
            "rotate-credential" => {
                let version = CredentialProtocolVersion::from_token(req_attr(el, "version")?)?;
                let credential_id = req_attr(el, "credential-id")?.to_owned();
                let tenant_id = req_attr(el, "tenant-id")?.to_owned();
                let account_id = el.attr("account-id").map(str::to_owned);
                let provider = req_attr(el, "provider")?.to_owned();
                let purpose = CredentialPurpose::from_token(req_attr(el, "purpose")?)?;
                let nonce = req_attr(el, "nonce")?.to_owned();
                let expires_at_ms = parse_i64_attr(el, "expires-at-ms")?;
                let sealed_el = el
                    .get_child("sealed-payload", NS_WADDLE_CREDENTIAL)
                    .ok_or(CredentialParseError::MissingChild("sealed-payload"))?;
                let sealed_payload = SealedPayloadXml::try_from(sealed_el)?;
                let idp_el = el
                    .get_child("idp-proof", NS_WADDLE_CREDENTIAL)
                    .ok_or(CredentialParseError::MissingChild("idp-proof"))?;
                let idp_proof = IdpProofXml::try_from(idp_el)?;
                let device_signature = el
                    .get_child("device-signature", NS_WADDLE_CREDENTIAL)
                    .map(DeviceSignatureXml::try_from)
                    .transpose()?;
                Ok(Self::RotateCredential(RotateCredentialRequest {
                    version,
                    credential_id,
                    tenant_id,
                    account_id,
                    provider,
                    purpose,
                    nonce,
                    expires_at_ms,
                    sealed_payload,
                    idp_proof,
                    device_signature,
                }))
            }
            "revoke-credential" => {
                let version = CredentialProtocolVersion::from_token(req_attr(el, "version")?)?;
                let credential_id = req_attr(el, "credential-id")?.to_owned();
                let tenant_id = req_attr(el, "tenant-id")?.to_owned();
                let account_id = el.attr("account-id").map(str::to_owned);
                let nonce = req_attr(el, "nonce")?.to_owned();
                let expires_at_ms = parse_i64_attr(el, "expires-at-ms")?;
                let idp_proof = el
                    .get_child("idp-proof", NS_WADDLE_CREDENTIAL)
                    .map(IdpProofXml::try_from)
                    .transpose()?;
                let device_signature = el
                    .get_child("device-signature", NS_WADDLE_CREDENTIAL)
                    .map(DeviceSignatureXml::try_from)
                    .transpose()?;
                if idp_proof.is_none() && device_signature.is_none() {
                    return Err(CredentialParseError::MissingChild("idp-proof"));
                }
                Ok(Self::RevokeCredential(RevokeCredentialRequest {
                    version,
                    credential_id,
                    tenant_id,
                    account_id,
                    nonce,
                    expires_at_ms,
                    idp_proof,
                    device_signature,
                }))
            }
            "mutate-policy" => {
                let version = CredentialProtocolVersion::from_token(req_attr(el, "version")?)?;
                let tenant_id = req_attr(el, "tenant-id")?.to_owned();
                let scope = req_attr(el, "scope")?.to_owned();
                let target_id = req_attr(el, "target-id")?.to_owned();
                let action = req_attr(el, "action")?.to_owned();
                let provider = req_attr(el, "provider")?.to_owned();
                let nonce = req_attr(el, "nonce")?.to_owned();
                let expires_at_ms = parse_i64_attr(el, "expires-at-ms")?;
                let idp_el = el
                    .get_child("idp-proof", NS_WADDLE_CREDENTIAL)
                    .ok_or(CredentialParseError::MissingChild("idp-proof"))?;
                let idp_proof = IdpProofXml::try_from(idp_el)?;
                Ok(Self::MutatePolicy(MutatePolicyRequest {
                    version,
                    tenant_id,
                    scope,
                    target_id,
                    action,
                    provider,
                    nonce,
                    expires_at_ms,
                    idp_proof,
                }))
            }
            _ => Err(CredentialParseError::UnexpectedElement),
        }
    }
}

/// Signed Broker Encryption Subkey bundle (`<subkey-bundle/>`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubkeyBundleXml {
    /// Protocol version.
    pub version: CredentialProtocolVersion,
    /// Broker trust domain.
    pub trust_domain: String,
    /// Pinned Root BKSK identifier.
    pub root_kid: String,
    /// Rotatable subkey UUID.
    pub subkey_id: String,
    /// Subkey algorithm.
    pub alg: SubkeyAlg,
    /// Monotonic anti-rollback epoch.
    pub key_epoch: u64,
    /// Validity window start (Unix ms).
    pub not_before_ms: i64,
    /// Validity window end (Unix ms).
    pub not_after_ms: i64,
    /// Base64-encoded SEC1 P-256 public key.
    pub public_key_b64: String,
    /// Root signature algorithm.
    pub root_sig_alg: SigningAlg,
    /// Base64-encoded Root BKSK signature.
    pub root_signature_b64: String,
}

/// Opaque device enrollment receipt (`<device-receipt/>`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceReceiptXml {
    /// Enrolled device key UUID.
    pub device_key_id: String,
    /// Status token (`"active"`).
    pub status: String,
}

/// Opaque credential lifecycle receipt (`<credential-receipt/>`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CredentialReceiptXml {
    /// Opaque credential UUID.
    pub credential_id: String,
    /// Credential version.
    pub version: u32,
    /// Closed lifecycle status.
    pub status: CredentialStatus,
}

/// Opaque status row (`<credential/>`) inside `<credential-status-result/>`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CredentialStatusEntry {
    /// Opaque credential UUID.
    pub id: String,
    /// Provider token.
    pub provider: String,
    /// Purpose token.
    pub purpose: CredentialPurpose,
    /// Lifecycle status.
    pub status: CredentialStatus,
    /// Credential version.
    pub version: u32,
    /// Last updated timestamp (Unix ms).
    pub updated_at_ms: i64,
}

/// Opaque policy mutation receipt (`<policy-receipt/>`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PolicyReceiptXml {
    /// Policy identifier.
    pub policy_id: String,
    /// Policy generation counter.
    pub generation: u64,
    /// Status token (`"applied"`).
    pub status: String,
}

/// Top-level opaque `urn:waddle:credential:1` IQ result payload.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CredentialResponse {
    /// `<subkey-bundle/>`
    SubkeyBundle(SubkeyBundleXml),
    /// `<device-receipt/>`
    DeviceReceipt(DeviceReceiptXml),
    /// `<credential-receipt/>`
    CredentialReceipt(CredentialReceiptXml),
    /// `<credential-status-result/>`
    StatusResult(Vec<CredentialStatusEntry>),
    /// `<policy-receipt/>`
    PolicyReceipt(PolicyReceiptXml),
}

impl CredentialResponse {
    /// Serializes the opaque response into a [`minidom::Element`].
    #[must_use]
    pub fn build_element(&self) -> Element {
        match self {
            Self::SubkeyBundle(b) => Element::builder("subkey-bundle", NS_WADDLE_CREDENTIAL)
                .attr(xml_ncname!("version").to_owned(), b.version.as_str())
                .attr(
                    xml_ncname!("trust-domain").to_owned(),
                    b.trust_domain.as_str(),
                )
                .attr(xml_ncname!("root-kid").to_owned(), b.root_kid.as_str())
                .attr(xml_ncname!("subkey-id").to_owned(), b.subkey_id.as_str())
                .attr(xml_ncname!("alg").to_owned(), b.alg.as_str())
                .attr(xml_ncname!("key-epoch").to_owned(), b.key_epoch.to_string())
                .attr(
                    xml_ncname!("not-before-ms").to_owned(),
                    b.not_before_ms.to_string(),
                )
                .attr(
                    xml_ncname!("not-after-ms").to_owned(),
                    b.not_after_ms.to_string(),
                )
                .append(
                    Element::builder("public-key", NS_WADDLE_CREDENTIAL)
                        .attr(xml_ncname!("encoding").to_owned(), "base64")
                        .append(b.public_key_b64.as_str())
                        .build(),
                )
                .append(
                    Element::builder("root-signature", NS_WADDLE_CREDENTIAL)
                        .attr(xml_ncname!("alg").to_owned(), b.root_sig_alg.as_str())
                        .attr(xml_ncname!("encoding").to_owned(), "base64")
                        .append(b.root_signature_b64.as_str())
                        .build(),
                )
                .build(),
            Self::DeviceReceipt(r) => Element::builder("device-receipt", NS_WADDLE_CREDENTIAL)
                .attr(
                    xml_ncname!("device-key-id").to_owned(),
                    r.device_key_id.as_str(),
                )
                .attr(xml_ncname!("status").to_owned(), r.status.as_str())
                .build(),
            Self::CredentialReceipt(r) => {
                Element::builder("credential-receipt", NS_WADDLE_CREDENTIAL)
                    .attr(
                        xml_ncname!("credential-id").to_owned(),
                        r.credential_id.as_str(),
                    )
                    .attr(xml_ncname!("version").to_owned(), r.version.to_string())
                    .attr(xml_ncname!("status").to_owned(), r.status.as_str())
                    .build()
            }
            Self::StatusResult(entries) => {
                let mut builder =
                    Element::builder("credential-status-result", NS_WADDLE_CREDENTIAL);
                for entry in entries {
                    builder = builder.append(
                        Element::builder("credential", NS_WADDLE_CREDENTIAL)
                            .attr(xml_ncname!("id").to_owned(), entry.id.as_str())
                            .attr(xml_ncname!("provider").to_owned(), entry.provider.as_str())
                            .attr(xml_ncname!("purpose").to_owned(), entry.purpose.as_str())
                            .attr(xml_ncname!("status").to_owned(), entry.status.as_str())
                            .attr(xml_ncname!("version").to_owned(), entry.version.to_string())
                            .attr(
                                xml_ncname!("updated-at-ms").to_owned(),
                                entry.updated_at_ms.to_string(),
                            )
                            .build(),
                    );
                }
                builder.build()
            }
            Self::PolicyReceipt(r) => Element::builder("policy-receipt", NS_WADDLE_CREDENTIAL)
                .attr(xml_ncname!("policy-id").to_owned(), r.policy_id.as_str())
                .attr(
                    xml_ncname!("generation").to_owned(),
                    r.generation.to_string(),
                )
                .attr(xml_ncname!("status").to_owned(), r.status.as_str())
                .build(),
        }
    }
}

impl TryFrom<&Element> for CredentialResponse {
    type Error = CredentialParseError;

    fn try_from(el: &Element) -> Result<Self, Self::Error> {
        if el.ns() != NS_WADDLE_CREDENTIAL {
            return Err(CredentialParseError::WrongNamespace);
        }
        match el.name() {
            "subkey-bundle" => {
                let version = CredentialProtocolVersion::from_token(req_attr(el, "version")?)?;
                let trust_domain = req_attr(el, "trust-domain")?.to_owned();
                let root_kid = req_attr(el, "root-kid")?.to_owned();
                let subkey_id = req_attr(el, "subkey-id")?.to_owned();
                let alg = SubkeyAlg::from_token(req_attr(el, "alg")?)?;
                let key_epoch = req_attr(el, "key-epoch")?
                    .parse::<u64>()
                    .map_err(|_| CredentialParseError::InvalidAttribute("key-epoch"))?;
                let not_before_ms = parse_i64_attr(el, "not-before-ms")?;
                let not_after_ms = parse_i64_attr(el, "not-after-ms")?;
                let public_key_b64 = req_child_text(el, "public-key")?;
                let sig_el = el
                    .get_child("root-signature", NS_WADDLE_CREDENTIAL)
                    .ok_or(CredentialParseError::MissingChild("root-signature"))?;
                let root_sig_alg = SigningAlg::from_token(req_attr(sig_el, "alg")?)?;
                let root_signature_b64 = sig_el.text();
                if root_signature_b64.trim().is_empty() {
                    return Err(CredentialParseError::MissingText("root-signature"));
                }
                Ok(Self::SubkeyBundle(SubkeyBundleXml {
                    version,
                    trust_domain,
                    root_kid,
                    subkey_id,
                    alg,
                    key_epoch,
                    not_before_ms,
                    not_after_ms,
                    public_key_b64,
                    root_sig_alg,
                    root_signature_b64,
                }))
            }
            "device-receipt" => Ok(Self::DeviceReceipt(DeviceReceiptXml {
                device_key_id: req_attr(el, "device-key-id")?.to_owned(),
                status: req_attr(el, "status")?.to_owned(),
            })),
            "credential-receipt" => {
                let credential_id = req_attr(el, "credential-id")?.to_owned();
                let version = req_attr(el, "version")?
                    .parse::<u32>()
                    .map_err(|_| CredentialParseError::InvalidAttribute("version"))?;
                let status = CredentialStatus::from_token(req_attr(el, "status")?)?;
                Ok(Self::CredentialReceipt(CredentialReceiptXml {
                    credential_id,
                    version,
                    status,
                }))
            }
            "credential-status-result" => {
                let mut entries = Vec::new();
                for child in el.children() {
                    if !child.is("credential", NS_WADDLE_CREDENTIAL) {
                        continue;
                    }
                    entries.push(CredentialStatusEntry {
                        id: req_attr(child, "id")?.to_owned(),
                        provider: req_attr(child, "provider")?.to_owned(),
                        purpose: CredentialPurpose::from_token(req_attr(child, "purpose")?)?,
                        status: CredentialStatus::from_token(req_attr(child, "status")?)?,
                        version: req_attr(child, "version")?
                            .parse::<u32>()
                            .map_err(|_| CredentialParseError::InvalidAttribute("version"))?,
                        updated_at_ms: parse_i64_attr(child, "updated-at-ms")?,
                    });
                }
                Ok(Self::StatusResult(entries))
            }
            "policy-receipt" => Ok(Self::PolicyReceipt(PolicyReceiptXml {
                policy_id: req_attr(el, "policy-id")?.to_owned(),
                generation: req_attr(el, "generation")?
                    .parse::<u64>()
                    .map_err(|_| CredentialParseError::InvalidAttribute("generation"))?,
                status: req_attr(el, "status")?.to_owned(),
            })),
            _ => Err(CredentialParseError::UnexpectedElement),
        }
    }
}

/// Returns `true` if `el` is in `urn:waddle:credential:1` or any versioned
/// `urn:waddle:credential:*` namespace.
#[must_use]
pub fn is_credential_payload(el: &Element) -> bool {
    el.ns() == NS_WADDLE_CREDENTIAL
        || el.ns() == "urn:waddle:credential"
        || el.ns().starts_with(NS_WADDLE_CREDENTIAL_PREFIX)
}

/// Errors produced when parsing a `urn:waddle:credential:1` XML element.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum CredentialParseError {
    /// Element namespace is not `urn:waddle:credential:1`.
    #[error("wrong XML namespace for credential IQ")]
    WrongNamespace,
    /// Unsupported protocol version in namespace or `version` attribute.
    #[error("unsupported credential protocol version")]
    UnsupportedVersion,
    /// Unknown or unexpected element name.
    #[error("unexpected credential IQ element")]
    UnexpectedElement,
    /// Required attribute is missing or empty.
    #[error("missing required attribute: {0}")]
    MissingAttribute(&'static str),
    /// Attribute value is invalid or out of closed set.
    #[error("invalid attribute value: {0}")]
    InvalidAttribute(&'static str),
    /// Required child element is missing.
    #[error("missing required child element: {0}")]
    MissingChild(&'static str),
    /// Required text content is empty.
    #[error("missing required text content in element: {0}")]
    MissingText(&'static str),
}

fn req_attr<'a>(el: &'a Element, name: &'static str) -> Result<&'a str, CredentialParseError> {
    match el.attr(name) {
        Some(v) if !v.trim().is_empty() => Ok(v),
        _ => Err(CredentialParseError::MissingAttribute(name)),
    }
}

fn parse_i64_attr(el: &Element, name: &'static str) -> Result<i64, CredentialParseError> {
    req_attr(el, name)?
        .parse::<i64>()
        .map_err(|_| CredentialParseError::InvalidAttribute(name))
}

fn req_child_text(el: &Element, child_name: &'static str) -> Result<String, CredentialParseError> {
    let child = el
        .get_child(child_name, NS_WADDLE_CREDENTIAL)
        .ok_or(CredentialParseError::MissingChild(child_name))?;
    let text = child.text();
    if text.trim().is_empty() {
        return Err(CredentialParseError::MissingText(child_name));
    }
    Ok(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn waddle_credential_roundtrip_all_requests_and_responses() {
        let enroll_req = CredentialRequest::EnrollCredential(EnrollCredentialRequest {
            version: CredentialProtocolVersion::V1,
            tenant_id: "example.com".to_owned(),
            account_id: Some("0195a000-0000-7000-8000-000000000002".to_owned()),
            provider: "openrouter".to_owned(),
            purpose: CredentialPurpose::MemberInference,
            nonce: "n-uuid-2".to_owned(),
            expires_at_ms: 1_760_000_300_000,
            sealed_payload: SealedPayloadXml {
                subkey_id: "0195a000-0000-7000-8000-000000000001".to_owned(),
                alg: SealAlg::P256EcdhHkdfSha256Aes256Gcm,
                ephemeral_key_b64: "BGVwaGtleQ==".to_owned(),
                ciphertext_b64: "Y2lwaGVydGV4dA==".to_owned(),
            },
            idp_proof: IdpProofXml::ColonyJwt {
                token: "eyJhbGciOiJFUzI1NiJ9.payload.sig".to_owned(),
            },
            device_signature: Some(DeviceSignatureXml {
                device_key_id: "0195a000-0000-7000-8000-000000000003".to_owned(),
                alg: SigningAlg::P256Ecdsa,
                signature_b64: "c2ln".to_owned(),
            }),
        });
        let el = enroll_req.build_element();
        assert!(is_credential_payload(&el));
        let parsed = CredentialRequest::try_from(&el).expect("parse enroll");
        assert_eq!(parsed, enroll_req);

        let subkey_req = CredentialRequest::GetSubkey;
        assert_eq!(
            CredentialRequest::try_from(&subkey_req.build_element()).expect("parse get-subkey"),
            subkey_req
        );

        let receipt = CredentialResponse::CredentialReceipt(CredentialReceiptXml {
            credential_id: "0195a000-0000-7000-8000-000000000009".to_owned(),
            version: 1,
            status: CredentialStatus::Active,
        });
        let receipt_el = receipt.build_element();
        assert_eq!(
            CredentialResponse::try_from(&receipt_el).expect("parse receipt"),
            receipt
        );
    }

    #[test]
    fn waddle_credential_rejects_unknown_version_namespace() {
        let v99 = Element::builder("enroll-credential", "urn:waddle:credential:99").build();
        assert!(is_credential_payload(&v99));
        assert_eq!(
            CredentialRequest::try_from(&v99),
            Err(CredentialParseError::UnsupportedVersion)
        );
    }
}
