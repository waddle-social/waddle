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
            let mut builder = MamIqBuilder::new(&iq_id, &query_id, max)
                .to_jid(&room_jid)
                .thread_id(&thread_id);
            if let Some(before) = before_id.as_deref() {
                builder = builder.before(before);
            }
            let iq = builder.build();
            let page = send_mam_query_command(inner, iq, query_id).await?;
            to_js_value(&mam_page_to_js(page))
        })
    }

    pub fn search_room_history(&self, room_jid: String, query: String, max: u32) -> Promise {
        let inner = self.inner.clone();
        future_to_promise(async move {
            let query_id = uuid::Uuid::new_v4().to_string();
            let iq_id = uuid::Uuid::new_v4().to_string();
            let iq = MamIqBuilder::new(&iq_id, &query_id, max)
                .before("")
                .to_jid(&room_jid)
                .fulltext(&query)
                .build();
            let page = send_mam_query_command(inner, iq, query_id).await?;
            to_js_value(&mam_page_to_js(page))
        })
    }

    pub fn search_dm_history(&self, peer_jid: String, query: String, max: u32) -> Promise {
        let inner = self.inner.clone();
        future_to_promise(async move {
            let query_id = uuid::Uuid::new_v4().to_string();
            let iq_id = uuid::Uuid::new_v4().to_string();
            let iq = MamIqBuilder::new(&iq_id, &query_id, max)
                .before("")
                .with_jid(&peer_jid)
                .fulltext(&query)
                .build();
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
            let mut builder = MamIqBuilder::new(&iq_id, &query_id, max).to_jid(&room_jid);
            if let Some(b) = before.as_deref() {
                builder = builder.before(b);
            }
            let iq = builder.build();
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
            let mut builder = MamIqBuilder::new(&iq_id, &query_id, max).with_jid(&peer_jid);
            if let Some(b) = before.as_deref() {
                builder = builder.before(b);
            }
            let iq = builder.build();
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

    /// Waddle-specific MAM stanza-id filter — fetch a batch of messages from
    /// a room MAM archive by XEP-0359 stanza-id. Uses the custom data-form
    /// var `{urn:waddle:mam-stanza-id:0}stanza-id` per XEP-0313 §4.2 +
    /// XEP-0068 (not the `urn:xmpp:sid:0` namespace, which is XEP-0359
    /// wire protocol only). Used by the pinned-panel rich-preview render
    /// path to materialize `TimelineMessage`s for pinned entries that
    /// are not in the loaded timeline window.
    pub fn fetch_room_messages_by_stanza_ids(
        &self,
        room_jid: String,
        stanza_ids: Vec<String>,
    ) -> Promise {
        let inner = self.inner.clone();
        future_to_promise(async move {
            if stanza_ids.is_empty() {
                return to_js_value(&mam_page_to_js(waddle_xmpp_client::MamPage {
                    messages: vec![],
                    rsm: waddle_xmpp_client::RsmPageInfo::default(),
                    query_id: String::new(),
                    is_complete: true,
                }));
            }
            let refs: Vec<&str> = stanza_ids.iter().map(String::as_str).collect();
            let query_id = uuid::Uuid::new_v4().to_string();
            let iq_id = uuid::Uuid::new_v4().to_string();
            let iq = MamIqBuilder::new(&iq_id, &query_id, stanza_ids.len() as u32)
                .before("")
                .to_jid(&room_jid)
                .stanza_ids(&refs)
                .build();
            let page = send_mam_query_command(inner, iq, query_id).await?;
            to_js_value(&mam_page_to_js(page))
        })
    }
}
