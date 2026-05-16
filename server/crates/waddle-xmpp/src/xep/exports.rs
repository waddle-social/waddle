pub use super::xep0004::{
    find_data_form, is_data_form, DataForm, DataFormError, Field, FieldOption, FieldType, FormType,
    FromElement, IntoElement, NS_DATA_FORMS,
};

pub use super::xep0012::{build_last_activity_response, is_last_activity_query, NS_LAST_ACTIVITY};

pub use super::xep0047::{
    build_ibb_close, build_ibb_data_element, build_ibb_data_iq, build_ibb_item_not_found,
    build_ibb_not_acceptable, build_ibb_open, build_ibb_resource_constraint, build_ibb_result,
    build_ibb_unexpected_request, is_ibb_close, is_ibb_data, is_ibb_open, message_has_ibb_data,
    next_seq, parse_ibb_close, parse_ibb_data_from_iq, parse_ibb_data_from_message, parse_ibb_open,
    validate_data_size, IbbClose, IbbData, IbbError, IbbOpen, StanzaType as IbbStanzaType, NS_IBB,
};

pub use super::xep0050::{
    build_bad_request as build_command_bad_request, build_bad_session_id, build_command_error,
    build_command_items, build_command_result, build_forbidden as build_command_forbidden,
    build_item_not_found as build_command_item_not_found,
    build_not_allowed as build_command_not_allowed, build_session_expired,
    is_command_node_disco_info, is_command_request, is_commands_disco_info,
    is_commands_disco_items, parse_command_from_iq, Action as CommandAction,
    AllowedActions as CommandAllowedActions, Command, CommandDefinition, CommandError,
    Note as CommandNote, NoteType as CommandNoteType, Status as CommandStatus, NODE_COMMANDS,
    NS_COMMANDS,
};

pub use super::xep0054::{
    build_empty_vcard_response, build_vcard_element, build_vcard_response, build_vcard_success,
    is_vcard_get, is_vcard_set, parse_vcard_element, parse_vcard_from_iq, VCard, VCardError,
    VCardPhoto, NS_VCARD,
};

pub use super::xep0077::{
    build_registration_error, build_registration_fields_response, build_registration_success,
    is_registration_query, parse_registration_iq, RegistrationError, RegistrationRequest,
};

pub use super::xep0115::{
    build_caps_element, build_caps_element_with_extensions, build_waddle_caps_element,
    compute_caps_hash, compute_caps_hash_with_extensions, ensure_caps_payload,
    extract_caps_from_presence, is_caps_node_query, parse_caps_node, CachedDiscoInfo, Caps,
    CapsCache, NS_CAPS, WADDLE_CAPS_NODE,
};

pub use super::xep0249::{
    build_direct_invite, build_invite_message, is_direct_invite, message_has_direct_invite,
    parse_direct_invite, parse_direct_invite_from_message, DirectInvite, NS_CONFERENCE,
};

pub use super::xep0363::{
    build_upload_error, build_upload_slot_response, effective_content_type, is_upload_request,
    parse_upload_request, sanitize_filename, UploadError, UploadRequest, UploadSlot,
    DEFAULT_MAX_FILE_SIZE, NS_HTTP_UPLOAD,
};

pub use super::xep0191::{
    build_block_push, build_blocking_error, build_blocking_success, build_blocklist_response,
    build_unblock_push, is_block_set, is_blocking_query, is_blocklist_get, is_unblock_set,
    parse_blocking_request, BlockingError, BlockingRequest, NS_BLOCKING,
};

pub use super::xep0085::{
    build_chat_state_element, build_chat_state_message, extract_chat_state_from_message,
    is_chat_state_element, is_standalone_notification, parse_chat_state, set_chat_state,
    strip_chat_states, ChatState as Xep0085ChatState, ChatStateCarrier, ChatStateError,
    NS_CHATSTATES,
};

pub use super::xep0184::{
    build_receipt_message, build_receipt_received_element, build_receipt_request_element,
    extract_receipt_from_message, extract_received_id, has_receipt_received, has_receipt_request,
    is_receipt_received_element, is_receipt_request_element, is_standalone_receipt,
    set_receipt_received, set_receipt_request, strip_receipts, ReceiptCarrier, ReceiptError,
    ReceiptKind, NS_RECEIPTS,
};

pub use super::xep0199::{build_ping_result, is_ping, NS_PING};

pub use super::xep0092::{
    build_version_element, build_version_response, is_version_query, parse_version_response,
    SoftwareVersion, NS_VERSION,
};

pub use super::xep0106::{escape_node, is_escaped, needs_escaping, unescape_node, JidEscaping};

pub use super::xep0107::{
    build_mood_element, build_mood_retraction, is_mood_element, parse_mood_element, Mood,
    MoodError, MoodKind, NS_MOOD, PEP_NODE_MOOD,
};

pub use super::xep0108::{
    build_activity_element, build_activity_retraction, is_activity_element, parse_activity_element,
    Activity, ActivityError, GeneralActivity, SpecificActivity, NS_ACTIVITY, PEP_NODE_ACTIVITY,
};

pub use super::xep0118::{
    build_tune_element, build_tune_retraction, is_tune_element, parse_tune_element, Tune,
    TuneError, NS_TUNE, PEP_NODE_TUNE,
};

pub use super::xep0172::{
    build_nick_element, extract_nickname_from_message, extract_nickname_from_presence, has_nick,
    is_nick_element, set_nickname, strip_nickname, Nickname, NicknameCarrier, NS_NICK,
};

pub use super::xep0202::{
    build_current_time_response, build_time_response, is_time_query, parse_time_response,
    EntityTime, NS_TIME,
};

pub use super::xep0203::{
    add_delay, add_delay_stamp, build_delay_element, build_delay_element_simple,
    extract_delay_from_message, extract_delay_stamp, has_delay, is_delay_element,
    parse_delay_element, strip_delay, DelayCarrier, DelayError, DelayInfo, NS_DELAY,
};

pub use super::xep0292::{
    build_vcard4_element, is_vcard4_element, parse_vcard4, VCard4, VCard4Error, NS_VCARD4,
    PEP_NODE_VCARD4,
};

pub use super::xep0300::{
    build_hash_element, compute_hash, parse_hash_element, sha1_hex, sha256_base64, sha256_hex,
    verify_hash, HashAlgo, HashError, HashValue, Hashable, NS_HASHES,
};

pub use super::xep0297::{
    build_forwarded_element, build_forwarded_now, build_forwarded_with_delay,
    extract_forwarded_from_message, is_forwarded_element, parse_forwarded_element,
    ForwardedMessage, ForwardingCarrier, NS_FORWARD,
};

pub use super::xep0308::{
    build_correction_message, build_replace_element, extract_correction_from_message,
    extract_replaces_id, is_correction_message, is_replace_element, parse_correction_from_message,
    set_correction, strip_correction, Correction, CorrectionCarrier, CorrectionError,
    NS_MESSAGE_CORRECT,
};

pub use super::xep0317::{
    build_hats_element, extract_hats_from_presence, has_hats, is_hats_element, parse_hats_element,
    set_hats, strip_hats, Hat, HatCarrier, HatSet, NS_HATS,
};

pub use super::xep0319::{
    add_idle, build_idle_element, extract_idle_from_presence, has_idle, is_idle_element,
    parse_idle_element, strip_idle, IdleCarrier, IdleError, IdleInfo, NS_IDLE,
};

pub use super::xep0333::{
    add_markable, build_displayed_element, build_displayed_message, build_markable_element,
    extract_marker_from_message, extract_marker_id, has_markable, has_marker, is_marker_element,
    is_standalone_marker, strip_markers, Marker, MarkerCarrier, MarkerError, NS_CHAT_MARKERS,
};

pub use super::xep0334::{
    add_hint, build_hint_element, extract_hints_from_message, has_hint, is_hint_element,
    remove_hint, should_skip_carbons, should_skip_storage, strip_hints, Hint, HintCarrier,
    NS_HINTS,
};

pub use super::xep0372::{
    add_reference, build_reference_element, extract_mention_uris, extract_mentioned_jids,
    extract_references_from_message, has_references, is_reference_element, parse_reference_element,
    strip_references, Reference, ReferenceCarrier, ReferenceError, ReferenceType, NS_REFERENCE,
};

pub use super::xep0377::{
    build_report_element, is_report_element, parse_report, Report, ReportReason, ReportRecord,
    ReportStore, NS_REPORTING,
};

pub use waddle_xmpp_core::xep0392::{
    apply_cvd_correction, compute_hue, generate_color, generate_color_with_params, ConsistentColor,
    CvdCorrection, HslColor, DEFAULT_LIGHTNESS, DEFAULT_SATURATION,
};

pub use super::xep0393::{
    blocks_to_html, blocks_to_plain, parse_blocks, parse_spans, spans_to_html, spans_to_plain,
    Block, Span, StyledBody,
};

pub use waddle_xmpp_core::xep0359::{
    add_origin_id, add_stanza_id as add_stanza_id_xep0359, build_origin_id_element,
    build_stanza_id_element, extract_origin_id as extract_origin_id_xep0359, extract_origin_id_str,
    extract_stanza_id_by, extract_stanza_ids, has_origin_id, has_stanza_id, is_origin_id_element,
    is_stanza_id_element, remove_stanza_ids_by, strip_all_ids, OriginId as Xep0359OriginId,
    StanzaId as Xep0359StanzaId, StanzaIdCarrier, NS_SID,
};

pub use super::xep0357::{
    build_push_disable_result, build_push_enable_result, is_push_disable, is_push_enable,
    parse_push_disable, parse_push_enable, PushDisable, PushEnable, NS_PUBSUB_PUBLISH_OPTIONS,
    NS_PUSH,
};

pub use super::xep0410::{
    build_self_ping, interpret_self_ping_response, is_self_ping, SelfPingResult,
    FEATURE_MUC_SELFPING, PING_TIMEOUT_SECS, RECOMMENDED_INTERVAL_SECS,
};

pub use super::xep0401::{AccountInvite, InviteRedeemError, InviteStore, COMMAND_NODE_INVITE};

pub use super::xep0421::{
    build_occupant_id_element, extract_occupant_id_from_message, extract_occupant_id_from_presence,
    generate_occupant_id, is_occupant_id_element, set_occupant_id_on_message,
    set_occupant_id_on_presence, strip_occupant_id_from_message, strip_occupant_id_from_presence,
    OccupantId, OccupantIdCarrier, NS_OCCUPANT_ID,
};

pub use super::xep0424::{
    build_retract_element, build_retracted_element, build_retraction_message,
    build_tombstone_message, extract_retraction_from_message, extract_retracts_id,
    is_retract_element, is_retracted_element, is_retraction_message, is_tombstone_message,
    set_retraction, strip_retraction, Retracted, Retraction, RetractionCarrier, RetractionError,
    RetractionKind, NS_MESSAGE_RETRACT,
};

pub use super::xep0425::{
    build_moderated_retract_element, build_moderation_result_message, extract_moderation_result,
    is_moderation_result_message, parse_moderation_iq, ModerationCarrier, ModerationRequest,
    ModerationResult, NS_MESSAGE_MODERATE,
};

pub use super::xep0444::{
    build_reaction_element, build_reaction_message, build_reactions_element, extract_reacted_id,
    extract_reactions_from_message, is_reaction_message, is_reactions_element, set_reactions,
    strip_reactions, ReactionCarrier, ReactionError, ReactionSet, NS_REACTIONS,
};

pub use super::xep0428::{
    build_fallback_element, is_fallback_element, parse_fallbacks_from_message,
    set_fallback_payloads, strip_fallback_ranges, FallbackIndication, FallbackRange, NS_FALLBACK,
};

pub use super::xep0430::{
    build_entry_element, build_inbox_query_result, build_mark_read_result, is_inbox_iq,
    parse_entry_element, parse_inbox_query, parse_mark_read, InboxError, InboxMarkRead, InboxQuery,
    NS_INBOX,
};

pub use super::xep0402::{
    build_bookmark_element, build_bookmark_item, is_bookmarks_node, parse_bookmark, Bookmark,
    BookmarkError, NS_BOOKMARKS2, PEP_NODE as BOOKMARKS_PEP_NODE,
};

pub use super::xep0048::{
    build_legacy_bookmarks_element, from_native_bookmark, is_legacy_bookmarks_namespace,
    parse_legacy_bookmarks, to_native_bookmark, LegacyBookmark, NS_BOOKMARKS_LEGACY,
};

pub use super::xep0049::{
    build_private_storage_result, build_private_storage_success, is_private_storage_query,
    parse_private_storage_get, parse_private_storage_set, PrivateStorageKey, NS_PRIVATE,
};

pub use super::xep0084::{
    build_avatar_data, build_avatar_metadata, compute_avatar_hash, is_avatar_data_node,
    is_avatar_metadata_node, parse_avatar_data, parse_avatar_metadata, AvatarInfo,
    NODE_AVATAR_DATA, NODE_AVATAR_METADATA, NS_AVATAR_DATA, NS_AVATAR_METADATA,
};

pub use super::xep0153::{
    build_vcard_update_element, compute_photo_hash, compute_photo_hash_from_base64,
    has_vcard_update, parse_vcard_update, NS_VCARD_UPDATE,
};

pub use super::xep0223::{
    is_private_storage_node, FEATURE_ACCESS_WHITELIST, FEATURE_PERSISTENT_ITEMS,
};

pub use super::xep0447::{
    build_file_sharing_element, extract_file_sharing_from_message, has_file_sharing,
    is_file_sharing_element, parse_file_sharing_element, set_file_sharing, strip_file_sharing,
    Disposition, FileSharing, FileSharingCarrier, Source, NS_SFS, NS_URL_DATA,
};

pub use super::xep0445::{
    build_preauth_element, extract_preauth, has_preauth, is_preauth_element, PreauthToken,
    PreauthValidation, NS_PARS,
};

pub use super::xep0446::{
    build_file_metadata_element, extract_file_metadata_from_message, has_file_metadata,
    is_file_metadata_element, parse_file_metadata_element, set_file_metadata, strip_file_metadata,
    FileMetadata, FileMetadataCarrier, FileMetadataError, NS_FILE_METADATA,
};

pub use super::xep0448::{
    build_encrypted_element, extract_encrypted_file, is_encrypted_file_element,
    parse_encrypted_element, set_encrypted_file, Cipher, EncryptedFile, EncryptedFileError,
    EncryptedHash, NS_ESFS,
};

pub use super::xep0461::{
    build_reply_element, is_reply_element, parse_reply_from_message, set_reply_payload,
    ReplyReference, NS_REPLY,
};

pub use super::xep0059::{
    build_rsm_request_element, build_rsm_response_element, extract_rsm_request,
    extract_rsm_response, is_rsm_element, parse_rsm_request, parse_rsm_response, RsmError,
    RsmPaginated, RsmRequest, RsmResponse, NS_RSM,
};

pub use super::xep0431::{matches_fulltext, MamSearchQuery, SearchResult, FIELD_FULLTEXT};

pub use super::xep0437::UnreadTracker;

pub use super::xep0433::{
    build_search_request, build_search_response, is_search_request, parse_search_request,
    parse_search_results, ChannelResult, SearchRequest, Searchable, NS_CHANNEL_SEARCH,
};

pub use super::xep0449::{
    build_sticker_element, build_sticker_message, extract_sticker_ref, is_sticker_element,
    is_sticker_message, set_sticker_ref, strip_sticker_ref, Sticker, StickerCarrier, StickerPack,
    StickerRef, NS_STICKERS,
};

pub use super::xep0452::{
    build_mention_notification_element, build_mention_notification_message,
    extract_mention_notification, has_mention_notification, is_mention_notification_element,
    set_mention_notification, strip_mention_notification, MentionCounter, MentionNotification,
    MentionNotificationCarrier, NS_MENTION_NOTIFICATION,
};

pub use super::xep0471::{
    build_event_element, is_event_element, parse_event, CalendarEvent, Rsvp, RsvpStatus,
    NS_CALENDAR, PUBSUB_NODE_EVENTS,
};

pub use super::xep0470::{
    build_attachments_element, is_attachments_element, parse_attachment_target, Attachment,
    AttachmentPayload, AttachmentTarget, NS_PUBSUB_ATTACHMENTS,
};

pub use super::xep_waddle_pin::{
    build_pinned_element as build_pinned_message_element,
    build_unpinned_element as build_unpinned_message_element, extract_pin_intent_from_message,
    parse_pinned_element as parse_pinned_message_element,
    parse_unpinned_element as parse_unpinned_message_element, PinIntent, NS_WADDLE_PIN_V0,
};

pub use waddle_xmpp_core::xep0472::{
    build_feed_entry_element, is_feed_entry, parse_feed_entry, FeedEntry, NS_SOCIAL_FEED,
    PUBSUB_NODE_FEED,
};

pub use super::xep0492::{
    build_notification_setting_child, build_notification_settings_element, build_notify_element,
    is_notification_settings_element, is_notify_element, parse_notification_setting,
    parse_notify_fallback_setting, replace_fallback_notification_setting, validate_notify_element,
    NotificationLevel, NotificationSettings, NotificationSettingsError, RoomNotificationSetting,
    NS_NOTIFICATION_SETTINGS,
};

pub use super::xep0469::{
    build_pinned_element, get_pin_state, is_bookmark_pinned, is_pinned_element, pin_bookmark,
    set_pin_state, sort_bookmarks_pinned_first, unpin_bookmark, PinState, Pinnable,
    NS_BOOKMARKS_PINNING,
};

pub use super::xep0501::{
    build_story_element, filter_active, is_story_element, parse_story, Story, DEFAULT_EXPIRY_HOURS,
    NS_STORIES, PUBSUB_NODE_STORIES,
};

pub use super::xep0502::{
    build_activity_notification, build_subscribe_element,
    is_activity_element as is_muc_activity_element, parse_activity_notifications, ActivityTracker,
    RoomActivity, NS_MUC_ACTIVITY,
};

pub use super::xep0500::{
    parse_slow_mode_interval, SlowModeCheck, SlowModeConfig, SlowModeTracker,
    FIELD_SLOW_MODE_INTERVAL, SLOW_MODE_DISABLED,
};

pub use super::xep0486::{extract_avatar_hash_from_presence, MucAvatar, MucAvatarCache};

pub use super::xep0488::{
    build_invite_message_element, build_invite_request, build_invite_response,
    build_invite_share_message, extract_invite_from_iq, extract_invite_from_message,
    has_invite_in_message, is_invite_element, is_invite_request, set_invite_on_message,
    strip_invite_from_message, InviteToken, InviteTokenCarrier, InviteTokenError,
    NS_MUC_TOKEN_INVITE,
};

pub use super::xep0513::{
    build_mention_element, build_mentions_elements, extract_explicit_mentions,
    has_explicit_mentions, is_mention_element, parse_mention_element, set_explicit_mentions,
    strip_explicit_mentions, ExplicitMention, ExplicitMentionCarrier, ExplicitMentions,
    CHANNEL_MENTION, NS_EXPLICIT_MENTIONS,
};

pub use super::xep0508::{
    build_thread_create_element, build_thread_reply_element, extract_forum_action,
    has_forum_action, is_forum_element, set_thread_create, set_thread_reply, strip_forum,
    ForumAction, ForumCarrier, ThreadCreate, ThreadReply, ThreadSummary, FIELD_FORUM_MODE,
    NS_FORUMS,
};

pub use super::xep0503::{
    build_channel_item, build_muc_roominfo_form, build_muc_roominfo_pubsub_form,
    build_room_metadata_form, build_room_space_metadata_forms,
    build_room_space_metadata_forms_with_description, build_server_role_form, build_space_node_iri,
    build_space_parent_form, build_spaces_metadata_form, build_spaces_metadata_form_for_requester,
    build_spaces_type_form, SpaceAffiliation, NS_SPACES, NS_WADDLE_ROOM_METADATA,
};

// Re-export commonly used items at the xep module level
