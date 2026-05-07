use super::*;

#[cfg(test)]
mod extension_disco_tests {
    use super::*;

    #[test]
    fn extension_namespaces_are_advertised_without_provider_gate() {
        let features = extension_namespaces_for_disco(vec![
            "urn:waddle:bot:1".to_string(),
            "urn:example:extension:1".to_string(),
        ]);

        assert_eq!(
            features,
            vec![
                Feature::new("urn:waddle:bot:1"),
                Feature::new("urn:example:extension:1")
            ]
        );
    }
}

#[cfg(test)]
mod vcard_fallback_tests {
    use super::*;
    use crate::db::MigrationRunner;
    use waddle_xmpp::xep::xep0054::{NS_VCARD, VCardPhoto};

    async fn test_db(name: &str) -> Arc<Database> {
        let db = Arc::new(Database::in_memory(name).await.expect("database"));
        MigrationRunner::global()
            .run(&db)
            .await
            .expect("migrations");
        db
    }

    #[tokio::test]
    async fn vcard_get_fallback_returns_photo_extval_without_stored_vcard() {
        let db = test_db("vcard-profile-fallback").await;
        let conn = db.guard().await.expect("connection");
        conn.execute(
            "INSERT INTO users (id, username, xmpp_localpart, avatar_url, created_at, updated_at) VALUES (?, ?, ?, ?, ?, ?)",
            vec![
                Value::from("user-rawkode"),
                Value::from("rawkode"),
                Value::from("rawkode"),
                Value::from("https://cdn.example.com/rawkode.png"),
                Value::from("2026-04-25T00:00:00Z"),
                Value::from("2026-04-25T00:00:00Z"),
            ],
        )
        .await
        .expect("insert user");

        let target_jid: BareJid = "rawkode@example.com".parse().expect("jid");
        let vcard = avatar_vcard_from_user_profile(Arc::clone(&db), &target_jid)
            .await
            .expect("fallback")
            .expect("profile avatar");
        assert!(matches!(
            vcard.photo,
            Some(VCardPhoto::External { ref url }) if url == "https://cdn.example.com/rawkode.png"
        ));

        let stored = VCardStore::new(db)
            .get(&target_jid)
            .await
            .expect("stored lookup");
        assert_eq!(stored, None, "GET fallback must not persist a vCard");
    }

    #[tokio::test]
    async fn vcard_get_fallback_builds_typed_result_not_internal_server_error() {
        let db = test_db("vcard-profile-response").await;
        let conn = db.guard().await.expect("connection");
        conn.execute(
            "INSERT INTO users (id, username, xmpp_localpart, avatar_url, created_at, updated_at) VALUES (?, ?, ?, ?, ?, ?)",
            vec![
                Value::from("user-icepuma"),
                Value::from("icepuma"),
                Value::from("icepuma"),
                Value::from("https://cdn.example.com/icepuma.jpg"),
                Value::from("2026-04-25T00:00:00Z"),
                Value::from("2026-04-25T00:00:00Z"),
            ],
        )
        .await
        .expect("insert user");

        let target_jid: BareJid = "icepuma@example.com".parse().expect("jid");
        let vcard = avatar_vcard_from_user_profile(db, &target_jid)
            .await
            .expect("fallback")
            .expect("profile avatar");
        let iq = xmpp_parsers::iq::Iq {
            from: Some("rawkode@example.com/web".parse::<Jid>().expect("from")),
            to: Some("icepuma@example.com".parse::<Jid>().expect("to")),
            id: "vcard-get".to_string(),
            payload: xmpp_parsers::iq::IqType::Get(Element::builder("vCard", NS_VCARD).build()),
        };
        let response = iq_to_xml(waddle_xmpp::xep::xep0054::build_vcard_response(&iq, &vcard));

        assert!(response.contains("type=\"result\"") || response.contains("type='result'"));
        assert!(response.contains("<EXTVAL>https://cdn.example.com/icepuma.jpg</EXTVAL>"));
        assert!(!response.contains("internal-server-error"));
    }
}
