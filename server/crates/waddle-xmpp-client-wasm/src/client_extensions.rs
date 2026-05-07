use super::*;

#[wasm_bindgen]
impl WaddleClient {
    pub fn discover_extension_routes(&self, user_jid: Option<String>) -> Promise {
        let inner = self.inner.clone();
        future_to_promise(async move {
            let requested_jid = user_jid.unwrap_or_else(|| inner.borrow().config.jid.clone());
            let jid: Jid = requested_jid
                .parse()
                .map_err(|err| js_error(format!("invalid JID: {err}")))?;
            let domain = jid_domain(&jid.to_string());
            let service_jid = discover_extension_command_service(inner.clone(), &domain).await;
            let mut routes = Vec::new();

            let items_result =
                send_iq_command(inner.clone(), build_disco_items_iq(&service_jid, None)).await?;
            let items = discovery::parse_disco_items_result(&items_result)
                .ok_or_else(|| js_error("could not parse extension route disco#items result"))?;
            for item in items {
                let Some(node) = item.node.as_deref() else {
                    continue;
                };
                let item_service_jid = item.jid.as_str();
                let info_result = send_iq_command(
                    inner.clone(),
                    build_disco_info_iq(item_service_jid, Some(node)),
                )
                .await?;
                if let Some(info) =
                    discovery::parse_disco_info_result(&info_result, item_service_jid)
                {
                    routes.extend(parse_extension_routes_from_info(&info, item_service_jid));
                }
            }

            if routes.is_empty() {
                let info_result =
                    send_iq_command(inner.clone(), build_disco_info_iq(&service_jid, None)).await?;
                if let Some(info) = discovery::parse_disco_info_result(&info_result, &service_jid) {
                    routes.extend(parse_extension_routes_from_info(&info, &service_jid));
                }
            }

            to_js_value(&routes)
        })
    }

    pub fn fetch_extension_route_items(&self, route: JsValue, room_jid: String) -> Promise {
        let inner = self.inner.clone();
        future_to_promise(async move {
            let route: WaddleExtensionRoute = serde_wasm_bindgen::from_value(route)
                .map_err(|err| js_error(format!("invalid extension route: {err}")))?;
            let node = route.state_node.replace("{room}", &room_jid);
            let iq = build_extension_route_items_iq(
                &route.service_jid,
                &node,
                Some(EXTENSION_ROUTE_ITEM_LIMIT),
            );
            let result = send_iq_command(inner, iq).await?;
            let items = parse_extension_route_items_result(&result);
            to_js_value(&items)
        })
    }
}
