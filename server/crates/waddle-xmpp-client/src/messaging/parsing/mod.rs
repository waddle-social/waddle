//! Conversion of incoming message XML into structured client events.

mod files;
mod markup;
pub(super) mod payloads;

use std::borrow::Cow;

use chrono::{DateTime, Utc};
use minidom::Element;

use crate::xep::fallback::{body_fallbacks_for, strip_fallback_ranges, BodyFallback};
use crate::xep::{
    call_thread as xep_call_thread, reply as xep_reply, safety_scores as xep_safety_scores,
    thread as xep_thread,
};
use waddle_xmpp_core::xep0359::StanzaId as StableStanzaId;
use waddle_xmpp_core::{first_eligible_https_url_text, PreviewImageMediaType};

use self::files::{parse_file_sharing_element, parse_shared_file};
use self::markup::parse_markup_spans;
pub use self::payloads::{
    parse_chat_state_payload, parse_correction_payload, parse_displayed_marker_payload,
    parse_extension_envelope, parse_in_call_reaction_payload, parse_markable_marker_payload,
    parse_moderation_payload, parse_reaction_payload, parse_retraction_payload,
    parse_retraction_tombstone_payload,
};
use super::namespaces::{
    NS_CLIENT, NS_DELAY, NS_MARKUP, NS_MESSAGE_MODERATE, NS_MESSAGE_RETRACT, NS_MUC_USER,
    NS_OPENGRAPH, NS_OPENGRAPH_IMAGE, NS_OPENGRAPH_VIDEO, NS_ORIGIN_ID, NS_RDF_SYNTAX,
    NS_REFERENCES, NS_SFS, NS_SIMS, NS_STANZA_ID, NS_STICKERS, NS_WADDLE_EXTENSION,
};
use super::presence::parse_presence;
use super::types::{
    InCallSignal, InboundMessage, LinkPreviewData, LinkPreviewImageData, LinkPreviewPlayer,
    LinkPreviewVideoData, MdsDisplayedEntry, MessagingEvent, ReferenceData, SharedFile,
    SharedFileDisposition,
};
use url::Url;

mod message;
use message::body_without_reply_fallback;
use message::cached_preview_media_origin_allowed;
use message::first_eligible_https_url;
use message::has_moderation_retract;
use message::is_link_preview_xep0363_file_path;
use message::link_preview_image_url;
use message::og_image_text;
use message::og_text;
use message::og_video_type_essence;
pub use message::parse;
pub(super) use message::parse_cached_preview_image_url;
use message::parse_link_preview;
use message::parse_link_preview_image;
use message::parse_link_preview_native_video;
use message::parse_link_preview_player;
use message::parse_link_previews;
use message::parse_message;
use message::parse_web_url;
