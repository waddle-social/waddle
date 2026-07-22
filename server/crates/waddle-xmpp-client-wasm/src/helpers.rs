use super::*;

pub(crate) fn build_client_config(config: &StoredConfig) -> Result<ClientConfig, JsValue> {
    let jid: BareJid = config
        .jid
        .parse()
        .map_err(|err| js_error(format!("invalid JID: {err}")))?;
    let domain: BareJid = jid
        .domain()
        .to_string()
        .parse()
        .map_err(|err| js_error(format!("invalid domain: {err}")))?;
    let transport = WebSocketConfig::new(
        config
            .server_url
            .parse()
            .map_err(|err| js_error(format!("invalid server URL: {err}")))?,
    )
    .map_err(|err| js_error(err.to_string()))?;
    let resource =
        ClientResource::new(config.resource.clone()).map_err(|err| js_error(err.to_string()))?;
    let auth = OAuthBearerConfig::new(jid, resource, AccessToken::new(config.access_token.clone()))
        .map_err(|err| js_error(err.to_string()))?;
    let mut client_config = ClientConfig::new(ConnectionConfig::new(domain), transport, auth)
        .map_err(|err| js_error(err.to_string()))?;
    client_config.session.stream_management.resume_state = config.resume_state.clone();
    Ok(client_config)
}

pub(crate) fn parse_raw_iq(xml: &str) -> Result<Element, JsValue> {
    // minidom 0.16 requires every element to carry a namespace at parse time.
    // Callers omit xmlns='jabber:client' on the <iq> element (it is implied by
    // the stream context).  Inject the namespace into the raw string before
    // parsing so minidom does not return MissingNamespace.
    //
    // We inspect only the opening <iq …> tag (up to the first '>') to avoid
    // matching xmlns= attributes on child elements.
    let normalized: std::borrow::Cow<str> = {
        let tag_end = xml.find('>').unwrap_or(xml.len());
        let iq_tag = &xml[..tag_end];
        if iq_tag.contains("xmlns=") {
            std::borrow::Cow::Borrowed(xml)
        } else {
            std::borrow::Cow::Owned(xml.replacen("<iq", "<iq xmlns='jabber:client'", 1))
        }
    };
    let stanza = normalized
        .parse::<Element>()
        .map_err(|err| js_error(format!("invalid IQ XML: {err}")))?;
    if stanza.name() != "iq" {
        return Err(js_error("raw stanza must be an <iq/> element"));
    }
    Ok(stanza)
}

pub(crate) fn element_to_xml_string(element: &Element) -> Result<String, JsValue> {
    let mut buffer = Vec::new();
    element
        .write_to(&mut buffer)
        .map_err(|err| js_error(format!("failed to serialize IQ XML: {err}")))?;
    String::from_utf8(buffer).map_err(|err| js_error(format!("IQ XML was not UTF-8: {err}")))
}

pub(crate) fn bare_jid(jid: &str) -> String {
    jid.split('/').next().unwrap_or(jid).to_string()
}

/// Parse a JS-supplied pubsub service address (e.g. the
/// `community.<domain>` component) into the typed bare JID the
/// `waddle_xmpp_client::pubsub` builders require.
pub(crate) fn service_bare_jid(jid: &str) -> Result<jid::BareJid, JsValue> {
    jid.parse()
        .map_err(|err| js_error(format!("invalid pubsub service JID: {err}")))
}

pub(crate) fn jid_domain(jid: &str) -> String {
    let bare = bare_jid(jid);
    bare.split('@')
        .next_back()
        .unwrap_or(bare.as_str())
        .to_string()
}

pub(crate) fn parse_muc_affiliation(value: &str) -> Result<MucAffiliation, JsValue> {
    match value {
        "owner" => Ok(MucAffiliation::Owner),
        "admin" => Ok(MucAffiliation::Admin),
        "member" => Ok(MucAffiliation::Member),
        "outcast" => Ok(MucAffiliation::Outcast),
        "none" => Ok(MucAffiliation::None),
        _ => Err(js_error(format!("invalid affiliation: {value}"))),
    }
}

pub(crate) fn message_delivery_stanza_id(element: &Element) -> Option<StanzaId> {
    if element.name() != "message" {
        return None;
    }

    element.attr("id").and_then(|id| StanzaId::new(id).ok())
}

pub(crate) fn to_js_value<T: Serialize + ?Sized>(value: &T) -> Result<JsValue, JsValue> {
    serde_wasm_bindgen::to_value(value).map_err(|err| js_error(err.to_string()))
}

pub(crate) fn js_error(message: impl ToString) -> JsValue {
    JsValue::from_str(&message.to_string())
}
