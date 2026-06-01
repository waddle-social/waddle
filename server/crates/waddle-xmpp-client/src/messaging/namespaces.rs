pub(crate) const NS_DELAY: &str = "urn:xmpp:delay";
pub(crate) const NS_STANZA_ID: &str = "urn:xmpp:sid:0";
pub(crate) const NS_ORIGIN_ID: &str = "urn:xmpp:sid:0";
pub const NS_REACTIONS: &str = "urn:xmpp:reactions:0";
pub(crate) const NS_MARKUP: &str = "urn:xmpp:markup:0";
pub(crate) const NS_WADDLE_MARKUP: &str = "urn:waddle:markup:0";
pub const NS_CHAT_STATES: &str = "http://jabber.org/protocol/chatstates";
pub const NS_CHAT_MARKERS: &str = "urn:xmpp:chat-markers:0";
pub(crate) const NS_REFERENCES: &str = "urn:xmpp:reference:0";
pub const NS_MESSAGE_RETRACT: &str = "urn:xmpp:message-retract:1";
pub const NS_MESSAGE_MODERATE: &str = "urn:xmpp:message-moderate:1";
pub const NS_MESSAGE_CORRECT: &str = "urn:xmpp:message-correct:0";
pub(crate) const NS_HINTS: &str = "urn:xmpp:hints";
pub(crate) const NS_HATS: &str = "urn:xmpp:hats:0";
pub(crate) const NS_SIMS: &str = "urn:xmpp:sims:1";
pub(crate) const NS_SFS: &str = "urn:xmpp:sfs:0";
pub(crate) const NS_FILE_METADATA: &str = "urn:xmpp:file:metadata:0";
pub(crate) const NS_URL_DATA: &str = "http://jabber.org/protocol/url-data";
/// `jabber:client` — top-level namespace stamped on every C2S
/// stanza. `pub` so the wasm and Apple FFI crates can borrow the
/// canonical constant rather than re-declare it (typed-payloads
/// hard rule: namespaces live in dedicated constants, not at
/// call sites).
pub const NS_CLIENT: &str = "jabber:client";
pub(crate) const NS_WADDLE_EXTENSION: &str = "urn:waddle:extension:1";
pub(crate) const NS_WADDLE_LINK_PREVIEW: &str = "urn:waddle:link-preview:0";
pub(crate) const NS_RDF_SYNTAX: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#";
pub(crate) const NS_OPENGRAPH: &str = "https://ogp.me/ns#";
#[cfg(all(feature = "native", not(target_arch = "wasm32")))]
pub(crate) const NS_MUC: &str = "http://jabber.org/protocol/muc";
pub(crate) const NS_MUC_USER: &str = "http://jabber.org/protocol/muc#user";
pub(crate) const NS_STICKERS: &str = "urn:xmpp:stickers:0";
pub(crate) const NS_VCARD_UPDATE: &str = "vcard-temp:x:update";
pub const NS_WADDLE_PIN_V0: &str = "urn:waddle:pin:0";

/// `urn:xmpp:jingle:muji:0` — XEP-0272 Multiparty Jingle (Muji)
/// namespace. Used in MUC presence to advertise call participation
/// (`<muji><preparing/></muji>` while joining; `<muji><content/></muji>`
/// once contents are declared; absent when leaving).
///
/// Mirrors `waddle_xmpp::xep::xep0272::NS_MUJI` — the client crate
/// cannot depend on the server crate, so the constant is duplicated
/// and kept in sync via a test.
pub const NS_MUJI: &str = "urn:xmpp:jingle:muji:0";

/// `urn:xmpp:jingle:apps:rtp:1` — XEP-0167 RTP description namespace,
/// used inside Muji content declarations.
pub const NS_JINGLE_RTP: &str = "urn:xmpp:jingle:apps:rtp:1";

/// Build a `<muji xmlns='urn:xmpp:jingle:muji:0'>` element from typed
/// inputs. When `audio` or `video` is true, a corresponding
/// `<content creator='initiator' name='audio|video'>
///   <description xmlns='urn:xmpp:jingle:apps:rtp:1' media='…'/>
/// </content>` child is emitted (XEP-0272 §Joining content shape;
/// XEP-0167 §3.2 minimal `<description>`).
///
/// When `preparing` is true, a `<preparing/>` sentinel child is
/// emitted (XEP-0272 §Joining two-phase flow). An "empty"
/// `<muji/>` (no preparing, no contents) is XEP-0272 §Leaving — the
/// caller should typically omit the element entirely instead.
///
/// CLAUDE.md XML hard rule: callers never hand-roll the element via
/// raw `Element::builder` strings.
pub fn build_muji_element(preparing: bool, audio: bool, video: bool) -> minidom::Element {
    let mut builder = minidom::Element::builder("muji", NS_MUJI);
    if preparing {
        builder = builder.append(minidom::Element::builder("preparing", NS_MUJI).build());
    }
    if audio {
        builder = builder.append(build_muji_content("audio"));
    }
    if video {
        builder = builder.append(build_muji_content("video"));
    }
    builder.build()
}

fn build_muji_content(media: &str) -> minidom::Element {
    // XEP-0167 §3.1 / XEP-0272 §Joining examples include
    // `<payload-type/>` children inside `<description/>`. Emit the
    // canonical LiveKit-supported codecs (Opus for audio, VP8 for
    // video) so a strict Muji peer reading our MUC presence sees
    // a valid codec advertisement. The actual encoding selection
    // happens inside LiveKit below the XMPP signaling layer.
    let mut description = minidom::Element::builder("description", NS_JINGLE_RTP)
        .attr(minidom::rxml::xml_ncname!("media").to_owned(), media);
    match media {
        "audio" => {
            description = description.append(
                minidom::Element::builder("payload-type", NS_JINGLE_RTP)
                    .attr(minidom::rxml::xml_ncname!("id").to_owned(), "111")
                    .attr(minidom::rxml::xml_ncname!("name").to_owned(), "opus")
                    .attr(minidom::rxml::xml_ncname!("clockrate").to_owned(), "48000")
                    .attr(minidom::rxml::xml_ncname!("channels").to_owned(), "2")
                    .build(),
            );
        }
        "video" => {
            description = description.append(
                minidom::Element::builder("payload-type", NS_JINGLE_RTP)
                    .attr(minidom::rxml::xml_ncname!("id").to_owned(), "96")
                    .attr(minidom::rxml::xml_ncname!("name").to_owned(), "VP8")
                    .attr(minidom::rxml::xml_ncname!("clockrate").to_owned(), "90000")
                    .build(),
            );
        }
        _ => {}
    }
    minidom::Element::builder("content", NS_MUJI)
        .attr(
            minidom::rxml::xml_ncname!("creator").to_owned(),
            "initiator",
        )
        .attr(minidom::rxml::xml_ncname!("name").to_owned(), media)
        .append(description.build())
        .build()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_preparing_muji() {
        let elem = build_muji_element(true, false, false);
        assert_eq!(elem.name(), "muji");
        assert_eq!(elem.ns(), NS_MUJI);
        assert!(
            elem.children().any(|c| c.name() == "preparing"),
            "preparing child must be present"
        );
        assert!(
            !elem.children().any(|c| c.name() == "content"),
            "no content children in preparing-only Muji"
        );
    }

    #[test]
    fn builds_audio_video_muji_with_rtp_descriptions() {
        let elem = build_muji_element(false, true, true);
        let contents: Vec<_> = elem.children().filter(|c| c.name() == "content").collect();
        assert_eq!(contents.len(), 2, "audio + video content children");
        assert!(contents.iter().any(|c| c.attr("name") == Some("audio")));
        assert!(contents.iter().any(|c| c.attr("name") == Some("video")));
        // XEP-0167 minimal description (no payload-types — LiveKit
        // dictates codecs).
        for content in contents {
            let desc = content
                .children()
                .find(|c| c.name() == "description")
                .expect("description present");
            assert_eq!(desc.ns(), NS_JINGLE_RTP);
            assert!(matches!(desc.attr("media"), Some("audio") | Some("video")));
        }
    }

    /// Pin the duplicate constants against the canonical XEP namespaces.
    /// The server crate's `xep0272` module has a matching test against
    /// `xmpp_parsers` and the XEP-0272 wire shape.
    #[test]
    fn ns_constants_match_xep_namespaces() {
        assert_eq!(NS_MUJI, "urn:xmpp:jingle:muji:0");
        assert_eq!(NS_JINGLE_RTP, "urn:xmpp:jingle:apps:rtp:1");
    }
}
