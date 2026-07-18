use super::*;

#[instrument(skip(state, query))]
pub(super) async fn callback_handler(
    State(state): State<Arc<AuthState>>,
    Query(query): Query<CallbackQuery>,
) -> impl IntoResponse {
    if let Some(err) = query.error {
        let msg = query
            .error_description
            .unwrap_or_else(|| "provider returned an error".to_string());
        record_auth_failure("unknown", None, AuthFailure::AuthorizationRejected);
        return auth_error_to_response(AuthError::AuthorizationFailed(format!("{}: {}", err, msg)))
            .into_response();
    }

    let Some(state_key) = query.state else {
        record_auth_failure("unknown", None, AuthFailure::StateMismatch);
        return auth_error_to_response(AuthError::InvalidState).into_response();
    };
    let Some(code) = query.code else {
        record_auth_failure("unknown", None, AuthFailure::CallbackInvalidCode);
        return auth_error_to_response(AuthError::InvalidRequest("missing code/state".to_string()))
            .into_response();
    };

    let pending = match state.auth_handshake.take_pending(&state_key).await {
        Ok(Some(pending)) => pending,
        Ok(None) => {
            record_auth_failure("unknown", None, AuthFailure::StateMismatch);
            return auth_error_to_response(AuthError::InvalidState).into_response();
        }
        Err(err) => return auth_error_response("unknown", AuthStage::State, err),
    };

    if pending.is_expired() {
        record_auth_failure(&pending.provider_id, None, AuthFailure::StateExpired);
        return auth_error_to_response(AuthError::InvalidState).into_response();
    }

    if pending.state != state_key {
        record_auth_failure(&pending.provider_id, None, AuthFailure::StateMismatch);
        return auth_error_to_response(AuthError::InvalidState).into_response();
    }

    let provider = match state.providers.get(&pending.provider_id) {
        Some(p) => p,
        None => {
            record_auth_failure(
                &pending.provider_id,
                None,
                AuthFailure::CallbackInvalidClient,
            );
            return auth_error_to_response(AuthError::InvalidProvider(pending.provider_id))
                .into_response();
        }
    };

    let identity_claims = match provider.kind {
        AuthProviderKind::Oidc => {
            let mut provider_for_exchange = provider.clone();
            provider_for_exchange.client_id = pending.client_id.clone();
            provider_for_exchange.client_secret = pending.client_secret.clone();
            provider_for_exchange.token_endpoint_auth_method = pending.token_endpoint_auth_method;
            provider_for_exchange.require_dpop = pending.require_dpop;

            let issuer = provider.issuer.as_deref().ok_or_else(|| {
                AuthError::InvalidRequest("oidc provider missing issuer".to_string())
            });
            let issuer = match issuer {
                Ok(v) => v,
                Err(err) => return auth_error_response(&provider.id, AuthStage::OidcCallback, err),
            };

            let discovery = match oidc::discover(&state.http_client, issuer).await {
                Ok(v) => v,
                Err(err) => {
                    record_auth_failure(
                        &provider.id,
                        None,
                        AuthFailure::from_error(AuthStage::OidcCallback, &err),
                    );
                    let response_error = match err {
                        AuthError::HttpError(detail) => AuthError::AuthorizationFailed(detail),
                        other => other,
                    };
                    return auth_error_to_response(response_error).into_response();
                }
            };

            let token = match oidc::exchange_authorization_code(
                &state.http_client,
                &provider_for_exchange,
                &discovery,
                &code,
                &pending.redirect_uri,
                &pending.code_verifier,
                pending.require_dpop,
            )
            .await
            {
                Ok(v) => {
                    record_auth_success(AuthStage::TokenExchange);
                    v
                }
                Err(err) => {
                    return auth_error_response(&provider.id, AuthStage::TokenExchange, err)
                }
            };

            match oidc::claims_from_token_response(
                &state.http_client,
                &provider_for_exchange,
                &discovery,
                &token,
                Some(&pending.nonce),
            )
            .await
            {
                Ok(outcome) => {
                    match outcome.userinfo {
                        oidc::UserinfoFetchOutcome::NotRequested => {}
                        oidc::UserinfoFetchOutcome::Succeeded => {
                            record_auth_success(AuthStage::Userinfo);
                        }
                        oidc::UserinfoFetchOutcome::Failed(error) => {
                            record_auth_failure(
                                &provider.id,
                                None,
                                AuthFailure::from_error(AuthStage::Userinfo, &error),
                            );
                        }
                    }
                    outcome.claims
                }
                Err(err) => {
                    let stage = match &err {
                        AuthError::InvalidNonce => AuthStage::State,
                        AuthError::UserInfoFailed(_) => AuthStage::Userinfo,
                        _ => AuthStage::OidcCallback,
                    };
                    return auth_error_response(&provider.id, stage, err);
                }
            }
        }
        AuthProviderKind::OAuth2 => {
            let token_endpoint = match provider.token_endpoint.as_deref() {
                Some(v) => v,
                None => {
                    return auth_error_response(
                        &provider.id,
                        AuthStage::TokenExchange,
                        AuthError::InvalidRequest(
                            "oauth2 provider missing token_endpoint".to_string(),
                        ),
                    );
                }
            };

            let userinfo_endpoint = match provider.userinfo_endpoint.as_deref() {
                Some(v) => v,
                None => {
                    return auth_error_response(
                        &provider.id,
                        AuthStage::Userinfo,
                        AuthError::InvalidRequest(
                            "oauth2 provider missing userinfo_endpoint".to_string(),
                        ),
                    );
                }
            };

            let token = match oauth2::exchange_code(
                &state.http_client,
                provider,
                token_endpoint,
                &code,
                &pending.redirect_uri,
                &pending.code_verifier,
                pending.require_dpop,
            )
            .await
            {
                Ok(v) => {
                    record_auth_success(AuthStage::TokenExchange);
                    v
                }
                Err(err) => {
                    return auth_error_response(&provider.id, AuthStage::TokenExchange, err)
                }
            };

            match oidc::claims_from_oauth2_fallback(
                &state.http_client,
                provider,
                provider.issuer.clone(),
                &token.access_token,
                userinfo_endpoint,
            )
            .await
            {
                Ok(v) => {
                    record_auth_success(AuthStage::Userinfo);
                    v
                }
                Err(err) => return auth_error_response(&provider.id, AuthStage::Userinfo, err),
            }
        }
    };

    record_auth_success(AuthStage::State);

    let linked = match state
        .identity_service
        .resolve_or_create_user(provider, &identity_claims, &state.xmpp_domain)
        .await
    {
        Ok(v) => v,
        Err(err) => return auth_error_response(&provider.id, AuthStage::OidcCallback, err),
    };

    // Fire-and-forget the OIDC → PEP profile bridge so login latency
    // is unaffected. The hook is installed at HTTP bootstrap once
    // `WebSocketState` exists; if it isn't installed (e.g. tests that
    // bypass HTTP), this is a silent no-op.
    if let Some(hook) = state.profile_publish_hook() {
        if let Ok(jid) =
            format!("{}@{}", linked.user.xmpp_localpart, state.xmpp_domain).parse::<jid::BareJid>()
        {
            // Treat empty / whitespace-only `name` claims as absent so
            // the bridge does NOT publish a blank `<FN>` to vcard-temp
            // / vCard4. After this filter, `from_oidc_claims` maps
            // `None` to `NameIntent::Remove` so a name that disappears
            // from the IDP claim is actively cleared (instead of
            // stale-set).
            let display_name = identity_claims
                .name
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_string);

            // Three-way classification of the OIDC `picture` claim:
            //   - absent / null            → RemoveIfOidcOwned
            //   - present but unparseable  → Skip (preserve prior
            //     state — actively wiping on a typo'd URL would be
            //     destructive)
            //   - present and parseable    → SetFromUrl
            // We do NOT route through `from_oidc_claims` here because
            // its two-way mapping treats `None` as "remove", which
            // would conflate "IDP forgot the picture claim" with
            // "IDP returned a malformed URL".
            let raw_avatar = identity_claims
                .avatar_url
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty());
            let photo_intent = match raw_avatar {
                None => crate::profile::PhotoIntent::RemoveIfOidcOwned,
                Some(s) => match url::Url::parse(s) {
                    Ok(url) => crate::profile::PhotoIntent::SetFromUrl(url),
                    Err(parse_err) => {
                        tracing::warn!(
                            jid = %linked.user.xmpp_localpart,
                            url = %s,
                            error = %parse_err,
                            "OIDC `picture` claim is not a parseable URL; skipping PHOTO sync (prior avatar preserved)"
                        );
                        crate::profile::PhotoIntent::Skip
                    }
                },
            };
            let name_intent = match display_name {
                Some(s) => crate::profile::NameIntent::Set(s),
                None => crate::profile::NameIntent::Remove,
            };
            let source = crate::profile::ProfileSource::Oidc {
                photo: photo_intent,
                name: name_intent,
            };
            (hook)(jid, source);
        }
    }

    if let Err(err) = reconcile_user_membership(
        &state.permission_actor,
        &state.bootstrap_membership,
        &linked.user.jid,
        &linked.user.xmpp_localpart,
    )
    .await
    {
        return auth_error_response(
            &provider.id,
            AuthStage::OidcCallback,
            AuthError::DatabaseError(format!("Failed to provision account membership: {err}")),
        );
    }

    let session = Session::new(
        &linked.user.jid,
        &linked.user.username,
        &linked.user.xmpp_localpart,
    );

    if let Err(err) = state.session_manager.create_session(&session).await {
        return auth_error_response(&provider.id, AuthStage::OidcCallback, err);
    }

    match pending.flow {
        PendingFlow::Browser {
            next,
            session_transport,
        } => {
            let redirect_to = match session_transport {
                BrowserSessionTransport::Cookie => next.unwrap_or_else(|| "/".to_string()),
                BrowserSessionTransport::Fragment => {
                    let target = next.unwrap_or_else(|| "/".to_string());
                    let mut url = match url::Url::parse(&target) {
                        Ok(parsed) => parsed,
                        Err(_) => match url::Url::parse(&state.base_url)
                            .and_then(|base| base.join(&target))
                        {
                            Ok(parsed) => parsed,
                            Err(err) => {
                                return auth_error_response(
                                    &provider.id,
                                    AuthStage::OidcCallback,
                                    AuthError::InvalidRequest(format!(
                                        "invalid browser redirect target: {}",
                                        err
                                    )),
                                );
                            }
                        },
                    };

                    let mut fragment = url::form_urlencoded::Serializer::new(String::new());
                    if let Some(existing) = url.fragment() {
                        for (key, value) in
                            url::form_urlencoded::parse(existing.as_bytes()).into_owned()
                        {
                            if key != "waddle_session_id" {
                                fragment.append_pair(&key, &value);
                            }
                        }
                    }
                    fragment.append_pair("waddle_session_id", &session.id);
                    url.set_fragment(Some(&fragment.finish()));
                    url.to_string()
                }
            };

            let mut response = Redirect::temporary(&redirect_to).into_response();
            response.headers_mut().append(
                header::SET_COOKIE,
                state
                    .session_cookie_header(Some(&session.id), 60 * 60 * 24 * 30)
                    .parse()
                    .expect("valid cookie"),
            );
            record_auth_success(AuthStage::OidcCallback);
            response
        }
        PendingFlow::Device { device_code } => {
            // The approval must actually land on the device row: if the
            // grant expired and was pruned mid-callback, reporting
            // "Device authorized" would strand the poller forever and
            // leak the session we just created — so roll it back and
            // surface the expiry instead.
            // The session is already committed; any exit from this
            // block that can't hand it to the device poller must roll
            // it back or it lives as a 30-day orphan.
            let rollback_session = |err: AuthError| async {
                if let Err(rollback_err) = state.session_manager.delete_session(&session.id).await {
                    warn!(error = %rollback_err, "Failed to roll back session for failed device approval");
                }
                auth_error_to_response(err).into_response()
            };

            let approved = match state.auth_handshake.get_device(&device_code).await {
                Ok(Some(mut entry)) => {
                    if entry.is_expired() {
                        // The IdP round-trip outlived the device-code
                        // window. `update_device` is also time-gated in
                        // SQL, so this is belt-and-braces plus eager
                        // cleanup of the dead row.
                        if let Err(err) = state.auth_handshake.remove_device(&device_code).await {
                            warn!(error = %err, "Failed to remove expired device authorization");
                        }
                        false
                    } else {
                        entry.status = DeviceAuthStatus::Approved;
                        entry.session_id = Some(session.id.clone());
                        match state.auth_handshake.update_device(&entry).await {
                            Ok(approved) => approved,
                            Err(err) => {
                                record_auth_failure(
                                    &provider.id,
                                    None,
                                    AuthFailure::from_error(AuthStage::DeviceFlow, &err),
                                );
                                return rollback_session(err).await;
                            }
                        }
                    }
                }
                Ok(None) => false,
                Err(err) => {
                    record_auth_failure(
                        &provider.id,
                        None,
                        AuthFailure::from_error(AuthStage::DeviceFlow, &err),
                    );
                    return rollback_session(err).await;
                }
            };

            if !approved {
                if let Err(err) = state.session_manager.delete_session(&session.id).await {
                    warn!(error = %err, "Failed to roll back session for expired device grant");
                }
                record_auth_failure(&provider.id, None, AuthFailure::DeviceExpired);
                return auth_error_to_response(AuthError::InvalidRequest(
                    "device authorization expired; restart the device login".to_string(),
                ))
                .into_response();
            }

            record_auth_success(AuthStage::DeviceFlow);
            record_auth_success(AuthStage::OidcCallback);
            (
                StatusCode::OK,
                axum::response::Html("<html><body><h1>Device authorized</h1><p>You can close this window.</p></body></html>".to_string()),
            )
                .into_response()
        }
        PendingFlow::Xmpp {
            client_redirect_uri,
            client_state,
            client_code_challenge,
        } => {
            let auth_code = Uuid::new_v4().to_string();
            if let Err(err) = state
                .auth_handshake
                .insert_xmpp_code(
                    &auth_code,
                    &XmppAuthCode {
                        session_id: session.id.clone(),
                        redirect_uri: client_redirect_uri.clone(),
                        code_challenge: client_code_challenge,
                        created_at: Utc::now(),
                    },
                )
                .await
            {
                // No code ever reaches the client, so the session just
                // created would be a live 30-day orphan — roll it back.
                if let Err(rollback_err) = state.session_manager.delete_session(&session.id).await {
                    warn!(error = %rollback_err, "Failed to roll back session for failed xmpp code insert");
                }
                return auth_error_response(&provider.id, AuthStage::OidcCallback, err);
            }

            let mut redirect = match url::Url::parse(&client_redirect_uri) {
                Ok(v) => v,
                Err(err) => {
                    error!(error = %err, "Invalid XMPP redirect URI");
                    return auth_error_response(
                        &provider.id,
                        AuthStage::OidcCallback,
                        AuthError::InvalidRequest("invalid xmpp redirect_uri".to_string()),
                    );
                }
            };

            {
                let mut qp = redirect.query_pairs_mut();
                qp.append_pair("code", &auth_code);
                if let Some(state_value) = client_state {
                    qp.append_pair("state", &state_value);
                }
            }

            record_auth_success(AuthStage::OidcCallback);
            Redirect::temporary(redirect.as_str()).into_response()
        }
    }
}
