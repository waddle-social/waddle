//! Matrix rows "TLS versions" and "Resumption in configuration": the
//! profile negotiates TLS 1.3 and admits TLS 1.2 with the extended master
//! secret only, refuses older versions, keeps handshake signature
//! verification in both versions, and never resumes a session, issues a
//! ticket or accepts early data.

use std::sync::Arc;

use rustls::server::ServerConnection;
use rustls::version::TLS12;
use rustls::{
    CertificateError, ClientConnection, Connection, HandshakeKind, PeerIncompatible,
    ProtocolVersion, SupportedProtocolVersion,
};
use waddle_server::s2s::verify::{
    initiating_config, receiving_config, OutboundTransport, TlsConfigError, Verification,
    DIRECT_TLS_ALPN, TLS_PROTOCOL_VERSIONS,
};

use crate::harness::{
    attempt, connect, deliver, domain, drive, raw_client_config, raw_server_config,
    resuming_client_config, resuming_server_config, settings, verifier, Fixture, HOME_A, HOME_B,
};
use crate::pki::NOW;

static TLS12_ONLY: &[&SupportedProtocolVersion] = &[&TLS12];

#[test]
fn two_profile_endpoints_negotiate_tls13() {
    let fixture = Fixture::private();
    let home_a = fixture.issuer.issue_for(&[HOME_A]);
    let home_b = fixture.issuer.issue_for(&[HOME_B]);

    let stream = attempt(&fixture.inventory, (HOME_A, &home_a), (HOME_B, &home_b));

    assert!(stream.handshake.completed());
    assert_eq!(
        stream.handshake.client.protocol_version(),
        Some(ProtocolVersion::TLSv1_3)
    );
    assert_eq!(
        stream.handshake.server.protocol_version(),
        Some(ProtocolVersion::TLSv1_3)
    );
}

#[test]
fn a_tls12_only_peer_is_admitted_and_verified_in_both_roles() {
    let fixture = Fixture::private();
    let home_a = fixture.issuer.issue_for(&[HOME_A]);
    let home_b = fixture.issuer.issue_for(&[HOME_B]);

    // A TLS 1.2 initiating server.
    let initiating = verifier(&fixture.inventory, HOME_B, NOW);
    let receiving = verifier(&fixture.inventory, HOME_A, NOW);
    let handshake = connect(
        raw_client_config(NOW, TLS12_ONLY, Arc::clone(&initiating), &home_a, &home_a),
        receiving_config(
            &settings(NOW),
            home_b.certified_key(),
            Arc::clone(&receiving),
        )
        .expect("a receiving configuration"),
        initiating.reference().server_name(),
    );
    assert!(handshake.completed());
    assert_eq!(
        handshake.server.protocol_version(),
        Some(ProtocolVersion::TLSv1_2)
    );
    assert_eq!(
        receiving
            .established(&handshake.server)
            .expect("the receiving connection's evidence")
            .domain(),
        &domain(HOME_A)
    );

    // A TLS 1.2 receiving server.
    let initiating = verifier(&fixture.inventory, HOME_B, NOW);
    let receiving = verifier(&fixture.inventory, HOME_A, NOW);
    let handshake = connect(
        initiating_config(
            &settings(NOW),
            home_a.certified_key(),
            Arc::clone(&initiating),
            OutboundTransport::StartTls,
        )
        .expect("an initiating configuration"),
        raw_server_config(NOW, TLS12_ONLY, Arc::clone(&receiving), &home_b, &home_b),
        initiating.reference().server_name(),
    );
    assert!(handshake.completed());
    assert_eq!(
        handshake.client.protocol_version(),
        Some(ProtocolVersion::TLSv1_2)
    );
    assert_eq!(
        initiating
            .established(&handshake.client)
            .expect("the initiating connection's evidence")
            .domain(),
        &domain(HOME_B)
    );
}

#[test]
fn a_tls12_handshake_establishes_evidence_only_once_it_completes() {
    let fixture = Fixture::private();
    let home_a = fixture.issuer.issue_for(&[HOME_A]);
    let home_b = fixture.issuer.issue_for(&[HOME_B]);
    let initiating = verifier(&fixture.inventory, HOME_B, NOW);
    let receiving = verifier(&fixture.inventory, HOME_A, NOW);
    let client = ClientConnection::new(
        Arc::new(raw_client_config(
            NOW,
            TLS12_ONLY,
            Arc::clone(&initiating),
            &home_a,
            &home_a,
        )),
        initiating.reference().server_name(),
    )
    .expect("an initiating connection");
    let server = ServerConnection::new(Arc::new(
        receiving_config(
            &settings(NOW),
            home_b.certified_key(),
            Arc::clone(&receiving),
        )
        .expect("a receiving configuration"),
    ))
    .expect("a receiving connection");
    let mut client = Connection::Client(client);
    let mut server = Connection::Server(server);
    let mut client_error = None;
    let mut server_error = None;

    // ClientHello.
    assert!(deliver(&mut client, &mut server, &mut server_error) > 0);
    // ServerHello, Certificate, ServerKeyExchange, CertificateRequest and
    // ServerHelloDone: the initiating verifier checks the chain and the
    // ServerKeyExchange signature, so its verification is complete while
    // the handshake is not.
    assert!(deliver(&mut server, &mut client, &mut client_error) > 0);
    assert!(matches!(
        initiating.verification(),
        Verification::Verified(_)
    ));
    assert!(client.is_handshaking());
    assert!(initiating.established(&client).is_none());

    // Certificate, ClientKeyExchange, CertificateVerify and Finished: the
    // receiving verifier checks the chain and the CertificateVerify
    // signature. The initiating side still awaits the server's Finished.
    assert!(deliver(&mut client, &mut server, &mut server_error) > 0);
    assert!(matches!(
        receiving.verification(),
        Verification::Verified(_)
    ));
    assert!(client.is_handshaking());
    assert!(initiating.established(&client).is_none());

    // The server's Finished completes the handshake on both sides.
    assert!(deliver(&mut server, &mut client, &mut client_error) > 0);
    assert!(client_error.is_none() && server_error.is_none());
    assert!(!client.is_handshaking() && !server.is_handshaking());
    assert_eq!(client.protocol_version(), Some(ProtocolVersion::TLSv1_2));
    assert!(initiating.established(&client).is_some());
    assert!(receiving.established(&server).is_some());
}

/// A TLS 1.1 ClientHello: legacy version 0x0302, no supported_versions
/// extension, one ECDHE suite and a signature_algorithms extension.
fn tls11_client_hello() -> Vec<u8> {
    let mut hello = Vec::new();
    // client_version, then a 32-byte random.
    hello.extend_from_slice(&[0x03, 0x02]);
    hello.extend_from_slice(&[0x5a; 32]);
    // An empty session_id.
    hello.push(0x00);
    // cipher_suites: TLS_ECDHE_RSA_WITH_AES_128_GCM_SHA256.
    hello.extend_from_slice(&[0x00, 0x02, 0xc0, 0x2f]);
    // compression_methods: null.
    hello.extend_from_slice(&[0x01, 0x00]);
    // extensions: signature_algorithms with ecdsa_secp256r1_sha256.
    hello.extend_from_slice(&[0x00, 0x08, 0x00, 0x0d, 0x00, 0x04, 0x00, 0x02, 0x04, 0x03]);

    let length = u8::try_from(hello.len()).expect("a short ClientHello");
    let mut record = vec![0x16, 0x03, 0x01, 0x00, length + 4];
    record.extend_from_slice(&[0x01, 0x00, 0x00, length]);
    record.extend_from_slice(&hello);
    record
}

#[test]
fn a_tls11_client_hello_is_refused_before_any_certificate_is_read() {
    let fixture = Fixture::private();
    let home_b = fixture.issuer.issue_for(&[HOME_B]);
    let receiving = verifier(&fixture.inventory, HOME_A, NOW);
    let config = receiving_config(
        &settings(NOW),
        home_b.certified_key(),
        Arc::clone(&receiving),
    )
    .expect("a receiving configuration");
    let mut server = ServerConnection::new(Arc::new(config)).expect("a receiving connection");

    let hello = tls11_client_hello();
    server
        .read_tls(&mut &hello[..])
        .expect("read the ClientHello");
    let error = server
        .process_new_packets()
        .expect_err("TLS 1.1 is outside the profile");

    assert_eq!(
        error,
        rustls::Error::PeerIncompatible(PeerIncompatible::Tls12NotOffered)
    );
    assert_eq!(receiving.verification(), Verification::Pending);
}

/// Wrap a handshake message of `kind` in a TLS 1.2 handshake record.
fn handshake_record(kind: u8, body: &[u8]) -> Vec<u8> {
    let body_length = u32::try_from(body.len()).expect("a short handshake message");
    let [_, high, middle, low] = body_length.to_be_bytes();
    let mut message = vec![kind, high, middle, low];
    message.extend_from_slice(body);
    let record_length = u16::try_from(message.len()).expect("a short record");
    let mut record = vec![0x16, 0x03, 0x03];
    record.extend_from_slice(&record_length.to_be_bytes());
    record.extend_from_slice(&message);
    record
}

/// A TLS 1.2 ClientHello without the extended_master_secret extension:
/// legacy version 0x0303 and no supported_versions extension, one ECDHE
/// suite the receiving configuration supports, and the groups, point
/// formats and signature algorithms it needs.
fn tls12_client_hello_without_ems() -> Vec<u8> {
    let mut hello = Vec::new();
    hello.extend_from_slice(&[0x03, 0x03]);
    hello.extend_from_slice(&[0x5a; 32]);
    // An empty session_id.
    hello.push(0x00);
    // cipher_suites: TLS_ECDHE_ECDSA_WITH_AES_128_GCM_SHA256.
    hello.extend_from_slice(&[0x00, 0x02, 0xc0, 0x2b]);
    // compression_methods: null.
    hello.extend_from_slice(&[0x01, 0x00]);
    let extensions: &[u8] = &[
        // supported_groups: x25519, secp256r1.
        0x00, 0x0a, 0x00, 0x06, 0x00, 0x04, 0x00, 0x1d, 0x00, 0x17,
        // ec_point_formats: uncompressed.
        0x00, 0x0b, 0x00, 0x02, 0x01, 0x00,
        // signature_algorithms: ecdsa_secp256r1_sha256.
        0x00, 0x0d, 0x00, 0x04, 0x00, 0x02, 0x04, 0x03,
    ];
    let length = u16::try_from(extensions.len()).expect("short extensions");
    hello.extend_from_slice(&length.to_be_bytes());
    hello.extend_from_slice(extensions);
    handshake_record(0x01, &hello)
}

/// A TLS 1.2 ServerHello without the extended_master_secret extension. Its
/// random carries no downgrade marker, and its session_id is empty, so the
/// client has nothing else to object to.
fn tls12_server_hello_without_ems() -> Vec<u8> {
    let mut hello = Vec::new();
    hello.extend_from_slice(&[0x03, 0x03]);
    hello.extend_from_slice(&[0x5a; 32]);
    hello.push(0x00);
    // cipher_suite: TLS_ECDHE_ECDSA_WITH_AES_128_GCM_SHA256.
    hello.extend_from_slice(&[0xc0, 0x2b]);
    // compression_method: null.
    hello.push(0x00);
    // An empty extensions block.
    hello.extend_from_slice(&[0x00, 0x00]);
    handshake_record(0x02, &hello)
}

#[test]
fn a_receiving_configuration_refuses_a_tls12_client_without_extended_master_secret() {
    let fixture = Fixture::private();
    let home_b = fixture.issuer.issue_for(&[HOME_B]);
    let receiving = verifier(&fixture.inventory, HOME_A, NOW);
    let config = receiving_config(
        &settings(NOW),
        home_b.certified_key(),
        Arc::clone(&receiving),
    )
    .expect("a receiving configuration");
    let mut server = ServerConnection::new(Arc::new(config)).expect("a receiving connection");

    let hello = tls12_client_hello_without_ems();
    server
        .read_tls(&mut &hello[..])
        .expect("read the ClientHello");
    let error = server
        .process_new_packets()
        .expect_err("TLS 1.2 without the extended master secret");

    assert_eq!(
        error,
        rustls::Error::PeerIncompatible(PeerIncompatible::ExtendedMasterSecretExtensionRequired)
    );
    assert_eq!(receiving.verification(), Verification::Pending);
}

#[test]
fn an_initiating_configuration_refuses_a_tls12_server_without_extended_master_secret() {
    let fixture = Fixture::private();
    let home_a = fixture.issuer.issue_for(&[HOME_A]);
    let initiating = verifier(&fixture.inventory, HOME_B, NOW);
    let config = initiating_config(
        &settings(NOW),
        home_a.certified_key(),
        Arc::clone(&initiating),
        OutboundTransport::StartTls,
    )
    .expect("an initiating configuration");
    let mut client = ClientConnection::new(Arc::new(config), initiating.reference().server_name())
        .expect("an initiating connection");
    // The ClientHello is discarded; the ServerHello answers it.
    let mut sent = Vec::new();
    client.write_tls(&mut sent).expect("write the ClientHello");

    let hello = tls12_server_hello_without_ems();
    client
        .read_tls(&mut &hello[..])
        .expect("read the ServerHello");
    let error = client
        .process_new_packets()
        .expect_err("TLS 1.2 without the extended master secret");

    assert_eq!(
        error,
        rustls::Error::PeerIncompatible(PeerIncompatible::ExtendedMasterSecretExtensionRequired)
    );
    assert_eq!(initiating.verification(), Verification::Pending);
}

#[test]
fn the_profile_lists_tls13_before_tls12_and_nothing_else() {
    let versions: Vec<ProtocolVersion> = TLS_PROTOCOL_VERSIONS
        .iter()
        .map(|version| version.version)
        .collect();

    assert_eq!(
        versions,
        vec![ProtocolVersion::TLSv1_3, ProtocolVersion::TLSv1_2]
    );
}

#[test]
fn the_configurations_turn_off_resumption_tickets_and_early_data() {
    let fixture = Fixture::private();
    let home_a = fixture.issuer.issue_for(&[HOME_A]);
    let home_b = fixture.issuer.issue_for(&[HOME_B]);

    let client = initiating_config(
        &settings(NOW),
        home_a.certified_key(),
        verifier(&fixture.inventory, HOME_B, NOW),
        OutboundTransport::StartTls,
    )
    .expect("an initiating configuration");
    assert!(!client.enable_early_data);
    assert!(client.enable_sni);
    assert!(client.require_ems);
    assert!(!client.enable_secret_extraction);
    assert!(client.alpn_protocols.is_empty());
    assert!(client.cert_compressors.is_empty());
    assert!(client.cert_decompressors.is_empty());

    let server = receiving_config(
        &settings(NOW),
        home_b.certified_key(),
        verifier(&fixture.inventory, HOME_A, NOW),
    )
    .expect("a receiving configuration");
    assert_eq!(server.send_tls13_tickets, 0);
    assert_eq!(server.max_early_data_size, 0);
    assert!(!server.send_half_rtt_data);
    assert!(server.require_ems);
    assert!(!server.enable_secret_extraction);
    assert!(server.alpn_protocols.is_empty());
    assert!(server.cert_compressors.is_empty());
    assert!(server.cert_decompressors.is_empty());
    assert!(!server.session_storage.can_cache());
    assert!(!server.ticketer.enabled());
}

#[test]
fn a_direct_tls_connection_offers_the_xmpp_server_protocol() {
    let fixture = Fixture::private();
    let home_a = fixture.issuer.issue_for(&[HOME_A]);
    let home_b = fixture.issuer.issue_for(&[HOME_B]);
    let initiating = verifier(&fixture.inventory, HOME_B, NOW);
    let receiving = verifier(&fixture.inventory, HOME_A, NOW);
    let client = initiating_config(
        &settings(NOW),
        home_a.certified_key(),
        Arc::clone(&initiating),
        OutboundTransport::DirectTls,
    )
    .expect("an initiating configuration");
    assert_eq!(client.alpn_protocols, vec![DIRECT_TLS_ALPN.to_vec()]);
    let mut server = raw_server_config(
        NOW,
        TLS_PROTOCOL_VERSIONS,
        Arc::clone(&receiving),
        &home_b,
        &home_b,
    );
    server.alpn_protocols = vec![DIRECT_TLS_ALPN.to_vec()];

    let handshake = connect(client, server, initiating.reference().server_name());

    assert!(handshake.completed());
    assert_eq!(handshake.client.alpn_protocol(), Some(DIRECT_TLS_ALPN));
    assert_eq!(
        initiating
            .established(&handshake.client)
            .expect("the initiating connection's evidence")
            .domain(),
        &domain(HOME_B)
    );
}

#[test]
fn a_configuration_refuses_a_key_that_does_not_belong_to_the_certificate() {
    let fixture = Fixture::private();
    let home_a = fixture.issuer.issue_for(&[HOME_A]);
    let other = fixture.issuer.issue_for(&[HOME_A]);
    let mismatched = Arc::new(rustls::sign::CertifiedKey::new(
        home_a.chain.clone(),
        other.signing_key(),
    ));

    let client = initiating_config(
        &settings(NOW),
        Arc::clone(&mismatched),
        verifier(&fixture.inventory, HOME_B, NOW),
        OutboundTransport::StartTls,
    );
    assert_eq!(client.err(), Some(TlsConfigError::KeyMismatch));

    let server = receiving_config(
        &settings(NOW),
        mismatched,
        verifier(&fixture.inventory, HOME_B, NOW),
    );
    assert_eq!(server.err(), Some(TlsConfigError::KeyMismatch));
}

/// Two consecutive connections of one stock client to one stock server.
/// The second resumes: the harness can observe resumption when it happens.
#[test]
fn stock_rustls_endpoints_resume_a_session() {
    let fixture = Fixture::private();
    let home_a = fixture.issuer.issue_for(&[HOME_A]);
    let home_b = fixture.issuer.issue_for(&[HOME_B]);
    let client = Arc::new(resuming_client_config(
        &fixture.issuer,
        &home_a,
        TLS_PROTOCOL_VERSIONS,
    ));
    let server = Arc::new(resuming_server_config(&fixture.issuer, &home_b));
    let server_name = verifier(&fixture.inventory, HOME_B, NOW)
        .reference()
        .server_name();
    let connection = || {
        drive(
            ClientConnection::new(Arc::clone(&client), server_name.clone())
                .expect("an initiating connection"),
            ServerConnection::new(Arc::clone(&server)).expect("a receiving connection"),
        )
    };

    let first = connection();
    assert!(first.completed());
    assert_eq!(first.server.handshake_kind(), Some(HandshakeKind::Full));

    let second = connection();
    assert!(second.completed());
    assert_eq!(second.server.handshake_kind(), Some(HandshakeKind::Resumed));
}

/// A receiving configuration serves one connection. A client that would
/// resume gets no ticket and no stored session from it, so a second
/// connection on the same configuration runs a full handshake, presents its
/// certificate again and is refused by the single-use verifier instead of
/// being let in without verification.
fn receiving_configuration_never_resumes(versions: &'static [&'static SupportedProtocolVersion]) {
    let fixture = Fixture::private();
    let home_a = fixture.issuer.issue_for(&[HOME_A]);
    let home_b = fixture.issuer.issue_for(&[HOME_B]);
    let client = Arc::new(resuming_client_config(&fixture.issuer, &home_a, versions));
    let receiving = verifier(&fixture.inventory, HOME_A, NOW);
    let server = Arc::new(
        receiving_config(
            &settings(NOW),
            home_b.certified_key(),
            Arc::clone(&receiving),
        )
        .expect("a receiving configuration"),
    );
    let server_name = verifier(&fixture.inventory, HOME_B, NOW)
        .reference()
        .server_name();
    let connection = || {
        drive(
            ClientConnection::new(Arc::clone(&client), server_name.clone())
                .expect("an initiating connection"),
            ServerConnection::new(Arc::clone(&server)).expect("a receiving connection"),
        )
    };

    let first = connection();
    assert!(first.completed());
    assert_eq!(first.server.handshake_kind(), Some(HandshakeKind::Full));
    let evidence = receiving
        .established(&first.server)
        .expect("the first connection's evidence");
    assert_eq!(evidence.domain(), &domain(HOME_A));

    let second = connection();
    assert!(!second.completed());
    assert_ne!(second.server.handshake_kind(), Some(HandshakeKind::Resumed));
    assert_eq!(second.server_error, Some(reused()));
    assert!(receiving.established(&second.server).is_none());
    assert_eq!(receiving.verification(), Verification::Verified(evidence));
}

/// The error a verifier ends a second connection's handshake with.
fn reused() -> rustls::Error {
    rustls::Error::InvalidCertificate(CertificateError::ApplicationVerificationFailure)
}

#[test]
fn a_receiving_configuration_never_resumes_a_tls13_session() {
    receiving_configuration_never_resumes(TLS_PROTOCOL_VERSIONS);
}

#[test]
fn a_receiving_configuration_never_resumes_a_tls12_session() {
    receiving_configuration_never_resumes(TLS12_ONLY);
}

/// The initiating counterpart: a server that stores sessions and issues
/// tickets cannot make an initiating configuration resume.
#[test]
fn an_initiating_configuration_never_resumes_a_session() {
    let fixture = Fixture::private();
    let home_a = fixture.issuer.issue_for(&[HOME_A]);
    let home_b = fixture.issuer.issue_for(&[HOME_B]);
    let initiating = verifier(&fixture.inventory, HOME_B, NOW);
    let client = Arc::new(
        initiating_config(
            &settings(NOW),
            home_a.certified_key(),
            Arc::clone(&initiating),
            OutboundTransport::StartTls,
        )
        .expect("an initiating configuration"),
    );
    let server = Arc::new(resuming_server_config(&fixture.issuer, &home_b));
    let connection = || {
        let mut connection =
            ClientConnection::new(Arc::clone(&client), initiating.reference().server_name())
                .expect("an initiating connection");
        assert!(connection.early_data().is_none());
        drive(
            connection,
            ServerConnection::new(Arc::clone(&server)).expect("a receiving connection"),
        )
    };

    let first = connection();
    assert!(first.completed());
    assert_eq!(first.client.handshake_kind(), Some(HandshakeKind::Full));
    let evidence = initiating
        .established(&first.client)
        .expect("the first connection's evidence");
    assert_eq!(evidence.domain(), &domain(HOME_B));

    let second = connection();
    assert!(!second.completed());
    assert_ne!(second.client.handshake_kind(), Some(HandshakeKind::Resumed));
    assert_eq!(second.client_error, Some(reused()));
    assert!(initiating.established(&second.client).is_none());
    assert_eq!(initiating.verification(), Verification::Verified(evidence));
}

/// Every connection gets its own configuration and verifier, so a second
/// stream between the same domains verifies the certificate chain, the CRLs
/// and the name again.
#[test]
fn every_stream_verifies_the_peer_again() {
    let fixture = Fixture::private();
    let home_a = fixture.issuer.issue_for(&[HOME_A]);
    let home_b = fixture.issuer.issue_for(&[HOME_B]);

    for _ in 0..2 {
        let stream = attempt(&fixture.inventory, (HOME_A, &home_a), (HOME_B, &home_b));

        assert!(stream.handshake.completed());
        assert_eq!(
            stream.handshake.client.handshake_kind(),
            Some(HandshakeKind::Full)
        );
        assert_eq!(
            stream.handshake.server.handshake_kind(),
            Some(HandshakeKind::Full)
        );
        assert_eq!(
            stream
                .receiver_established()
                .expect("the initiating connection's evidence")
                .domain(),
            &domain(HOME_B)
        );
        assert_eq!(
            stream
                .initiator_established()
                .expect("the receiving connection's evidence")
                .domain(),
            &domain(HOME_A)
        );
    }
}
