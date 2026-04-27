use waddle_xmpp::xep::xep0320::{
    build_dtls_fingerprint, DtlsFingerprint, DtlsFingerprintHash, DtlsSetup, NS_JINGLE_DTLS,
};

#[test]
fn xep0320_builds_dtls_fingerprint() {
    let fingerprint = DtlsFingerprint::new("AA:BB").expect("fingerprint");
    let elem = build_dtls_fingerprint(
        DtlsFingerprintHash::Sha256,
        DtlsSetup::Actpass,
        &fingerprint,
    );
    assert_eq!(elem.name(), "fingerprint");
    assert_eq!(elem.ns(), NS_JINGLE_DTLS);
    assert_eq!(elem.attr("hash"), Some("sha-256"));
    assert_eq!(elem.attr("setup"), Some("actpass"));
    assert_eq!(elem.text(), "AA:BB");
}
