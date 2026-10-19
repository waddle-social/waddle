//! XEP-0402 / XEP-0492 bookmark payload validation and projection
//! mutation derivation: pure functions over published bookmark elements.

use super::*;

pub fn derive_bookmark_projection_mutation(
    owner_bare_jid: &BareJid,
    item_id: &str,
    payload: Option<&Element>,
    conversation_kind: ConversationKind,
    updated_at_ms: i64,
    source_version: i64,
) -> Result<NotificationSettingsProjectionMutation, NotificationSettingsProjectionError> {
    let payload = payload.ok_or_else(|| {
        NotificationSettingsProjectionError::InvalidBookmarkPayload(
            "bookmark publish item must contain a conference payload".to_string(),
        )
    })?;
    let bookmark = validate_xep0402_bookmark_publish(item_id, payload)?;
    derive_validated_bookmark_projection_mutation(
        owner_bare_jid,
        &bookmark,
        payload,
        conversation_kind,
        updated_at_ms,
        source_version,
    )
}

pub fn derive_validated_bookmark_projection_mutation(
    owner_bare_jid: &BareJid,
    bookmark: &waddle_xmpp::xep::xep0402::Bookmark,
    payload: &Element,
    conversation_kind: ConversationKind,
    updated_at_ms: i64,
    source_version: i64,
) -> Result<NotificationSettingsProjectionMutation, NotificationSettingsProjectionError> {
    let notify = bookmark_notify_element(payload)?;
    let Some(notify) = notify else {
        return Ok(NotificationSettingsProjectionMutation::Delete {
            owner_bare_jid: owner_bare_jid.clone(),
            conversation_jid: bookmark.jid.clone(),
            source: NotificationSettingsSource::Xep0402Bookmarks,
        });
    };

    let mode = match parse_notify_fallback_setting(notify) {
        Ok(Some(mode)) => mode,
        Ok(None) => {
            return Ok(NotificationSettingsProjectionMutation::Delete {
                owner_bare_jid: owner_bare_jid.clone(),
                conversation_jid: bookmark.jid.clone(),
                source: NotificationSettingsSource::Xep0402Bookmarks,
            });
        }
        Err(error) => return Err(error.into()),
    };
    let rich_payload_opt_in = waddle_xmpp::xep::xep0492::parse_rich_payload_opt_in(notify);

    Ok(NotificationSettingsProjectionMutation::Upsert(
        NotificationSettingsProjection {
            owner_bare_jid: owner_bare_jid.clone(),
            conversation_jid: bookmark.jid.clone(),
            conversation_kind,
            mode,
            rich_payload_opt_in,
            source_version,
            updated_at_ms,
            source: NotificationSettingsSource::Xep0402Bookmarks,
            source_item_jid: bookmark.jid.clone(),
        },
    ))
}

pub fn validate_xep0402_bookmark_publish(
    item_id: &str,
    payload: &Element,
) -> Result<waddle_xmpp::xep::xep0402::Bookmark, NotificationSettingsProjectionError> {
    let item_jid: BareJid = item_id.parse().map_err(|_| {
        NotificationSettingsProjectionError::InvalidConversationJid(item_id.to_string())
    })?;
    if item_jid.node().is_none() {
        return Err(NotificationSettingsProjectionError::InvalidConversationJid(
            item_id.to_string(),
        ));
    }

    let bookmark = waddle_xmpp::xep::xep0402::parse_bookmark(item_id, payload)?;
    validate_xep0402_conference_shape(payload)?;
    validate_xep0492_notify_extensions(payload)?;
    Ok(bookmark)
}

/// Publish-time validator for the Waddle DM-bookmark carrier
/// (`urn:waddle:dm-bookmarks:0`).
///
/// DM counterpart of [`validate_xep0402_bookmark_publish`]: it delegates
/// to the strict foundation parser
/// [`waddle_xmpp::xep::xep_waddle_dm_bookmarks::parse_dm_bookmark`],
/// which pins the `<dm-bookmark>` root, requires the item id to be a bare
/// JID with a localpart, and validates the single hosted XEP-0492
/// `<notify>`. The error maps to
/// [`NotificationSettingsProjectionError::InvalidDmBookmark`] via
/// `#[from]`.
pub fn validate_dm_bookmark_publish(
    item_id: &str,
    payload: &Element,
) -> Result<
    waddle_xmpp::xep::xep_waddle_dm_bookmarks::DmBookmark,
    NotificationSettingsProjectionError,
> {
    Ok(waddle_xmpp::xep::xep_waddle_dm_bookmarks::parse_dm_bookmark(item_id, payload)?)
}

/// Derive the projection mutation for a validated DM bookmark.
///
/// DM counterpart of [`derive_validated_bookmark_projection_mutation`].
/// The conversation kind is always [`ConversationKind::Direct`] and the
/// hosted XEP-0492 `<notify>` is [`DmBookmark::notify`] directly — there
/// is no XEP-0402 `<extensions>` indirection.
///
/// * `notify == None` (no `<notify>` child) → [`NotificationSettingsProjectionMutation::Delete`].
/// * `notify == Some(_)` with a parseable fallback setting → [`NotificationSettingsProjectionMutation::Upsert`].
/// * `notify == Some(_)` whose fallback resolves to `None` → `Delete` (no override).
/// * a malformed `<notify>` fallback → propagated [`NotificationSettingsProjectionError::InvalidNotify`].
///
/// [`DmBookmark`]: waddle_xmpp::xep::xep_waddle_dm_bookmarks::DmBookmark
/// [`DmBookmark::notify`]: waddle_xmpp::xep::xep_waddle_dm_bookmarks::DmBookmark::notify
pub fn derive_dm_bookmark_projection_mutation(
    owner_bare_jid: &BareJid,
    dm_bookmark: &waddle_xmpp::xep::xep_waddle_dm_bookmarks::DmBookmark,
    updated_at_ms: i64,
    source_version: i64,
) -> Result<NotificationSettingsProjectionMutation, NotificationSettingsProjectionError> {
    let Some(notify) = dm_bookmark.notify.as_ref() else {
        return Ok(NotificationSettingsProjectionMutation::Delete {
            owner_bare_jid: owner_bare_jid.clone(),
            conversation_jid: dm_bookmark.jid.clone(),
            source: NotificationSettingsSource::WaddleDmBookmarks,
        });
    };

    let mode = match parse_notify_fallback_setting(notify) {
        Ok(Some(mode)) => mode,
        Ok(None) => {
            return Ok(NotificationSettingsProjectionMutation::Delete {
                owner_bare_jid: owner_bare_jid.clone(),
                conversation_jid: dm_bookmark.jid.clone(),
                source: NotificationSettingsSource::WaddleDmBookmarks,
            });
        }
        Err(error) => return Err(error.into()),
    };
    let rich_payload_opt_in = waddle_xmpp::xep::xep0492::parse_rich_payload_opt_in(notify);

    Ok(NotificationSettingsProjectionMutation::Upsert(
        NotificationSettingsProjection {
            owner_bare_jid: owner_bare_jid.clone(),
            conversation_jid: dm_bookmark.jid.clone(),
            conversation_kind: ConversationKind::Direct,
            mode,
            rich_payload_opt_in,
            source_version,
            updated_at_ms,
            source: NotificationSettingsSource::WaddleDmBookmarks,
            source_item_jid: dm_bookmark.jid.clone(),
        },
    ))
}

fn validate_xep0402_conference_shape(
    payload: &Element,
) -> Result<(), NotificationSettingsProjectionError> {
    if payload
        .attr("autojoin")
        .is_some_and(|value| !matches!(value, "true" | "false" | "1" | "0"))
    {
        return Err(NotificationSettingsProjectionError::InvalidBookmarkPayload(
            "autojoin must be an XML Schema boolean".to_string(),
        ));
    }

    let mut nick_count = 0_u8;
    let mut password_count = 0_u8;
    let mut extensions_count = 0_u8;
    for child in payload.children() {
        if child.ns() != waddle_xmpp::xep::xep0402::NS_BOOKMARKS2 {
            return Err(NotificationSettingsProjectionError::InvalidBookmarkPayload(
                "foreign elements must be nested inside XEP-0402 extensions".to_string(),
            ));
        }
        match child.name() {
            "nick" => nick_count = nick_count.saturating_add(1),
            "password" => password_count = password_count.saturating_add(1),
            "extensions" => extensions_count = extensions_count.saturating_add(1),
            other => {
                return Err(NotificationSettingsProjectionError::InvalidBookmarkPayload(
                    format!("unexpected XEP-0402 conference child: {other}"),
                ));
            }
        }
    }
    if nick_count > 1 || password_count > 1 || extensions_count > 1 {
        return Err(NotificationSettingsProjectionError::InvalidBookmarkPayload(
            "nick, password, and extensions may appear at most once".to_string(),
        ));
    }
    Ok(())
}

fn validate_xep0492_notify_extensions(
    bookmark_payload: &Element,
) -> Result<(), NotificationSettingsProjectionError> {
    let Some(extensions) =
        bookmark_payload.get_child("extensions", waddle_xmpp::xep::xep0402::NS_BOOKMARKS2)
    else {
        return Ok(());
    };

    let mut notify_count = 0_u8;
    for child in extensions.children() {
        if child.ns() == waddle_xmpp::xep::xep0402::NS_BOOKMARKS2 {
            return Err(NotificationSettingsProjectionError::InvalidBookmarkPayload(
                "XEP-0402 extensions must contain extension namespaces".to_string(),
            ));
        }
        if child.ns() == waddle_xmpp::xep::NS_NOTIFICATION_SETTINGS {
            if !is_notify_element(child) {
                return Err(NotificationSettingsProjectionError::InvalidNotify(
                    waddle_xmpp::xep::NotificationSettingsError::NotNotifyElement,
                ));
            }
            validate_notify_element(child)?;
            notify_count = notify_count.saturating_add(1);
        }
    }
    if notify_count > 1 {
        return Err(NotificationSettingsProjectionError::MultipleNotifyElements);
    }
    Ok(())
}

fn bookmark_notify_element(
    bookmark_payload: &Element,
) -> Result<Option<&Element>, NotificationSettingsProjectionError> {
    let Some(extensions) =
        bookmark_payload.get_child("extensions", waddle_xmpp::xep::xep0402::NS_BOOKMARKS2)
    else {
        return Ok(None);
    };
    let mut notify_children = extensions
        .children()
        .filter(|child| is_notify_element(child));
    let notify = notify_children.next();
    if notify_children.next().is_some() {
        return Err(NotificationSettingsProjectionError::MultipleNotifyElements);
    }
    Ok(notify.filter(|element| is_notify_element(element)))
}
