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
            // An empty `<before/>` requests the most-recent page (XEP-0059
            // §2.5 / XEP-0313 §3.4): the server treats any present `<before>`
            // — including empty — as backward pagination. Omitting it would
            // page forward and return the *oldest* page instead, so default
            // the latest fetch to an empty cursor like the non-thread bindings.
            let iq = MamIqBuilder::new(&iq_id, &query_id, max)
                .to_jid(&room_jid)
                .thread_id(&thread_id)
                .before(before_id.as_deref().unwrap_or(""))
                .build();
            let page = send_mam_query_command(inner, iq, query_id).await?;
            to_js_value(&mam_page_to_js(page))
        })
    }

    /// Fetch a DM thread's archived replies from the account archive,
    /// filtered by `with=peer` and the Waddle MAM thread field. Mirrors
    /// `fetch_room_history_by_thread`, but targets the personal archive
    /// (`to=account` + `with=peer`) instead of a room. A `None` / empty
    /// `before_id` requests the most-recent page; a cursor pages older
    /// replies via RSM (XEP-0059).
    pub fn fetch_dm_history_by_thread(
        &self,
        peer_jid: String,
        thread_id: String,
        max: u32,
        before_id: Option<String>,
    ) -> Promise {
        let inner = self.inner.clone();
        future_to_promise(async move {
            let query_id = uuid::Uuid::new_v4().to_string();
            let iq_id = uuid::Uuid::new_v4().to_string();
            let account_jid = {
                let stored = inner.borrow().config.clone();
                bare_jid(&stored.jid)
            };
            // Empty `<before/>` = most-recent page (see
            // `fetch_room_history_by_thread`); omitting it would return the
            // oldest page and strand the newest replies of a long thread.
            let iq = MamIqBuilder::new(&iq_id, &query_id, max)
                .to_jid(&account_jid)
                .with_jid(&peer_jid)
                .thread_id(&thread_id)
                .before(before_id.as_deref().unwrap_or(""))
                .build();
            let page = send_mam_query_command(inner, iq, query_id).await?;
            to_js_value(&mam_page_to_js(page))
        })
    }

    pub fn search_room_history(&self, room_jid: String, query: String, max: u32) -> Promise {
        let inner = self.inner.clone();
        future_to_promise(async move {
            let query_id = uuid::Uuid::new_v4().to_string();
            let iq_id = uuid::Uuid::new_v4().to_string();
            let iq = build_room_search_history_iq(&iq_id, &query_id, max, &room_jid, &query);
            let page = send_mam_query_command(inner, iq, query_id).await?;
            to_js_value(&mam_page_to_js(page))
        })
    }

    pub fn search_dm_history(&self, peer_jid: String, query: String, max: u32) -> Promise {
        let inner = self.inner.clone();
        future_to_promise(async move {
            let query_id = uuid::Uuid::new_v4().to_string();
            let iq_id = uuid::Uuid::new_v4().to_string();
            let account_jid = {
                let stored = inner.borrow().config.clone();
                bare_jid(&stored.jid)
            };
            let iq =
                build_dm_search_history_iq(&iq_id, &query_id, max, &account_jid, &peer_jid, &query);
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
            let query_id = uuid::Uuid::new_v4().to_string();
            let iq_id = uuid::Uuid::new_v4().to_string();
            let mut builder = MamIqBuilder::new(&iq_id, &query_id, max).to_jid(&room_jid);
            builder = apply_page_param(builder, &page)?;
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
            let query_id = uuid::Uuid::new_v4().to_string();
            let iq_id = uuid::Uuid::new_v4().to_string();
            let account_jid = {
                let stored = inner.borrow().config.clone();
                bare_jid(&stored.jid)
            };
            let iq =
                build_dm_history_page_iq(&iq_id, &query_id, max, &account_jid, &peer_jid, &page)?;
            let result = send_mam_query_command(inner, iq, query_id).await?;
            to_js_value(&mam_page_to_js(result))
        })
    }

    pub fn fetch_personal_history_page(&self, max: u32, page_param: JsValue) -> Promise {
        let inner = self.inner.clone();
        future_to_promise(async move {
            let page: WaddleMamPageParam = serde_wasm_bindgen::from_value(page_param)
                .map_err(|err| js_error(err.to_string()))?;
            let query_id = uuid::Uuid::new_v4().to_string();
            let iq_id = uuid::Uuid::new_v4().to_string();
            let account_jid = {
                let stored = inner.borrow().config.clone();
                bare_jid(&stored.jid)
            };
            let iq = build_personal_history_page_iq(&iq_id, &query_id, max, &account_jid, &page)?;
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
            let account_jid = {
                let stored = inner.borrow().config.clone();
                bare_jid(&stored.jid)
            };
            let iq = build_dm_history_iq(
                &iq_id,
                &query_id,
                max,
                &account_jid,
                &peer_jid,
                before_id.as_deref(),
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

    /// Waddle-specific MAM stanza-id filter for 1:1 history. Targets the
    /// account's personal archive and constrains the query with `with=peer`.
    pub fn fetch_direct_messages_by_stanza_ids(
        &self,
        peer_jid: String,
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
            let account_jid = {
                let stored = inner.borrow().config.clone();
                bare_jid(&stored.jid)
            };
            let refs: Vec<&str> = stanza_ids.iter().map(String::as_str).collect();
            let query_id = uuid::Uuid::new_v4().to_string();
            let iq_id = uuid::Uuid::new_v4().to_string();
            let iq = MamIqBuilder::new(&iq_id, &query_id, stanza_ids.len() as u32)
                .before("")
                .to_jid(&account_jid)
                .with_jid(&peer_jid)
                .stanza_ids(&refs)
                .build();
            let page = send_mam_query_command(inner, iq, query_id).await?;
            to_js_value(&mam_page_to_js(page))
        })
    }
}

fn apply_page_param<'a>(
    builder: MamIqBuilder<'a>,
    page: &'a WaddleMamPageParam,
) -> Result<MamIqBuilder<'a>, JsValue> {
    let builder = match page.kind.as_str() {
        "latest" => builder.before(""),
        "before" => match page.before.as_deref() {
            Some(before) => builder.before(before),
            None => return Err(js_error("missing before page cursor")),
        },
        "after" => match page.after.as_deref() {
            Some(after) => builder.after(after),
            None => return Err(js_error("missing after page cursor")),
        },
        _ => return Err(js_error("invalid page param type")),
    };
    Ok(match page.start.as_deref() {
        Some(start) => builder.start(start),
        None => builder,
    })
}

fn build_personal_history_page_iq(
    iq_id: &str,
    query_id: &str,
    max: u32,
    account_jid: &str,
    page: &WaddleMamPageParam,
) -> Result<Element, JsValue> {
    let builder = MamIqBuilder::new(iq_id, query_id, max).to_jid(account_jid);
    Ok(apply_page_param(builder, page)?.build())
}

fn build_dm_history_page_iq(
    iq_id: &str,
    query_id: &str,
    max: u32,
    account_jid: &str,
    peer_jid: &str,
    page: &WaddleMamPageParam,
) -> Result<Element, JsValue> {
    let builder = MamIqBuilder::new(iq_id, query_id, max)
        .to_jid(account_jid)
        .with_jid(peer_jid);
    Ok(apply_page_param(builder, page)?.build())
}

fn build_dm_history_iq(
    iq_id: &str,
    query_id: &str,
    max: u32,
    account_jid: &str,
    peer_jid: &str,
    before_id: Option<&str>,
) -> Element {
    build_mam_iq(
        iq_id,
        query_id,
        max,
        before_id,
        Some(peer_jid),
        Some(account_jid),
    )
}

#[cfg(test)]
mod client_history_tests {
    use super::*;

    const TEST_MAM_NS: &str = "urn:xmpp:mam:2";
    const TEST_RSM_NS: &str = "http://jabber.org/protocol/rsm";

    fn mam_form_value(iq: &Element, field_var: &str) -> Option<String> {
        iq.get_child("query", TEST_MAM_NS)
            .and_then(|query| query.get_child("x", "jabber:x:data"))
            .and_then(|form| {
                form.children()
                    .find(|field| field.name() == "field" && field.attr("var") == Some(field_var))
            })
            .and_then(|field| field.get_child("value", "jabber:x:data"))
            .map(|element| element.text())
    }

    #[test]
    fn page_param_after_builds_rsm_after_query() {
        let page = WaddleMamPageParam {
            kind: "after".to_string(),
            before: None,
            after: Some("cursor-1".to_string()),
            start: None,
        };

        let iq = apply_page_param(MamIqBuilder::new("iq-1", "query-1", 10), &page)
            .expect("after page param should apply")
            .build();

        let after = iq
            .get_child("query", TEST_MAM_NS)
            .and_then(|query| query.get_child("set", TEST_RSM_NS))
            .and_then(|set| set.get_child("after", TEST_RSM_NS))
            .map(|element| element.text());
        assert_eq!(after.as_deref(), Some("cursor-1"));
    }

    #[test]
    fn page_param_start_builds_mam_start_field() {
        let page = WaddleMamPageParam {
            kind: "latest".to_string(),
            before: None,
            after: None,
            start: Some("2026-05-25T00:00:00Z".to_string()),
        };

        let iq = apply_page_param(MamIqBuilder::new("iq-1", "query-1", 10), &page)
            .expect("start page param should apply")
            .build();

        let value = mam_form_value(&iq, "start");
        assert_eq!(value.as_deref(), Some("2026-05-25T00:00:00Z"));
    }

    #[test]
    fn personal_history_page_targets_account_archive() {
        let page = WaddleMamPageParam {
            kind: "latest".to_string(),
            before: None,
            after: None,
            start: Some("2026-05-25T00:00:00Z".to_string()),
        };

        let iq = build_personal_history_page_iq("iq-1", "query-1", 10, "alice@example.com", &page)
            .expect("personal history IQ should build");

        assert_eq!(iq.attr("to"), Some("alice@example.com"));
        assert!(iq.get_child("query", TEST_MAM_NS).is_some());
    }

    #[test]
    fn dm_history_page_targets_account_archive_and_filters_peer() {
        let page = WaddleMamPageParam {
            kind: "latest".to_string(),
            before: None,
            after: None,
            start: None,
        };

        let iq = build_dm_history_page_iq(
            "iq-1",
            "query-1",
            10,
            "alice@example.com",
            "bob@example.com",
            &page,
        )
        .expect("DM history IQ should build");

        assert_eq!(iq.attr("to"), Some("alice@example.com"));
        let with_value = mam_form_value(&iq, "with");
        assert_eq!(with_value.as_deref(), Some("bob@example.com"));
    }

    fn build_dm_history_by_thread_iq(
        iq_id: &str,
        query_id: &str,
        max: u32,
        account_jid: &str,
        peer_jid: &str,
        thread_id: &str,
        before_id: Option<&str>,
    ) -> Element {
        // Mirrors `fetch_dm_history_by_thread`: always emit `<before>` (empty
        // for the latest page), so the server pages backward (newest-first).
        MamIqBuilder::new(iq_id, query_id, max)
            .to_jid(account_jid)
            .with_jid(peer_jid)
            .thread_id(thread_id)
            .before(before_id.unwrap_or(""))
            .build()
    }

    fn rsm_before(iq: &Element) -> Option<String> {
        iq.get_child("query", TEST_MAM_NS)
            .and_then(|query| query.get_child("set", TEST_RSM_NS))
            .and_then(|set| set.get_child("before", TEST_RSM_NS))
            .map(|element| element.text())
    }

    #[test]
    fn dm_history_by_thread_targets_account_filters_peer_and_thread() {
        let iq = build_dm_history_by_thread_iq(
            "iq-1",
            "query-1",
            10,
            "alice@example.com",
            "bob@example.com",
            "thread-42",
            None,
        );

        assert_eq!(iq.attr("to"), Some("alice@example.com"));
        assert_eq!(
            mam_form_value(&iq, "with").as_deref(),
            Some("bob@example.com")
        );
        assert_eq!(
            mam_form_value(&iq, "{urn:waddle:mam-thread:0}thread").as_deref(),
            Some("thread-42")
        );
    }

    #[test]
    fn dm_history_by_thread_latest_requests_newest_page_via_empty_before() {
        // No cursor → an empty `<before/>` must be present so the server pages
        // backward and returns the newest replies (not the oldest page).
        let iq = build_dm_history_by_thread_iq(
            "iq-1",
            "query-1",
            10,
            "alice@example.com",
            "bob@example.com",
            "thread-42",
            None,
        );

        assert_eq!(rsm_before(&iq).as_deref(), Some(""));
    }

    #[test]
    fn dm_history_by_thread_pages_older_with_rsm_before() {
        let iq = build_dm_history_by_thread_iq(
            "iq-1",
            "query-1",
            10,
            "alice@example.com",
            "bob@example.com",
            "thread-42",
            Some("cursor-7"),
        );

        assert_eq!(rsm_before(&iq).as_deref(), Some("cursor-7"));
        assert_eq!(
            mam_form_value(&iq, "{urn:waddle:mam-thread:0}thread").as_deref(),
            Some("thread-42")
        );
    }

    #[test]
    fn dm_legacy_history_targets_account_archive_and_filters_peer() {
        let iq = build_dm_history_iq(
            "iq-1",
            "query-1",
            10,
            "alice@example.com",
            "bob@example.com",
            Some("cursor-1"),
        );

        assert_eq!(iq.attr("to"), Some("alice@example.com"));
        let with_value = mam_form_value(&iq, "with");
        assert_eq!(with_value.as_deref(), Some("bob@example.com"));
        let before = iq
            .get_child("query", TEST_MAM_NS)
            .and_then(|query| query.get_child("set", TEST_RSM_NS))
            .and_then(|set| set.get_child("before", TEST_RSM_NS))
            .map(|element| element.text());
        assert_eq!(before.as_deref(), Some("cursor-1"));
    }
}
