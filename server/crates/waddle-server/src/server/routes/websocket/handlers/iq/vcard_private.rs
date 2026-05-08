use super::*;

pub(super) async fn handle_vcard_iq(
    iq: &xmpp_parsers::iq::Iq,
    state: &WebSocketState,
    sender_jid: Option<&FullJid>,
    response_from: Option<&str>,
    response_to: Option<&str>,
) -> Vec<String> {
    let Some(sender_jid) = sender_jid else {
        return vec![build_iq_error_xml_typed(
            &iq.id,
            response_from,
            response_to,
            not_authorized_iq_error("Authentication required."),
        )];
    };

    let db = match global_database(state).await {
        Ok(db) => Arc::new(db),
        Err(error) => {
            warn!(error = %error, "Failed to access database for vCard IQ");
            return vec![build_iq_error_xml_typed(
                &iq.id,
                response_from,
                response_to,
                internal_server_error_iq_error("Internal server error."),
            )];
        }
    };
    let store = VCardStore::new(Arc::clone(&db));

    if waddle_xmpp::xep::xep0054::is_vcard_get(iq) {
        let target_jid = iq
            .to
            .as_ref()
            .map(|jid| jid.to_bare())
            .unwrap_or_else(|| sender_jid.to_bare());
        let response = match store.get(&target_jid).await {
            Ok(Some(vcard)) => xmpp_parsers::iq::Iq {
                from: iq.to.clone(),
                to: iq.from.clone(),
                id: iq.id.clone(),
                payload: xmpp_parsers::iq::IqType::Result(Some(vcard)),
            },
            Ok(None) => {
                return vec![build_iq_error_xml_typed(
                    &iq.id,
                    response_from,
                    response_to,
                    item_not_found_iq_error("Requested item not found."),
                )];
            }
            Err(error) => {
                warn!(target = %target_jid, error = %error, "Failed to load vCard");
                return vec![build_iq_error_xml_typed(
                    &iq.id,
                    response_from,
                    response_to,
                    internal_server_error_iq_error("Internal server error."),
                )];
            }
        };
        return vec![iq_to_xml(response)];
    }

    if let Some(to) = &iq.to {
        if to.to_bare() != sender_jid.to_bare() {
            return vec![build_iq_error_xml_typed(
                &iq.id,
                response_from,
                response_to,
                forbidden_iq_error("Operation not permitted."),
            )];
        }
    }

    let xmpp_parsers::iq::IqType::Set(vcard) = &iq.payload else {
        return vec![build_iq_error_xml_typed(
            &iq.id,
            response_from,
            response_to,
            bad_request_iq_error("Malformed IQ payload."),
        )];
    };

    if let Err(error) = store.set(&sender_jid.to_bare(), vcard).await {
        warn!(jid = %sender_jid, error = %error, "Failed to store vCard");
        return vec![build_iq_error_xml_typed(
            &iq.id,
            response_from,
            response_to,
            internal_server_error_iq_error("Internal server error."),
        )];
    }

    vec![iq_to_xml(waddle_xmpp::xep::xep0054::build_vcard_success(
        iq,
    ))]
}

pub(super) async fn handle_private_storage_iq(
    iq: &xmpp_parsers::iq::Iq,
    state: &WebSocketState,
    sender_jid: Option<&FullJid>,
    response_from: Option<&str>,
    response_to: Option<&str>,
) -> Vec<String> {
    let Some(sender_jid) = sender_jid else {
        return vec![build_iq_error_xml_typed(
            &iq.id,
            response_from,
            response_to,
            not_authorized_iq_error("Authentication required."),
        )];
    };
    let bare_jid = sender_jid.to_bare();

    if let Some(key) = waddle_xmpp::xep::xep0049::parse_private_storage_get(iq) {
        if waddle_xmpp::xep::xep0048::is_legacy_bookmarks_namespace(&key.namespace) {
            let stored_items = state
                .deps
                .protocol
                .pubsub_storage
                .get_items(&bare_jid, waddle_xmpp::xep::xep0402::PEP_NODE, None, &[])
                .await
                .unwrap_or_default();
            let native_bookmarks: Vec<waddle_xmpp::xep::xep0402::Bookmark> = stored_items
                .iter()
                .filter_map(|item| {
                    let xml = item.payload_xml.as_ref()?;
                    let elem: Element = xml.parse().ok()?;
                    waddle_xmpp::xep::xep0402::parse_bookmark(&item.id, &elem).ok()
                })
                .collect();
            let legacy: Vec<waddle_xmpp::xep::xep0048::LegacyBookmark> = native_bookmarks
                .iter()
                .map(waddle_xmpp::xep::xep0048::from_native_bookmark)
                .collect();
            let bookmarks = waddle_xmpp::xep::xep0048::build_legacy_bookmarks_element(&legacy);
            let response = waddle_xmpp::xep::xep0049::build_private_storage_result(
                iq,
                Some(&String::from(&bookmarks)),
                &key,
            );
            return vec![iq_to_xml(response)];
        }

        let row = match state
            .deps
            .app_state
            .db_pool
            .global_actor()
            .clone()
            .ask(DbQueryOne {
                sql: "SELECT xml_content FROM private_xml_storage WHERE jid = ? AND namespace = ?"
                    .to_string(),
                params: vec![
                    Value::from(bare_jid.to_string()),
                    Value::from(key.namespace.clone()),
                ],
            })
            .await
        {
            Ok(row) => row,
            Err(error) => {
                warn!(jid = %bare_jid, namespace = %key.namespace, error = %error, "Failed to load private XML");
                return vec![build_iq_error_xml_typed(
                    &iq.id,
                    response_from,
                    response_to,
                    internal_server_error_iq_error("Internal server error."),
                )];
            }
        };
        let stored = match row {
            Some(values) => match row_value(&values, 0).and_then(|value| value.as_string()) {
                Ok(value) => Some(value),
                Err(error) => {
                    warn!(jid = %bare_jid, namespace = %key.namespace, error = %error, "Failed to decode private XML row");
                    return vec![build_iq_error_xml_typed(
                        &iq.id,
                        response_from,
                        response_to,
                        internal_server_error_iq_error("Internal server error."),
                    )];
                }
            },
            None => None,
        };
        let response =
            waddle_xmpp::xep::xep0049::build_private_storage_result(iq, stored.as_deref(), &key);
        return vec![iq_to_xml(response)];
    }

    if let Some((key, xml_content)) = waddle_xmpp::xep::xep0049::parse_private_storage_set(iq) {
        if waddle_xmpp::xep::xep0048::is_legacy_bookmarks_namespace(&key.namespace) {
            if let Ok(elem) = xml_content.parse::<Element>() {
                for legacy in waddle_xmpp::xep::xep0048::parse_legacy_bookmarks(&elem) {
                    if let Some(native) = waddle_xmpp::xep::xep0048::to_native_bookmark(&legacy) {
                        let bookmark_elem =
                            waddle_xmpp::xep::xep0402::build_bookmark_element(&native);
                        let item =
                            PubSubItem::new(Some(native.jid.to_string()), Some(bookmark_elem));
                        let _ = state
                            .deps
                            .protocol
                            .pubsub_storage
                            .publish_item(
                                &bare_jid,
                                waddle_xmpp::xep::xep0402::PEP_NODE,
                                &item,
                                Some(&bare_jid),
                                true,
                            )
                            .await;
                    }
                }
            }
            return vec![iq_to_xml(
                waddle_xmpp::xep::xep0049::build_private_storage_success(iq),
            )];
        }

        if let Err(error) = state
            .deps
            .app_state
            .db_pool
            .global_actor()
            .clone()
            .ask(DbExecute {
                sql: "INSERT OR REPLACE INTO private_xml_storage (jid, namespace, xml_content, updated_at) VALUES (?, ?, ?, datetime('now'))".to_string(),
                params: vec![
                    Value::from(bare_jid.to_string()),
                    Value::from(key.namespace),
                    Value::from(xml_content),
                ],
            })
            .await
        {
            warn!(jid = %bare_jid, error = %error, "Failed to store private XML");
            return vec![build_iq_error_xml_typed(
                            &iq.id,
                            response_from,
                            response_to,
                            internal_server_error_iq_error("Internal server error."),
                        )];
        }
        return vec![iq_to_xml(
            waddle_xmpp::xep::xep0049::build_private_storage_success(iq),
        )];
    }

    vec![build_iq_error_xml_typed(
        &iq.id,
        response_from,
        response_to,
        bad_request_iq_error("Malformed IQ payload."),
    )]
}
