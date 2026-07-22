//! XEP-0449: Stickers — client-side sticker-pack model.
//!
//! Covers the pack pubsub-item wire shape (build + strict parse), the
//! §"Sticker pack hash calculation" pack-id derivation, the PEP-node
//! IQ builders (items fetch, publish, retract), and the `<sticker/>`
//! message marker attached to outbound sends.
//!
//! The pack payload composes three XEPs: XEP-0446 file metadata (with
//! the mandatory lang-less `<desc/>` and ≥1 XEP-0300
//! `urn:xmpp:hashes:2` `<hash/>`), XEP-0447 `<sources/>`, and the
//! XEP-0449 `<pack/>` envelope with a trailing pack-level `<hash/>`.
//!
//! NOTE on the XEP example vector: the "Publish a new sticker pack"
//! example in xep-0449.xml elides pack items (`<!-- ... -->`), so its
//! literal hash `EpRv28DHHzFrE4zd+xaNpVb4jbu4s74XtioExNjQzZ0=` cannot
//! be reproduced from the visible content by any implementation of the
//! §"Sticker pack hash calculation" prose (verified against an
//! independent reference implementation and the moxxmpp interpretation
//! of the same prose). The tests instead pin a vector computed from
//! the example's *visible* content with this module's spec-faithful
//! algorithm, cross-checked against an out-of-tree Python reference.

use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use base64::Engine;
use jid::BareJid;
use minidom::Element;
use sha2::{Digest, Sha256};

use crate::pep::NS_PUBSUB;

const NS_CLIENT: &str = "jabber:client";
pub(crate) const NS_FILE_METADATA: &str = "urn:xmpp:file:metadata:0";
pub(crate) const NS_SFS: &str = "urn:xmpp:sfs:0";
pub(crate) const NS_URL_DATA: &str = "http://jabber.org/protocol/url-data";

/// XEP-0449 registrar namespace. Per the XEP, the same string is also
/// the PEP node id (`STICKERS_NODE`). Mirrors
/// `waddle_xmpp_core::pubsub::pep::PEP_NODE_STICKERS` — the client
/// crate cannot depend on the server crates' wire layer, so the
/// constant is duplicated and pinned equal by tests.
pub const NS_STICKERS: &str = "urn:xmpp:stickers:0";
/// The user PEP node sticker packs live on (XEP-0449 §create).
pub const STICKERS_NODE: &str = "urn:xmpp:stickers:0";
/// XEP-0300 hashes namespace used by pack items and the pack hash.
pub const NS_HASHES: &str = "urn:xmpp:hashes:2";

/// XEP-0300 hash algorithm carried on `<hash algo='…'/>`. Only
/// `sha-256` in v1 — it is XEP-0300's MUST-implement baseline and the
/// only algorithm Waddle publishes; unknown wire values are dropped at
/// the parse boundary rather than smuggled through as strings.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HashAlgo {
    Sha256,
}

impl HashAlgo {
    pub fn as_str(self) -> &'static str {
        match self {
            HashAlgo::Sha256 => "sha-256",
        }
    }

    pub fn from_attr(value: &str) -> Option<Self> {
        match value {
            "sha-256" => Some(HashAlgo::Sha256),
            _ => None,
        }
    }
}

/// One XEP-0300 `<hash xmlns='urn:xmpp:hashes:2'/>` value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StickerHash {
    pub algo: HashAlgo,
    /// Base64 text content of the `<hash/>` element.
    pub value_b64: String,
}

/// A localized text entry (`<name/>`, `<summary/>`, `<suggest/>`).
/// `lang` is the `xml:lang` attribute; `None` means the element
/// carries no language tag.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StickerText {
    pub lang: Option<String>,
    pub text: String,
}

impl StickerText {
    pub fn plain(text: impl Into<String>) -> Self {
        Self {
            lang: None,
            text: text.into(),
        }
    }
}

/// XEP-0446 pixel dimensions of a sticker image. The current XEP-0446
/// wire shape is `<width/>`/`<height/>`; the legacy single
/// `<dimensions>WxH</dimensions>` form used by older XEP-0449 examples
/// is accepted on parse and normalized into this pair.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StickerDimensions {
    pub width: u32,
    pub height: u32,
}

/// One sticker inside a pack: the XEP-0446 `<file/>` metadata plus the
/// XEP-0447 `<sources/>` and XEP-0449 `<suggest/>` children.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackSticker {
    /// Mandatory lang-less textual fallback (usually emoji).
    pub desc: String,
    pub media_type: Option<String>,
    pub size: Option<u64>,
    pub dimensions: Option<StickerDimensions>,
    /// ≥1 per XEP-0449; all stickers in a pack MUST share one algorithm.
    pub hashes: Vec<StickerHash>,
    /// XEP-0447 `<url-data target='…'/>` source URLs.
    pub sources: Vec<String>,
    pub suggests: Vec<StickerText>,
}

/// Sticker-pack id: the pubsub item id, derived from the first 144
/// bits (24 Base64 characters) of the pack hash. Imported packs keep
/// their original id, so any non-empty token is representable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackId(String);

impl PackId {
    pub fn new(value: impl Into<String>) -> Option<Self> {
        let value = value.into();
        if value.trim().is_empty() {
            return None;
        }
        Some(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for PackId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// A full sticker pack as stored on the pubsub node: one `<item/>`
/// whose id is the pack id and whose payload is the `<pack/>` element.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StickerPackDoc {
    pub id: PackId,
    /// `<name/>` entries; multiple entries differ by `xml:lang`.
    pub name: Vec<StickerText>,
    /// `<summary/>` entries; multiple entries differ by `xml:lang`.
    pub summary: Vec<StickerText>,
    /// `<restricted/>` marker: the pack may not be imported by others.
    pub restricted: bool,
    pub stickers: Vec<PackSticker>,
    /// Trailing pack-level `<hash/>` as seen on the wire. `None` only
    /// for locally assembled docs that have not been serialized yet;
    /// [`build_pack_element`] always emits the recomputed hash.
    pub pack_hash: Option<StickerHash>,
}

// ── Pack hash calculation (XEP-0449 §"Sticker pack hash calculation") ────────

const SEP_PACK: u8 = 0x1c; // ASCII File Separator (level 1)
const SEP_ITEM: u8 = 0x1d; // ASCII Group Separator (level 2)
const SEP_RECORD: u8 = 0x1e; // ASCII Record Separator (level 3)
const SEP_UNIT: u8 = 0x1f; // ASCII Unit Separator (level 4)

fn meta_component(element_name: &str, entry: &StickerText) -> Vec<u8> {
    // Per element: element name, xml:lang value, content — each with a
    // trailing 0x1f — then a closing 0x1e. An absent xml:lang encodes
    // as the empty octet string.
    let mut out = Vec::new();
    out.extend_from_slice(element_name.as_bytes());
    out.push(SEP_UNIT);
    out.extend_from_slice(entry.lang.as_deref().unwrap_or("").as_bytes());
    out.push(SEP_UNIT);
    out.extend_from_slice(entry.text.as_bytes());
    out.push(SEP_UNIT);
    out.push(SEP_RECORD);
    out
}

fn item_component(sticker: &PackSticker) -> Vec<u8> {
    // Per item: the file's <desc/> content plus 0x1e, then each
    // <hash/> as algo + 0x1f + content + 0x1f + 0x1e, hashes ordered
    // lesser to greater, then a closing 0x1d.
    let mut out = Vec::new();
    out.extend_from_slice(sticker.desc.as_bytes());
    out.push(SEP_RECORD);
    let mut hash_parts: Vec<Vec<u8>> = sticker
        .hashes
        .iter()
        .map(|hash| {
            let mut part = Vec::new();
            part.extend_from_slice(hash.algo.as_str().as_bytes());
            part.push(SEP_UNIT);
            part.extend_from_slice(hash.value_b64.as_bytes());
            part.push(SEP_UNIT);
            part.push(SEP_RECORD);
            part
        })
        .collect();
    hash_parts.sort();
    for part in hash_parts {
        out.extend_from_slice(&part);
    }
    out.push(SEP_ITEM);
    out
}

fn pack_hash_input(
    name: &[StickerText],
    summary: &[StickerText],
    stickers: &[PackSticker],
) -> Vec<u8> {
    let mut meta_parts: Vec<Vec<u8>> = name
        .iter()
        .map(|entry| meta_component("name", entry))
        .chain(summary.iter().map(|entry| meta_component("summary", entry)))
        .collect();
    meta_parts.sort();
    let mut input = Vec::new();
    for part in meta_parts {
        input.extend_from_slice(&part);
    }
    input.push(SEP_PACK);

    let mut item_parts: Vec<Vec<u8>> = stickers.iter().map(item_component).collect();
    item_parts.sort();
    for part in item_parts {
        input.extend_from_slice(&part);
    }
    input.push(SEP_PACK);
    input
}

/// Compute the pack-level `<hash/>` value (full Base64 SHA-256 digest)
/// over the pack's significant content.
pub fn compute_pack_hash(
    name: &[StickerText],
    summary: &[StickerText],
    stickers: &[PackSticker],
) -> StickerHash {
    let digest = Sha256::digest(pack_hash_input(name, summary, stickers));
    StickerHash {
        algo: HashAlgo::Sha256,
        value_b64: BASE64_STANDARD.encode(digest),
    }
}

/// Derive the pack id: the first 144 bits of the pack hash, Base64 —
/// equivalently the first 24 characters of the full hash's Base64.
pub fn compute_pack_id(
    name: &[StickerText],
    summary: &[StickerText],
    stickers: &[PackSticker],
) -> PackId {
    let full = compute_pack_hash(name, summary, stickers);
    PackId(full.value_b64.chars().take(24).collect())
}

// ── Pack element build / parse ───────────────────────────────────────────────

/// Build one XEP-0300 `<hash xmlns='urn:xmpp:hashes:2'/>` element.
pub fn build_hash_element(hash: &StickerHash) -> Element {
    Element::builder("hash", NS_HASHES)
        .attr(
            minidom::rxml::xml_ncname!("algo").to_owned(),
            hash.algo.as_str(),
        )
        .append(hash.value_b64.as_str())
        .build()
}

pub(crate) fn parse_file_hash_element(element: &Element) -> Option<StickerHash> {
    if !element.is("hash", NS_HASHES) {
        return None;
    }
    let algo = HashAlgo::from_attr(element.attr("algo")?)?;
    let value_b64 = element.text().trim().to_string();
    if value_b64.is_empty() {
        return None;
    }
    Some(StickerHash { algo, value_b64 })
}

fn append_localized(
    builder: minidom::ElementBuilder,
    name: &str,
    entries: &[StickerText],
) -> minidom::ElementBuilder {
    let mut builder = builder;
    for entry in entries {
        let mut child = Element::builder(name, NS_STICKERS);
        if let Some(lang) = entry.lang.as_deref() {
            child = child.attr_ns(
                minidom::rxml::Namespace::XML,
                minidom::rxml::xml_ncname!("lang").to_owned(),
                lang,
            );
        }
        builder = builder.append(child.append(entry.text.as_str()).build());
    }
    builder
}

fn build_pack_sticker_element(sticker: &PackSticker) -> Element {
    let mut file = Element::builder("file", NS_FILE_METADATA).build();
    if let Some(media_type) = sticker.media_type.as_deref() {
        file.append_child(
            Element::builder("media-type", NS_FILE_METADATA)
                .append(media_type)
                .build(),
        );
    }
    // XEP-0449: the mandatory textual fallback, never xml:lang-tagged.
    file.append_child(
        Element::builder("desc", NS_FILE_METADATA)
            .append(sticker.desc.as_str())
            .build(),
    );
    if let Some(size) = sticker.size {
        file.append_child(
            Element::builder("size", NS_FILE_METADATA)
                .append(size.to_string())
                .build(),
        );
    }
    if let Some(dimensions) = sticker.dimensions {
        // Current XEP-0446 wire shape (width/height elements); the
        // legacy `<dimensions>WxH</dimensions>` form is parse-only.
        file.append_child(
            Element::builder("width", NS_FILE_METADATA)
                .append(dimensions.width.to_string())
                .build(),
        );
        file.append_child(
            Element::builder("height", NS_FILE_METADATA)
                .append(dimensions.height.to_string())
                .build(),
        );
    }
    for hash in &sticker.hashes {
        file.append_child(build_hash_element(hash));
    }

    let mut sources = Element::builder("sources", NS_SFS).build();
    for url in &sticker.sources {
        sources.append_child(
            Element::builder("url-data", NS_URL_DATA)
                .attr(
                    minidom::rxml::xml_ncname!("target").to_owned(),
                    url.as_str(),
                )
                .build(),
        );
    }

    let item = Element::builder("item", NS_STICKERS)
        .append(file)
        .append(sources);
    append_localized(item, "suggest", &sticker.suggests).build()
}

/// Build the `<pack xmlns='urn:xmpp:stickers:0'/>` payload. The
/// trailing pack-level `<hash/>` is always recomputed from the pack's
/// significant content so a published pack can never carry a hash that
/// disagrees with its own items.
pub fn build_pack_element(doc: &StickerPackDoc) -> Element {
    let builder = Element::builder("pack", NS_STICKERS);
    let builder = append_localized(builder, "name", &doc.name);
    let mut builder = append_localized(builder, "summary", &doc.summary);
    if doc.restricted {
        builder = builder.append(Element::builder("restricted", NS_STICKERS).build());
    }
    for sticker in &doc.stickers {
        builder = builder.append(build_pack_sticker_element(sticker));
    }
    let hash = compute_pack_hash(&doc.name, &doc.summary, &doc.stickers);
    builder.append(build_hash_element(&hash)).build()
}

fn parse_localized(element: &Element, name: &str) -> Vec<StickerText> {
    element
        .children()
        .filter(|child| child.is(name, NS_STICKERS))
        .map(|child| StickerText {
            lang: child
                .attr_ns(&minidom::rxml::Namespace::XML, "lang")
                .map(str::to_string),
            text: child.text(),
        })
        .collect()
}

fn parse_legacy_dimensions(value: &str) -> Option<StickerDimensions> {
    let (width, height) = value.trim().split_once('x')?;
    Some(StickerDimensions {
        width: width.parse().ok()?,
        height: height.parse().ok()?,
    })
}

fn parse_pack_sticker(item: &Element) -> Option<PackSticker> {
    let file = item.get_child("file", NS_FILE_METADATA)?;
    // XEP-0449: exactly one lang-less <desc/> is mandatory; a desc that
    // only exists in some language is not the required fallback.
    let desc = file
        .children()
        .find(|child| {
            child.is("desc", NS_FILE_METADATA)
                && child
                    .attr_ns(&minidom::rxml::Namespace::XML, "lang")
                    .is_none()
        })
        .map(Element::text)?;
    let hashes: Vec<StickerHash> = file
        .children()
        .filter_map(parse_file_hash_element)
        .collect();
    if hashes.is_empty() {
        // XEP-0449: the metadata MUST include ≥1 <hash/>.
        return None;
    }
    let dimensions = match (
        file.get_child("width", NS_FILE_METADATA)
            .and_then(|e| e.text().parse().ok()),
        file.get_child("height", NS_FILE_METADATA)
            .and_then(|e| e.text().parse().ok()),
    ) {
        (Some(width), Some(height)) => Some(StickerDimensions { width, height }),
        _ => file
            .get_child("dimensions", NS_FILE_METADATA)
            .and_then(|e| parse_legacy_dimensions(&e.text())),
    };
    let sources = item
        .get_child("sources", NS_SFS)
        .map(|sources| {
            sources
                .children()
                .filter(|child| child.is("url-data", NS_URL_DATA))
                .filter_map(|child| child.attr("target"))
                .map(str::to_string)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    if sources.is_empty() {
        // XEP-0449: each item has a <sources/> element describing how
        // to retrieve the image; an unfetchable sticker is useless.
        return None;
    }
    Some(PackSticker {
        desc,
        media_type: file
            .get_child("media-type", NS_FILE_METADATA)
            .map(|e| e.text()),
        size: file
            .get_child("size", NS_FILE_METADATA)
            .and_then(|e| e.text().parse().ok()),
        dimensions,
        hashes,
        sources,
        suggests: parse_localized(item, "suggest"),
    })
}

/// Parse a `<pack xmlns='urn:xmpp:stickers:0'/>` payload with the
/// given pubsub item id. Malformed stickers (no lang-less desc, no
/// hash, no source) are dropped; a pack with zero remaining stickers
/// is rejected outright (XEP-0449: "one or more" items).
pub fn parse_pack_element(id: PackId, pack: &Element) -> Option<StickerPackDoc> {
    if !pack.is("pack", NS_STICKERS) {
        return None;
    }
    let stickers: Vec<PackSticker> = pack
        .children()
        .filter(|child| child.is("item", NS_STICKERS))
        .filter_map(parse_pack_sticker)
        .collect();
    if stickers.is_empty() {
        return None;
    }
    Some(StickerPackDoc {
        id,
        name: parse_localized(pack, "name"),
        summary: parse_localized(pack, "summary"),
        restricted: pack.get_child("restricted", NS_STICKERS).is_some(),
        stickers,
        // The pack-level hash is the LAST direct <hash/> child; the
        // per-sticker hashes live inside <file/> and are not direct
        // children of <pack/>.
        pack_hash: pack.children().filter_map(parse_file_hash_element).last(),
    })
}

/// Parse one pubsub `<item id='…'/>` (in `ns`) into a pack document.
pub fn parse_pack_item(item: &Element, ns: &str) -> Option<StickerPackDoc> {
    if !item.is("item", ns) {
        return None;
    }
    let id = PackId::new(item.attr("id")?)?;
    let pack = item.get_child("pack", NS_STICKERS)?;
    parse_pack_element(id, pack)
}

/// Parse an `<iq type='result'/>` for a sticker-node items query into
/// pack documents. `node` guards against a mismatched reply payload.
pub fn parse_sticker_packs_result(iq: &Element, node: &str) -> Vec<StickerPackDoc> {
    let Some(items) = iq
        .get_child("pubsub", NS_PUBSUB)
        .and_then(|pubsub| pubsub.get_child("items", NS_PUBSUB))
        .filter(|items| items.attr("node") == Some(node))
    else {
        return Vec::new();
    };
    items
        .children()
        .filter_map(|child| parse_pack_item(child, NS_PUBSUB))
        .collect()
}

// ── IQ builders ──────────────────────────────────────────────────────────────

fn build_items_iq(iq_id: &str, target: Option<&BareJid>, items: Element) -> Element {
    let mut builder = Element::builder("iq", NS_CLIENT)
        .attr(minidom::rxml::xml_ncname!("type").to_owned(), "get")
        .attr(minidom::rxml::xml_ncname!("id").to_owned(), iq_id);
    if let Some(target) = target {
        builder = builder.attr(
            minidom::rxml::xml_ncname!("to").to_owned(),
            target.to_string(),
        );
    }
    builder
        .append(Element::builder("pubsub", NS_PUBSUB).append(items).build())
        .build()
}

/// XEP-0060 §6.5.2 items GET for the `urn:xmpp:stickers:0` node.
/// `target: None` routes to the user's own PEP service (XEP-0163);
/// `Some(owner)` fetches another user's published packs.
pub fn build_sticker_packs_items_iq(iq_id: &str, target: Option<&BareJid>) -> Element {
    let items = Element::builder("items", NS_PUBSUB)
        .attr(minidom::rxml::xml_ncname!("node").to_owned(), STICKERS_NODE)
        .build();
    build_items_iq(iq_id, target, items)
}

/// XEP-0060 §6.5.8 items-by-id GET for a single pack (`<item id/>`
/// child). `node` allows packs hosted on a generic pubsub node other
/// than the PEP `urn:xmpp:stickers:0` default (XEP-0449 §send-from-pack
/// `jid`/`node` sticker attributes).
pub fn build_sticker_pack_items_iq(
    iq_id: &str,
    target: &BareJid,
    node: &str,
    pack_id: &PackId,
) -> Element {
    let items = Element::builder("items", NS_PUBSUB)
        .attr(minidom::rxml::xml_ncname!("node").to_owned(), node)
        .append(
            Element::builder("item", NS_PUBSUB)
                .attr(
                    minidom::rxml::xml_ncname!("id").to_owned(),
                    pack_id.as_str(),
                )
                .build(),
        )
        .build();
    build_items_iq(iq_id, Some(target), items)
}

/// XEP-0449 publish IQ to the user's own PEP sticker node. The pubsub
/// item id is the PACK ID derived from the pack content — never the
/// single-slot `current` idiom — and is recomputed here so the id can
/// never disagree with the published payload. Returns the id actually
/// used alongside the stanza.
pub fn build_pack_publish_iq(iq_id: &str, doc: &StickerPackDoc) -> (PackId, Element) {
    let id = compute_pack_id(&doc.name, &doc.summary, &doc.stickers);
    let canonical = StickerPackDoc {
        id: id.clone(),
        ..doc.clone()
    };
    let item = Element::builder("item", NS_PUBSUB)
        .attr(minidom::rxml::xml_ncname!("id").to_owned(), id.as_str())
        .append(build_pack_element(&canonical))
        .build();
    let publish = Element::builder("publish", NS_PUBSUB)
        .attr(minidom::rxml::xml_ncname!("node").to_owned(), STICKERS_NODE)
        .append(item)
        .build();
    let iq = Element::builder("iq", NS_CLIENT)
        .attr(minidom::rxml::xml_ncname!("type").to_owned(), "set")
        .attr(minidom::rxml::xml_ncname!("id").to_owned(), iq_id)
        .append(
            Element::builder("pubsub", NS_PUBSUB)
                .append(publish)
                .build(),
        )
        .build();
    (id, iq)
}

/// XEP-0060 §7.2 retract IQ removing one pack from the user's own PEP
/// sticker node.
pub fn build_pack_retract_iq(iq_id: &str, pack_id: &PackId) -> Element {
    let retract = Element::builder("retract", NS_PUBSUB)
        .attr(minidom::rxml::xml_ncname!("node").to_owned(), STICKERS_NODE)
        .append(
            Element::builder("item", NS_PUBSUB)
                .attr(
                    minidom::rxml::xml_ncname!("id").to_owned(),
                    pack_id.as_str(),
                )
                .build(),
        )
        .build();
    Element::builder("iq", NS_CLIENT)
        .attr(minidom::rxml::xml_ncname!("type").to_owned(), "set")
        .attr(minidom::rxml::xml_ncname!("id").to_owned(), iq_id)
        .append(
            Element::builder("pubsub", NS_PUBSUB)
                .append(retract)
                .build(),
        )
        .build()
}

// ── Message marker ───────────────────────────────────────────────────────────

/// Reference to the pack a sent sticker came from (XEP-0449
/// §send-from-pack). `jid`/`node` locate packs hosted outside the
/// sender's own PEP `urn:xmpp:stickers:0` node.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackRef {
    pub id: PackId,
    pub jid: Option<BareJid>,
    pub node: Option<String>,
}

/// The `<sticker xmlns='urn:xmpp:stickers:0'/>` marker attached to an
/// outbound sticker message. `pack: None` sends the bare marker
/// (XEP-0449 §send: the `pack` attribute is a MAY).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StickerMarker {
    pub pack: Option<PackRef>,
}

/// Build the message-level `<sticker/>` marker element.
pub fn build_sticker_marker_element(marker: &StickerMarker) -> Element {
    let mut builder = Element::builder("sticker", NS_STICKERS);
    if let Some(pack) = marker.pack.as_ref() {
        builder = builder.attr(
            minidom::rxml::xml_ncname!("pack").to_owned(),
            pack.id.as_str(),
        );
        if let Some(jid) = pack.jid.as_ref() {
            builder = builder.attr(
                minidom::rxml::xml_ncname!("jid").to_owned(),
                jid.to_string(),
            );
        }
        if let Some(node) = pack.node.as_deref() {
            builder = builder.attr(minidom::rxml::xml_ncname!("node").to_owned(), node);
        }
    }
    builder.build()
}

#[cfg(test)]
mod tests;
