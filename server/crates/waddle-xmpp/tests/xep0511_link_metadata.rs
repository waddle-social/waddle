//! XEP-0511: Link Metadata — dedicated conformance suite.
//!
//! Pins Waddle's public XEP-0511 surface to the RDF/OpenGraph wire
//! shape that clients render from live messages and MAM replay.

use minidom::Element;
use url::Url;
use waddle_xmpp::xep::{
    build_link_metadata_element, extract_link_metadata_from_message, parse_link_metadata_element,
    set_link_metadata, strip_link_metadata, LinkMetadata, LinkMetadataError, LinkMetadataVideo,
    LinkPreviewImage, NS_OPENGRAPH, NS_OPENGRAPH_IMAGE, NS_OPENGRAPH_VIDEO, NS_RDF_SYNTAX,
};
use waddle_xmpp_core::{DirectVideoMediaType, PreviewImageMediaType};
use xmpp_parsers::message::Message;

fn element(xml: &str) -> Element {
    xml.parse().expect("valid xml")
}

#[test]
fn xep0511_builds_rdf_description_with_namespaced_about_and_opengraph_children() {
    let metadata = LinkMetadata::new(Url::parse("https://the.link.example/article").expect("url"))
        .with_title("The Best Webpage")
        .with_description("Plain text preview")
        .with_canonical_url(Url::parse("https://example.com/canonical").expect("url"))
        .with_site_name("Example");

    let payload = build_link_metadata_element(&metadata);

    assert_eq!(payload.name(), "Description");
    assert_eq!(payload.ns(), NS_RDF_SYNTAX);
    assert_eq!(
        payload.attr_ns(&minidom::rxml::Namespace::from(NS_RDF_SYNTAX), "about"),
        Some("https://the.link.example/article")
    );
    assert_eq!(
        payload
            .get_child("title", NS_OPENGRAPH)
            .map(Element::text)
            .as_deref(),
        Some("The Best Webpage")
    );
    assert_eq!(
        payload
            .get_child("description", NS_OPENGRAPH)
            .map(Element::text)
            .as_deref(),
        Some("Plain text preview")
    );
    assert_eq!(
        payload
            .get_child("url", NS_OPENGRAPH)
            .map(Element::text)
            .as_deref(),
        Some("https://example.com/canonical")
    );
}

#[test]
fn xep0511_builds_cached_image_metadata_with_opengraph_structured_properties() {
    let image = LinkPreviewImage::new(
        Url::parse("https://waddle.example/api/files/11111111-1111-4111-8111-111111111111/link-preview-86610c40efe63f0a46c58c4b605c164b4ffa3a3ad3f1dcf13e6ba4c59cb3ce16.png").expect("url"),
    )
    .with_media_type(PreviewImageMediaType::Png)
    .with_dimensions(640, 360)
    .with_alt("Screenshot of the article");
    let metadata = LinkMetadata::new(Url::parse("https://the.link.example/article").expect("url"))
        .with_title("The Best Webpage")
        .with_image(image.clone());

    let payload = build_link_metadata_element(&metadata);

    assert_eq!(
        payload
            .get_child("image", NS_OPENGRAPH)
            .map(Element::text)
            .as_deref(),
        Some("https://waddle.example/api/files/11111111-1111-4111-8111-111111111111/link-preview-86610c40efe63f0a46c58c4b605c164b4ffa3a3ad3f1dcf13e6ba4c59cb3ce16.png")
    );
    assert_eq!(
        payload
            .get_child("type", NS_OPENGRAPH_IMAGE)
            .map(Element::text)
            .as_deref(),
        Some("image/png")
    );
    assert_eq!(
        payload
            .get_child("width", NS_OPENGRAPH_IMAGE)
            .map(Element::text)
            .as_deref(),
        Some("640")
    );
    assert_eq!(
        payload
            .get_child("height", NS_OPENGRAPH_IMAGE)
            .map(Element::text)
            .as_deref(),
        Some("360")
    );
    assert_eq!(
        payload
            .get_child("alt", NS_OPENGRAPH_IMAGE)
            .map(Element::text)
            .as_deref(),
        Some("Screenshot of the article")
    );
    assert_eq!(
        parse_link_metadata_element(&payload)
            .expect("image metadata parses")
            .images,
        vec![image]
    );
}

#[test]
fn xep0511_builds_native_og_video_with_real_media_type() {
    let metadata =
        LinkMetadata::new(Url::parse("https://rawkode.academy/watch/yoke").expect("url"))
            .with_title("Hands-on Yoke")
            .with_video(LinkMetadataVideo::Native {
                url: Url::parse("https://content.rawkode.academy/v/clip.mp4").expect("url"),
                media_type: DirectVideoMediaType::Mp4,
            });

    let payload = build_link_metadata_element(&metadata);

    // Conformant OpenGraph: og:video URL + ogv:secure_url + the real ogv:type.
    assert_eq!(
        payload
            .get_child("video", NS_OPENGRAPH)
            .map(Element::text)
            .as_deref(),
        Some("https://content.rawkode.academy/v/clip.mp4")
    );
    assert_eq!(
        payload
            .get_child("secure_url", NS_OPENGRAPH_VIDEO)
            .map(Element::text)
            .as_deref(),
        Some("https://content.rawkode.academy/v/clip.mp4")
    );
    assert_eq!(
        payload
            .get_child("type", NS_OPENGRAPH_VIDEO)
            .map(Element::text)
            .as_deref(),
        Some("video/mp4")
    );

    assert_eq!(
        parse_link_metadata_element(&payload)
            .expect("native video round-trips")
            .video,
        metadata.video
    );
}

#[test]
fn xep0511_builds_player_og_video_with_text_html_type() {
    let metadata = LinkMetadata::new(Url::parse("https://www.youtube.com/watch?v=x").expect("url"))
        .with_video(LinkMetadataVideo::Player {
            url: Url::parse("https://www.youtube-nocookie.com/embed/x").expect("url"),
            width: Some(1280),
            height: Some(720),
        });

    let payload = build_link_metadata_element(&metadata);

    assert_eq!(
        payload
            .get_child("type", NS_OPENGRAPH_VIDEO)
            .map(Element::text)
            .as_deref(),
        Some("text/html")
    );
    assert_eq!(
        parse_link_metadata_element(&payload)
            .expect("player round-trips")
            .video,
        metadata.video
    );
}

#[test]
fn xep0511_parses_og_video_type_essence_ignoring_parameters() {
    // A conformant (e.g. federated) sender may parameterise og:video:type; the
    // wire parser must match on the MIME essence for both the native and player
    // discriminations.
    let native = element(
        "<rdf:Description xmlns:rdf='http://www.w3.org/1999/02/22-rdf-syntax-ns#' xmlns:og='https://ogp.me/ns#' xmlns:ogv='https://ogp.me/ns#video:' rdf:about='https://rawkode.academy/watch/yoke'>\
           <og:video>https://content.rawkode.academy/v/clip.mp4</og:video>\
           <ogv:type>video/mp4; codecs=\"avc1.42E01E\"</ogv:type>\
         </rdf:Description>",
    );
    assert_eq!(
        parse_link_metadata_element(&native).expect("parses").video,
        Some(LinkMetadataVideo::Native {
            url: Url::parse("https://content.rawkode.academy/v/clip.mp4").expect("url"),
            media_type: DirectVideoMediaType::Mp4,
        })
    );

    let player = element(
        "<rdf:Description xmlns:rdf='http://www.w3.org/1999/02/22-rdf-syntax-ns#' xmlns:og='https://ogp.me/ns#' xmlns:ogv='https://ogp.me/ns#video:' rdf:about='https://www.youtube.com/watch?v=x'>\
           <og:video>https://www.youtube-nocookie.com/embed/x</og:video>\
           <ogv:type>text/html; charset=utf-8</ogv:type>\
         </rdf:Description>",
    );
    assert!(matches!(
        parse_link_metadata_element(&player).expect("parses").video,
        Some(LinkMetadataVideo::Player { .. })
    ));
}

#[test]
fn xep0511_parse_rejects_bare_or_missing_about() {
    let bare_about = element(
        "<rdf:Description xmlns:rdf='http://www.w3.org/1999/02/22-rdf-syntax-ns#' xmlns:og='https://ogp.me/ns#' about='https://example.com/a'>\
           <og:title>Example</og:title>\
         </rdf:Description>",
    );
    assert!(matches!(
        parse_link_metadata_element(&bare_about),
        Err(LinkMetadataError::BareAbout)
    ));

    let missing_about = element(
        "<rdf:Description xmlns:rdf='http://www.w3.org/1999/02/22-rdf-syntax-ns#' xmlns:og='https://ogp.me/ns#'>\
           <og:title>Example</og:title>\
         </rdf:Description>",
    );
    assert!(matches!(
        parse_link_metadata_element(&missing_about),
        Err(LinkMetadataError::MissingAbout)
    ));
}

#[test]
fn xep0511_message_helpers_round_trip_and_strip_metadata_payloads() {
    let mut message = Message::new(None::<jid::Jid>);
    let metadata =
        LinkMetadata::new(Url::parse("https://example.com/a").expect("url")).with_title("Example");

    set_link_metadata(&mut message, std::slice::from_ref(&metadata));

    let extracted = extract_link_metadata_from_message(&message);
    assert_eq!(extracted, vec![metadata]);

    strip_link_metadata(&mut message);
    assert!(extract_link_metadata_from_message(&message).is_empty());
}

#[test]
fn xep0511_message_extraction_skips_malformed_metadata_payloads() {
    let mut message = Message::new(None::<jid::Jid>);
    let valid =
        LinkMetadata::new(Url::parse("https://example.com/valid").expect("url")).with_title("OK");
    message.payloads.push(element(
        "<rdf:Description xmlns:rdf='http://www.w3.org/1999/02/22-rdf-syntax-ns#' xmlns:og='https://ogp.me/ns#' about='https://example.com/bare'>\
           <og:title>Invalid</og:title>\
         </rdf:Description>",
    ));
    message.payloads.push(build_link_metadata_element(&valid));

    assert_eq!(extract_link_metadata_from_message(&message), vec![valid]);
}
