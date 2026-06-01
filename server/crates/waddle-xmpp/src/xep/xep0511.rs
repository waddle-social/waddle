//! XEP-0511: Link Metadata
//!
//! Provides a typed view over the XEP-0511 RDF/OpenGraph message payload:
//! an `rdf:Description` element with a namespaced `rdf:about` attribute and
//! optional OpenGraph text properties.

use minidom::{
    rxml::{xml_ncname, Namespace},
    Element,
};
use thiserror::Error;
use url::Url;
use xmpp_parsers::message::Message;

/// RDF syntax namespace used by XEP-0511's `<rdf:Description/>` payload.
pub const NS_RDF_SYNTAX: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#";

/// OpenGraph namespace recommended by XEP-0511.
pub const NS_OPENGRAPH: &str = "https://ogp.me/ns#";

/// OpenGraph image structured-property namespace.
pub const NS_OPENGRAPH_IMAGE: &str = "https://ogp.me/ns#image:";

/// Errors that can occur while parsing XEP-0511 link metadata.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum LinkMetadataError {
    /// The root was not `<rdf:Description/>`.
    #[error("expected <Description/> in namespace '{NS_RDF_SYNTAX}'")]
    WrongRoot,
    /// The required namespaced `rdf:about` attribute is missing.
    #[error("missing rdf:about on XEP-0511 link metadata")]
    MissingAbout,
    /// A plain, non-RDF `about` attribute was supplied.
    #[error("XEP-0511 requires rdf:about, not a bare about attribute")]
    BareAbout,
    /// The `rdf:about` value was not a valid URL.
    #[error("invalid rdf:about URL")]
    InvalidAboutUrl,
    /// The `og:url` value was not a valid URL.
    #[error("invalid OpenGraph canonical URL")]
    InvalidCanonicalUrl,
}

/// OpenGraph preview image metadata carried inside XEP-0511.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinkPreviewImage {
    /// Waddle-controlled image URL clients may dereference.
    pub url: Url,
    /// Safe MIME type observed when Waddle cached the image.
    pub media_type: Option<String>,
    /// Image width in pixels, when available from metadata.
    pub width: Option<u32>,
    /// Image height in pixels, when available from metadata.
    pub height: Option<u32>,
    /// Human-readable alt text, when available.
    pub alt: Option<String>,
}

impl LinkPreviewImage {
    /// Create preview image metadata for a cached Waddle media URL.
    pub fn new(url: Url) -> Self {
        Self {
            url,
            media_type: None,
            width: None,
            height: None,
            alt: None,
        }
    }

    /// Set the safe MIME type.
    pub fn with_media_type(mut self, media_type: impl Into<String>) -> Self {
        self.media_type = Some(media_type.into());
        self
    }

    /// Set image dimensions.
    pub fn with_dimensions(mut self, width: u32, height: u32) -> Self {
        self.width = Some(width);
        self.height = Some(height);
        self
    }

    /// Set image alt text.
    pub fn with_alt(mut self, alt: impl Into<String>) -> Self {
        self.alt = Some(alt.into());
        self
    }
}

/// Typed plaintext subset of the OpenGraph metadata used by Waddle's first
/// XEP-0511 tracer bullet.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinkMetadata {
    /// IRI described by this payload (`rdf:about`).
    pub about: Url,
    /// `og:title`, when present.
    pub title: Option<String>,
    /// `og:description`, when present.
    pub description: Option<String>,
    /// `og:url`, when present.
    pub canonical_url: Option<Url>,
    /// `og:site_name`, when present.
    pub site_name: Option<String>,
    /// Cached preview images referenced through Waddle-controlled media URLs.
    pub images: Vec<LinkPreviewImage>,
}

impl LinkMetadata {
    /// Create link metadata for the described URL.
    pub fn new(about: Url) -> Self {
        Self {
            about,
            title: None,
            description: None,
            canonical_url: None,
            site_name: None,
            images: Vec::new(),
        }
    }

    /// Set `og:title`.
    pub fn with_title(mut self, title: impl Into<String>) -> Self {
        self.title = Some(title.into());
        self
    }

    /// Set `og:description`.
    pub fn with_description(mut self, description: impl Into<String>) -> Self {
        self.description = Some(description.into());
        self
    }

    /// Set `og:url`.
    pub fn with_canonical_url(mut self, canonical_url: Url) -> Self {
        self.canonical_url = Some(canonical_url);
        self
    }

    /// Set `og:site_name`.
    pub fn with_site_name(mut self, site_name: impl Into<String>) -> Self {
        self.site_name = Some(site_name.into());
        self
    }

    /// Add cached preview image metadata.
    pub fn with_image(mut self, image: LinkPreviewImage) -> Self {
        self.images.push(image);
        self
    }
}

/// Check if an element is an XEP-0511 `<rdf:Description/>` payload.
pub fn is_link_metadata_element(elem: &Element) -> bool {
    elem.name() == "Description" && elem.ns() == NS_RDF_SYNTAX
}

/// Parse one XEP-0511 `<rdf:Description/>` payload.
pub fn parse_link_metadata_element(elem: &Element) -> Result<LinkMetadata, LinkMetadataError> {
    if !is_link_metadata_element(elem) {
        return Err(LinkMetadataError::WrongRoot);
    }
    if has_bare_about(elem) {
        return Err(LinkMetadataError::BareAbout);
    }

    let about = elem
        .attr_ns(&Namespace::from(NS_RDF_SYNTAX), "about")
        .ok_or(LinkMetadataError::MissingAbout)
        .and_then(|raw| Url::parse(raw).map_err(|_| LinkMetadataError::InvalidAboutUrl))?;

    let mut metadata = LinkMetadata::new(about);
    metadata.title = og_text(elem, "title");
    metadata.description = og_text(elem, "description");
    metadata.site_name = og_text(elem, "site_name");
    metadata.canonical_url = match og_text(elem, "url") {
        Some(raw) => Some(Url::parse(&raw).map_err(|_| LinkMetadataError::InvalidCanonicalUrl)?),
        None => None,
    };
    metadata.images = parse_og_images(elem);

    Ok(metadata)
}

/// Extract all valid XEP-0511 link metadata payloads from a message.
///
/// Individual malformed payloads are ignored so foreign or legacy archive
/// data cannot make the entire message unreadable. Use
/// [`parse_link_metadata_element`] directly when strict conformance diagnostics
/// are required.
pub fn extract_link_metadata_from_message(msg: &Message) -> Vec<LinkMetadata> {
    msg.payloads
        .iter()
        .filter(|payload| is_link_metadata_element(payload))
        .filter_map(|payload| parse_link_metadata_element(payload).ok())
        .collect()
}

/// Build one XEP-0511 `<rdf:Description/>` payload.
pub fn build_link_metadata_element(metadata: &LinkMetadata) -> Element {
    let mut description = Element::builder("Description", NS_RDF_SYNTAX)
        .prefix(Some("rdf".to_string()), NS_RDF_SYNTAX)
        .expect("static RDF prefix is unique")
        .prefix(Some("og".to_string()), NS_OPENGRAPH)
        .expect("static OpenGraph prefix is unique")
        .prefix(Some("ogi".to_string()), NS_OPENGRAPH_IMAGE)
        .expect("static OpenGraph image prefix is unique")
        .attr_ns(
            Namespace::from(NS_RDF_SYNTAX),
            xml_ncname!("about").to_owned(),
            metadata.about.as_str(),
        )
        .build();

    append_og_text(&mut description, "title", metadata.title.as_deref());
    append_og_text(
        &mut description,
        "description",
        metadata.description.as_deref(),
    );
    append_og_text(
        &mut description,
        "url",
        metadata.canonical_url.as_ref().map(Url::as_str),
    );
    append_og_text(&mut description, "site_name", metadata.site_name.as_deref());
    for image in &metadata.images {
        append_og_text(&mut description, "image", Some(image.url.as_str()));
        append_og_image_text(&mut description, "type", image.media_type.as_deref());
        append_og_number(&mut description, "width", image.width);
        append_og_number(&mut description, "height", image.height);
        append_og_image_text(&mut description, "alt", image.alt.as_deref());
    }

    description
}

/// Add link metadata to a message.
pub fn set_link_metadata(msg: &mut Message, metadata: &[LinkMetadata]) {
    strip_link_metadata(msg);
    msg.payloads
        .extend(metadata.iter().map(build_link_metadata_element));
}

/// Remove XEP-0511 link metadata payloads from a message.
pub fn strip_link_metadata(msg: &mut Message) {
    msg.payloads
        .retain(|payload| !is_link_metadata_element(payload));
}

fn has_bare_about(elem: &Element) -> bool {
    elem.attrs()
        .iter()
        .any(|((ns, name), _)| ns.as_str().is_empty() && name.as_str() == "about")
}

fn og_text(elem: &Element, name: &str) -> Option<String> {
    elem.children()
        .find(|child| child.name() == name && child.ns() == NS_OPENGRAPH)
        .map(Element::text)
        .map(|text| text.trim().to_string())
        .filter(|text| !text.is_empty())
}

fn append_og_text(parent: &mut Element, name: &str, value: Option<&str>) {
    let Some(value) = value.filter(|value| !value.trim().is_empty()) else {
        return;
    };
    parent.append_child(
        Element::builder(name, NS_OPENGRAPH)
            .append(value.trim())
            .build(),
    );
}

fn parse_og_images(elem: &Element) -> Vec<LinkPreviewImage> {
    let mut images = Vec::new();
    let mut current: Option<LinkPreviewImage> = None;
    for child in elem.children() {
        if child.ns() == NS_OPENGRAPH && child.name() == "image" {
            if let Some(image) = current.take() {
                images.push(image);
            }
            current = Url::parse(child.text().trim())
                .ok()
                .map(LinkPreviewImage::new);
            continue;
        }
        if child.ns() != NS_OPENGRAPH_IMAGE {
            continue;
        }
        let Some(image) = current.as_mut() else {
            continue;
        };
        let value = child.text().trim().to_string();
        if value.is_empty() {
            continue;
        }
        match child.name() {
            "type" => image.media_type = Some(value),
            "width" => image.width = value.parse().ok(),
            "height" => image.height = value.parse().ok(),
            "alt" => image.alt = Some(value),
            _ => {}
        }
    }
    if let Some(image) = current {
        images.push(image);
    }
    images
}

fn append_og_number(parent: &mut Element, name: &str, value: Option<u32>) {
    let Some(value) = value else {
        return;
    };
    parent.append_child(
        Element::builder(name, NS_OPENGRAPH_IMAGE)
            .append(value.to_string())
            .build(),
    );
}

fn append_og_image_text(parent: &mut Element, name: &str, value: Option<&str>) {
    let Some(value) = value.filter(|value| !value.trim().is_empty()) else {
        return;
    };
    parent.append_child(
        Element::builder(name, NS_OPENGRAPH_IMAGE)
            .append(value.trim())
            .build(),
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn element(xml: &str) -> Element {
        xml.parse::<Element>().expect("valid xml")
    }

    #[test]
    fn parses_xep0511_description_with_namespaced_rdf_about() {
        let payload = element(
            r#"<rdf:Description xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#" xmlns:og="https://ogp.me/ns#" rdf:about="https://the.link.example.com/what-was-linked-to">
                <og:title>The Best Webpage</og:title>
                <og:description>This is a great webpage and you will really like it</og:description>
                <og:url>https://example.com/canonical-url/for/what-was-linked-to</og:url>
                <og:site_name>Example Website</og:site_name>
              </rdf:Description>"#,
        );

        let parsed = parse_link_metadata_element(&payload).expect("valid XEP-0511 metadata");

        assert_eq!(
            parsed.about.as_str(),
            "https://the.link.example.com/what-was-linked-to"
        );
        assert_eq!(parsed.title.as_deref(), Some("The Best Webpage"));
        assert_eq!(
            parsed.description.as_deref(),
            Some("This is a great webpage and you will really like it")
        );
        assert_eq!(
            parsed.canonical_url.as_ref().map(Url::as_str),
            Some("https://example.com/canonical-url/for/what-was-linked-to")
        );
        assert_eq!(parsed.site_name.as_deref(), Some("Example Website"));
    }

    #[test]
    fn rejects_bare_about_attribute() {
        let payload = element(
            r#"<rdf:Description xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#" about="https://example.com/">
                <title xmlns="https://ogp.me/ns#">Example</title>
              </rdf:Description>"#,
        );

        let err = parse_link_metadata_element(&payload).expect_err("bare about must be rejected");

        assert_eq!(err, LinkMetadataError::BareAbout);
    }

    #[test]
    fn builds_conformant_metadata_with_rdf_about() {
        let metadata = LinkMetadata::new(Url::parse("https://example.com/original").expect("url"))
            .with_title("Example")
            .with_description("Plain text preview")
            .with_canonical_url(Url::parse("https://example.com/canonical").expect("url"));

        let elem = build_link_metadata_element(&metadata);
        let parsed = parse_link_metadata_element(&elem).expect("built metadata parses");

        assert_eq!(parsed, metadata);
        assert_eq!(
            elem.attr_ns(&Namespace::from(NS_RDF_SYNTAX), "about"),
            Some("https://example.com/original")
        );
        assert_eq!(elem.attr("about"), None);
    }
}
