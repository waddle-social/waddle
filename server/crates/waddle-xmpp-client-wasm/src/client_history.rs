use super::*;

#[wasm_bindgen]
impl WaddleClient {
    pub fn fetch_room_history_by_thread(
        &self,
        room_jid: String,
        thread_id: String,
        max: u32,
        before_id: Option<String>,
    ) -> Promise {
        let inner = self.inner.clone();
        future_to_promise(async move {
            let query_id = uuid::Uuid::new_v4().to_string();
            let iq_id = uuid::Uuid::new_v4().to_string();
            let iq = build_mam_iq_extended(
                &iq_id,
                &query_id,
                max,
                before_id.as_deref(),
                None,
                None,
                Some(&room_jid),
                Some(&thread_id),
                None,
                None,
                None,
            );
            let page = send_mam_query_command(inner, iq, query_id).await?;
            to_js_value(&mam_page_to_js(page))
        })
    }

    pub fn search_room_history(&self, room_jid: String, query: String, max: u32) -> Promise {
        let inner = self.inner.clone();
        future_to_promise(async move {
            let query_id = uuid::Uuid::new_v4().to_string();
            let iq_id = uuid::Uuid::new_v4().to_string();
            let iq = build_mam_iq_extended(
                &iq_id,
                &query_id,
                max,
                Some(""),
                None,
                None,
                Some(&room_jid),
                None,
                Some(&query),
                None,
                None,
            );
            let page = send_mam_query_command(inner, iq, query_id).await?;
            to_js_value(&mam_page_to_js(page))
        })
    }

    pub fn search_dm_history(&self, peer_jid: String, query: String, max: u32) -> Promise {
        let inner = self.inner.clone();
        future_to_promise(async move {
            let query_id = uuid::Uuid::new_v4().to_string();
            let iq_id = uuid::Uuid::new_v4().to_string();
            let iq = build_mam_iq_extended(
                &iq_id,
                &query_id,
                max,
                Some(""),
                None,
                Some(&peer_jid),
                None,
                None,
                Some(&query),
                None,
                None,
            );
            let page = send_mam_query_command(inner, iq, query_id).await?;
            to_js_value(&mam_page_to_js(page))
        })
    }

    pub fn fetch_room_history_page(
        &self,
        room_jid: String,
        max: u32,
        page_param: JsValue,
    ) -> Promise {
        let inner = self.inner.clone();
        future_to_promise(async move {
            let page: WaddleMamPageParam = serde_wasm_bindgen::from_value(page_param)
                .map_err(|err| js_error(err.to_string()))?;
            let before = match page.kind.as_str() {
                "latest" => Some(String::new()),
                "before" => page.before,
                _ => return Err(js_error("invalid page param type")),
            };
            let query_id = uuid::Uuid::new_v4().to_string();
            let iq_id = uuid::Uuid::new_v4().to_string();
            let iq = build_mam_iq_extended(
                &iq_id,
                &query_id,
                max,
                before.as_deref(),
                None,
                None,
                Some(&room_jid),
                None,
                None,
                None,
                None,
            );
            let result = send_mam_query_command(inner, iq, query_id).await?;
            to_js_value(&mam_page_to_js(result))
        })
    }

    pub fn fetch_dm_history_page(
        &self,
        peer_jid: String,
        max: u32,
        page_param: JsValue,
    ) -> Promise {
        let inner = self.inner.clone();
        future_to_promise(async move {
            let page: WaddleMamPageParam = serde_wasm_bindgen::from_value(page_param)
                .map_err(|err| js_error(err.to_string()))?;
            let before = match page.kind.as_str() {
                "latest" => Some(String::new()),
                "before" => page.before,
                _ => return Err(js_error("invalid page param type")),
            };
            let query_id = uuid::Uuid::new_v4().to_string();
            let iq_id = uuid::Uuid::new_v4().to_string();
            let iq = build_mam_iq_extended(
                &iq_id,
                &query_id,
                max,
                before.as_deref(),
                None,
                Some(&peer_jid),
                None,
                None,
                None,
                None,
                None,
            );
            let result = send_mam_query_command(inner, iq, query_id).await?;
            to_js_value(&mam_page_to_js(result))
        })
    }

    pub fn search_users(&self, query: String) -> Promise {
        let inner = self.inner.clone();
        future_to_promise(async move {
            let _ = NS_USER_SEARCH;
            let domain = {
                let stored = inner.borrow().config.clone();
                jid_domain(&stored.jid)
            };
            let iq = build_user_search_iq(
                &domain,
                &UserSearchQuery {
                    nick: Some(query),
                    ..UserSearchQuery::default()
                },
            );
            let result = send_iq_command(inner, iq).await?;
            let users = parse_user_search_result(&result)
                .map(|results| {
                    results
                        .items
                        .into_iter()
                        .filter_map(|item| {
                            item.nick.as_ref()?;
                            Some(WaddleUserSearchResult {
                                jid: item.jid,
                                username: item.nick.unwrap_or_default(),
                                display_name: item.first.or(item.last),
                            })
                        })
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            to_js_value(&users)
        })
    }

    pub fn fetch_room_history(
        &self,
        room_jid: String,
        max: u32,
        before_id: Option<String>,
    ) -> Promise {
        let inner = self.inner.clone();
        future_to_promise(async move {
            let query_id = uuid::Uuid::new_v4().to_string();
            let iq_id = uuid::Uuid::new_v4().to_string();
            let iq = build_mam_iq(
                &iq_id,
                &query_id,
                max,
                before_id.as_deref(),
                None,
                Some(&room_jid),
            );
            let page = send_mam_query_command(inner, iq, query_id).await?;
            to_js_value(&mam_page_to_js(page))
        })
    }

    pub fn fetch_dm_history(
        &self,
        peer_jid: String,
        max: u32,
        before_id: Option<String>,
    ) -> Promise {
        let inner = self.inner.clone();
        future_to_promise(async move {
            let query_id = uuid::Uuid::new_v4().to_string();
            let iq_id = uuid::Uuid::new_v4().to_string();
            let iq = build_mam_iq(
                &iq_id,
                &query_id,
                max,
                before_id.as_deref(),
                Some(&peer_jid),
                None,
            );
            let page = send_mam_query_command(inner, iq, query_id).await?;
            to_js_value(&mam_page_to_js(page))
        })
    }
}
