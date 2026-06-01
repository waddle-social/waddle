//! XEP-0511: Link Metadata — dedicated conformance suite.
//!
//! Pins Waddle's public XEP-0511 surface to the RDF/OpenGraph wire
//! shape that clients render from live messages and MAM replay.

use minidom::Element;
use url::Url;
use waddle_xmpp::xep::{
    build_link_metadata_element, extract_link_metadata_from_message, parse_link_metadata_element,
    set_link_metadata, strip_link_metadata, LinkMetadata, LinkMetadataError, LinkPreviewImage,
    NS_OPENGRAPH, NS_OPENGRAPH_IMAGE, NS_RDF_SYNTAX,
};
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
        Url::parse("https://waddle.example/api/link-preview-media/sha256/86610c40efe63f0a46c58c4b605c164b4ffa3a3ad3f1dcf13e6ba4c59cb3ce16").expect("url"),
    )
    .with_media_type("image/png")
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
        Some("https://waddle.example/api/link-preview-media/sha256/86610c40efe63f0a46c58c4b605c164b4ffa3a3ad3f1dcf13e6ba4c59cb3ce16")
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
