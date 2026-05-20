use super::*;

use crate::push_service::{PushDevicePlatform, PushDeviceRegistration, WADDLE_PUSH_SERVICE_NS};

pub(super) async fn handle_push_service_iq(
    ctx: IqHandlerContext<'_>,
    state: &WebSocketState,
    phase: &ConnectionPhase,
) -> Vec<String> {
    if ctx.payload_ns != WADDLE_PUSH_SERVICE_NS || ctx.target_to != Some(ctx.push_domain) {
        return Vec::new();
    }
    if !phase.is_ready() {
        return vec![build_iq_error_xml_typed(
            ctx.id,
            ctx.response_from,
            ctx.response_to,
            not_authorized_iq_error("Authentication required."),
        )];
    }
    let Some(owner_bare_jid) = phase.bound_jid().map(|jid| jid.to_bare()) else {
        return vec![build_iq_error_xml_typed(
            ctx.id,
            ctx.response_from,
            ctx.response_to,
            not_authorized_iq_error("Authentication required."),
        )];
    };

    let xmpp_parsers::iq::Iq::Set { payload, .. } = ctx.iq else {
        return vec![build_iq_error_xml_typed(
            ctx.id,
            ctx.response_from,
            ctx.response_to,
            bad_request_iq_error("Push Service requests must be IQ set."),
        )];
    };

    match payload.name() {
        "ensure-node" => ensure_node(ctx, state, &owner_bare_jid, payload).await,
        "register-device" => register_device(ctx, state, &owner_bare_jid, payload).await,
        "disable-device" => disable_device(ctx, state, &owner_bare_jid, payload).await,
        _ => vec![build_iq_error_xml_typed(
            ctx.id,
            ctx.response_from,
            ctx.response_to,
            feature_not_implemented_iq_error("Push Service request not implemented."),
        )],
    }
}

async fn ensure_node(
    ctx: IqHandlerContext<'_>,
    state: &WebSocketState,
    owner_bare_jid: &BareJid,
    payload: &Element,
) -> Vec<String> {
    let Some(app_id) = payload.attr("app-id").filter(|value| !value.is_empty()) else {
        return vec![build_iq_error_xml_typed(
            ctx.id,
            ctx.response_from,
            ctx.response_to,
            bad_request_iq_error("Push Service app-id is required."),
        )];
    };

    match state
        .deps
        .protocol
        .push_service
        .ensure_node(owner_bare_jid, app_id)
        .await
    {
        Ok(node) => {
            let response = Element::builder("node", WADDLE_PUSH_SERVICE_NS)
                .attr(minidom::rxml::xml_ncname!("id").to_owned(), node.node())
                .attr(
                    minidom::rxml::xml_ncname!("jid").to_owned(),
                    ctx.push_domain,
                )
                .attr(
                    minidom::rxml::xml_ncname!("app-id").to_owned(),
                    node.app_id(),
                )
                .build();
            vec![iq_to_xml(build_result_with_payload(ctx.iq, response))]
        }
        Err(error) => push_service_error(ctx, error),
    }
}

async fn register_device(
    ctx: IqHandlerContext<'_>,
    state: &WebSocketState,
    owner_bare_jid: &BareJid,
    payload: &Element,
) -> Vec<String> {
    let Some(node) = payload.attr("node").filter(|value| !value.is_empty()) else {
        return vec![build_iq_error_xml_typed(
            ctx.id,
            ctx.response_from,
            ctx.response_to,
            bad_request_iq_error("Push Service node is required."),
        )];
    };
    let Some(device_id) = payload.attr("device-id").filter(|value| !value.is_empty()) else {
        return vec![build_iq_error_xml_typed(
            ctx.id,
            ctx.response_from,
            ctx.response_to,
            bad_request_iq_error("Push Service device-id is required."),
        )];
    };
    let Some(platform_raw) = payload.attr("platform").filter(|value| !value.is_empty()) else {
        return vec![build_iq_error_xml_typed(
            ctx.id,
            ctx.response_from,
            ctx.response_to,
            bad_request_iq_error("Push Service platform is required."),
        )];
    };
    let platform = match PushDevicePlatform::parse(platform_raw) {
        Ok(platform) => platform,
        Err(error) => {
            return vec![build_iq_error_xml_typed(
                ctx.id,
                ctx.response_from,
                ctx.response_to,
                bad_request_iq_error(&error.to_string()),
            )];
        }
    };
    let Some(environment) = payload
        .attr("environment")
        .filter(|value| !value.is_empty())
    else {
        return vec![build_iq_error_xml_typed(
            ctx.id,
            ctx.response_from,
            ctx.response_to,
            bad_request_iq_error("Push Service environment is required."),
        )];
    };

    let registration = PushDeviceRegistration::new(device_id, node, platform, environment)
        .with_provider_endpoint(child_text(payload, "provider-endpoint"))
        .with_provider_token(child_text(payload, "provider-token"))
        .with_provider_key_material(child_text(payload, "provider-key-material"));

    match state
        .deps
        .protocol
        .push_service
        .upsert_device(owner_bare_jid, registration)
        .await
    {
        Ok(device) => {
            let response = Element::builder("device", WADDLE_PUSH_SERVICE_NS)
                .attr(
                    minidom::rxml::xml_ncname!("id").to_owned(),
                    device.device_id(),
                )
                .attr(minidom::rxml::xml_ncname!("node").to_owned(), device.node())
                .attr(minidom::rxml::xml_ncname!("status").to_owned(), "active")
                .build();
            vec![iq_to_xml(build_result_with_payload(ctx.iq, response))]
        }
        Err(error) => push_service_error(ctx, error),
    }
}

async fn disable_device(
    ctx: IqHandlerContext<'_>,
    state: &WebSocketState,
    owner_bare_jid: &BareJid,
    payload: &Element,
) -> Vec<String> {
    let Some(node) = payload.attr("node").filter(|value| !value.is_empty()) else {
        return vec![build_iq_error_xml_typed(
            ctx.id,
            ctx.response_from,
            ctx.response_to,
            bad_request_iq_error("Push Service node is required."),
        )];
    };
    let Some(device_id) = payload.attr("device-id").filter(|value| !value.is_empty()) else {
        return vec![build_iq_error_xml_typed(
            ctx.id,
            ctx.response_from,
            ctx.response_to,
            bad_request_iq_error("Push Service device-id is required."),
        )];
    };

    match state
        .deps
        .protocol
        .push_service
        .disable_device_for_owner(owner_bare_jid, node, device_id, None)
        .await
    {
        Ok(true) => {
            let response = Element::builder("device", WADDLE_PUSH_SERVICE_NS)
                .attr(minidom::rxml::xml_ncname!("id").to_owned(), device_id)
                .attr(minidom::rxml::xml_ncname!("node").to_owned(), node)
                .attr(minidom::rxml::xml_ncname!("status").to_owned(), "disabled")
                .build();
            vec![iq_to_xml(build_result_with_payload(ctx.iq, response))]
        }
        Ok(false) => vec![build_iq_error_xml_typed(
            ctx.id,
            ctx.response_from,
            ctx.response_to,
            item_not_found_iq_error("Requested Push Service device not found."),
        )],
        Err(error) => push_service_error(ctx, error),
    }
}

fn child_text(parent: &Element, name: &str) -> Option<String> {
    parent
        .get_child(name, WADDLE_PUSH_SERVICE_NS)
        .map(Element::text)
        .filter(|value| !value.is_empty())
}

fn build_result_with_payload(
    request_iq: &xmpp_parsers::iq::Iq,
    payload: Element,
) -> xmpp_parsers::iq::Iq {
    xmpp_parsers::iq::Iq::Result {
        from: request_iq.to().cloned(),
        to: request_iq.from().cloned(),
        id: request_iq.id().to_string(),
        payload: Some(payload),
    }
}

fn push_service_error(ctx: IqHandlerContext<'_>, error: XmppError) -> Vec<String> {
    warn!(error = %error, "Push Service IQ failed");
    let stanza_error = push_service_stanza_error(error);
    vec![build_iq_error_xml_typed(
        ctx.id,
        ctx.response_from,
        ctx.response_to,
        stanza_error,
    )]
}
