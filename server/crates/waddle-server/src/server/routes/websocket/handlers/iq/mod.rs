use chrono;
use jid::{BareJid, FullJid, Jid};
use std::collections::HashSet;
use std::sync::Arc;
use tracing::{debug, info, warn};
use waddle_xmpp::carbons::CARBONS_NS;
use waddle_xmpp::commands::{CommandContext, CommandResult};
use waddle_xmpp::disco::{
    build_disco_info_response, build_disco_info_response_with_extensions,
    build_disco_items_response, community_service_features, muc_room_features,
    muc_service_features, parse_disco_info_query, parse_disco_items_query, push_service_features,
    spaces_service_features, upload_service_features, DiscoItem, Feature, Identity,
};
use waddle_xmpp::mam::{
    build_fin_iq, build_query_form_iq, build_result_messages, is_mam_query,
    is_mam_query_form_request, parse_mam_query, ArchivedMessage, ArchivedModeration,
    ArchivedRichMessage, ArchivedRichPayload, RichMessageId, RichText,
};
use waddle_xmpp::muc::admin::{
    build_admin_result, build_admin_set_result, build_role_result, is_muc_admin_iq,
    is_role_change_query, parse_admin_query, AdminItem,
};
use waddle_xmpp::muc::owner::build_config_form;
use waddle_xmpp::muc::room_actor::{
    ApplyAdminItems, EnforceMembersOnly, EnforceMembersOnlyAffiliations, GetAdminContext,
    GetOccupantByJid, GetSnapshot, PingSelfCheck, UpdateConfig,
};
use waddle_xmpp::muc::DATA_FORMS_NS;
use waddle_xmpp::presence::subscription::{
    build_subscription_presence, SubscriptionStateMachine, SubscriptionType,
};
use waddle_xmpp::protocol::{ConnectionPhase, StanzaContext as ProtocolStanzaContext};
use waddle_xmpp::pubsub::{
    build_pep_identity, build_pubsub_affiliations_result, build_pubsub_configure_form_result,
    build_pubsub_error, build_pubsub_items_result, build_pubsub_owner_subscriptions_result,
    build_pubsub_publish_result, build_pubsub_subscribe_result, build_pubsub_success,
    is_pep_request, is_pep_request_to, is_pubsub_iq, parse_pubsub_iq, pep_features, PubSubError,
    PubSubItem, PubSubRequest, SubId, SubscriptionState, PEP_NODE_AVATAR_DATA,
    PEP_NODE_AVATAR_METADATA,
};
use waddle_xmpp::registry::BroadcastOutcome;
use waddle_xmpp::roster::{
    build_roster_push, build_roster_result, build_roster_result_empty, parse_roster_get,
    parse_roster_set, AskType, RosterItem, RosterSetResult, RosterVersion, Subscription, ROSTER_NS,
};
use waddle_xmpp::xep::xep0357::{
    build_push_disable_result, build_push_enable_result, is_push_disable, is_push_enable,
    parse_push_disable, parse_push_enable,
};
use waddle_xmpp::xep::xep0363::{
    build_upload_error, build_upload_slot_response, effective_content_type, is_upload_request,
    parse_upload_request, sanitize_filename, UploadError, UploadSlot,
};
use waddle_xmpp::xep::xep0430::{
    build_inbox_entry_message, build_inbox_fin_iq, build_mark_read_result, is_inbox_iq,
    is_mark_read_iq, parse_inbox_query, parse_mark_read, InboxFinCounts, InboxLastMessage,
};
use waddle_xmpp::xep::{
    add_stanza_id_xep0359, build_command_items, build_command_result, build_last_activity_response,
    build_moderation_result_message, build_room_metadata_form, build_search_form_response,
    build_search_response_with_rsm, build_server_role_form, build_space_node_iri,
    build_space_parent_form, build_spaces_metadata_form_for_requester_with_owners,
    is_last_activity_query, is_search_form_request, is_search_request, is_time_query,
    is_version_query, parse_command_from_iq, parse_moderation_iq, parse_search_request,
    AdHocCommandCondition, ChannelResult, Command, CommandError, CommandStatus, RsmRequest,
    RsmResponse, Searchable, SpaceAffiliation, Xep0359StanzaId, NODE_COMMANDS, NS_CHANNEL_SEARCH,
};
use waddle_xmpp::{
    Affiliation, SpaceDetails, Stanza, StanzaErrorCondition, StanzaErrorType, XmppError,
};
use xmpp_parsers::minidom::Element;

use crate::server::routes::websocket::frame::ResponseBatch;
use crate::telemetry::mark_span_error;

mod archive_inbox_upload;
pub(crate) mod blocking;
mod caps_result;
mod commands;
mod community_items;
mod community_publish;
mod community_retract;
mod community_rsvp;
mod conn_state;
mod disco_info;
mod disco_items;
pub(crate) mod errors;
mod extension_forms;
mod extension_route_items;
mod full_jid_forward;
mod jingle_muji_gate;
#[cfg(feature = "clustering")]
pub(crate) mod jingle_muji_relay;
mod last_activity;
mod link_preview_lookup;
pub(crate) mod link_preview_player_embed;
mod link_preview_resolver;
mod mentions_permissions;
mod muc_admin;
#[cfg(feature = "clustering")]
pub(crate) mod muc_admin_relay;
// XEP-0045 §7.8.2 (#1248): the members-only mediated-invite auto-add in
// `handlers::message::muc_invite` persists the granted membership with
// the exact same tuple write the admin affiliation path uses.
pub(in crate::server::routes::websocket::handlers) use muc_admin::persist_managed_channel_affiliation;
mod muc_occupant_disco;
mod muc_owner_config;
pub(crate) mod muc_owner_moderation;
mod muc_self_ping;
mod pep_addressing;
mod permissions;
mod pin_query;
mod pubsub_admin;
mod pubsub_dispatch;
mod push;
mod remote_destination;
pub(crate) mod roster;
mod sans_io;
mod search;
mod session_jid;
mod spaces_bookmark_cleanup;
mod spaces_discovery;
mod spaces_items;
mod spaces_publish;
mod spaces_retract;
mod story_attachments;
#[cfg(test)]
mod test_helpers;
mod vcard_private;

use archive_inbox_upload::handle_archive_inbox_upload_iq;
use blocking::handle_blocking_iq;
use caps_result::handle_caps_disco_info_result;
use commands::{handle_command_iq, CommandTargets};
use community_items::handle_community_items;
use community_publish::handle_community_publish;
use community_retract::handle_community_retract;
pub use conn_state::IqConnState;
use disco_info::handle_disco_info_iq;
use disco_items::handle_disco_items_iq;
use extension_forms::{
    command_name_by_boundary, command_refs_by_boundary, extension_command_metadata_form,
    extension_features_for_disco, extension_route_disco_node, extension_route_metadata_form,
    CommandBoundary, EXTENSION_COMMAND_FORM_TYPE, EXTENSION_ROUTE_FORM_TYPE,
};
use extension_route_items::{handle_extension_route_items, PubSubItemsRead};
use full_jid_forward::route_full_jid_iq;
use last_activity::handle_last_activity_iq;
use link_preview_lookup::{handle_link_preview_lookup_iq, is_link_preview_lookup_iq};
use mentions_permissions::{handle_mentions_permissions_iq, is_mentions_permissions_iq};
use muc_owner_config::apply_muc_owner_config;
use muc_owner_moderation::handle_muc_owner_and_moderation_iq;
use muc_self_ping::handle_muc_self_ping_iq;
use pep_addressing::is_pep_self_or_to;
pub(crate) use permissions::managed_channel_permission_allowed;
use permissions::{
    build_muc_owner_config_response, build_xmpp_error_response, global_database,
    muc_owner_authorized, server_affiliation_for_requester, space_affiliation_for_requester,
    spaces_node_mutation_allowed,
};
use pin_query::{handle_pin_query_iq, is_pin_query_iq};
use pubsub_dispatch::handle_pubsub_iq;
use spaces_discovery::{
    room_space_link, space_details_from_node, spaces_service_bare_jid, RoomSpaceLink,
};
use spaces_items::handle_spaces_items;
use spaces_publish::handle_spaces_publish;
use spaces_retract::handle_spaces_retract;
#[cfg(test)]
pub use test_helpers::handle_iq;
// Re-exported via `pub(super)` so submodules that wildcard-import the
// IQ-handler module's namespace (`use super::*;`) pick up the typed
// IQ-error constructors without each having to re-import the
// `errors` submodule directly.
pub(super) use errors::{
    bad_format_iq_error, bad_request_iq_error, conflict_iq_error, feature_not_implemented_iq_error,
    forbidden_iq_error, internal_server_error_iq_error, item_not_found_iq_error,
    jid_malformed_iq_error, not_acceptable_iq_error, not_allowed_iq_error, not_authorized_iq_error,
    service_unavailable_iq_error,
};
use muc_admin::handle_muc_admin_iq;
use push::handle_push_iq;
use remote_destination::handle_sans_io_iq_with_remote_preflight as handle_sans_io_iq;
use roster::handle_roster_iq;
use search::{handle_channel_search_iq, handle_user_search_iq};
use vcard_private::{handle_private_storage_iq, handle_vcard_iq};

// `build_iq_error_xml_typed` is re-exported via `pub(super) use` so
// submodules wildcard-importing this module's namespace see it.
pub(super) use super::super::{build_iq_error_xml_typed, build_iq_error_xml_with_payload};

use super::super::{
    build_iq_result_xml, element_to_xml, get_room_actor, get_room_actor_result, iq_to_xml,
    is_muc_room_jid, stanza_to_xml, WebSocketState,
};
use super::presence::{
    get_managed_channel_for_room, resolve_muc_room_archive_access,
    send_current_presence_from_user_to_jid, send_unavailable_presence_from_user_to_jid,
    RoomArchiveAccess,
};
use crate::auth::{local_account_exists, local_account_jid_exists, Session};
use crate::db::actor::{DbExecute, DbQuery, DbQueryOne, GetDatabase};
use crate::db::blocking::DatabaseBlockingStorage;
use crate::db::roster::{
    DatabaseRosterStorage, RosterItemRow, RosterRowChange, RosterRowMutationKind,
    RosterStorageError,
};
use crate::db::{row_value, Database, Value, ValueExt};
use crate::permissions::{
    CheckPermission, Object, ObjectType, Permission, PermissionError, Relation, Subject,
    SubjectType, Tuple, WriteTuple,
};
use crate::server::bootstrap_membership::DEPLOYMENT_SERVER_ID;
use crate::server::managed_channel_policy::{
    server_policy_for_managed_channel, ManagedChannelServerPolicy,
    DEPLOYMENT_MEMBERSHIP_PERMISSIONS,
};
use crate::server::routes::websocket::ResolvedPrincipal;
use crate::server::xmpp_state::{get_xmpp_channel, list_xmpp_channels, XmppChannelRecord};
use crate::vcard::VCardStore;

#[cfg(test)]
mod tests;

mod dispatch;
pub use dispatch::handle_iq_with_conn_state;
use dispatch::push_service_stanza_error;
pub(super) use dispatch::response_batch_from_inline_room_effect_frames;
pub(super) use dispatch::IqHandlerContext;
