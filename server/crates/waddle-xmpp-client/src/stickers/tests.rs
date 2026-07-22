use super::*;

// ── Fixtures ─────────────────────────────────────────────────────────

/// The *visible* content of the xep-0449.xml "Publish a new sticker
/// pack" example (the example itself elides further items behind
/// `<!-- ... -->`, so the literal hash it prints is not reproducible —
/// see the module docs).
fn marsey_stickers() -> Vec<PackSticker> {
    vec![
        PackSticker {
            desc: "👍".to_string(),
            media_type: Some("image/png".to_string()),
            size: Some(71045),
            dimensions: Some(StickerDimensions {
                width: 512,
                height: 512,
            }),
            hashes: vec![StickerHash {
                algo: HashAlgo::Sha256,
                value_b64: "0AdP8lJOWJrugSKOIAqfEKqFatIpG5JBCjjxY253ojQ=".to_string(),
            }],
            sources: vec![
                "https://download.montague.lit/51078299-d071-46e1-b6d3-3de4a8ab67d6/sticker_marsey_thumbs_up.png"
                    .to_string(),
            ],
            suggests: vec![StickerText::plain("+1")],
        },
        PackSticker {
            desc: "😘".to_string(),
            media_type: Some("image/png".to_string()),
            size: Some(67016),
            dimensions: Some(StickerDimensions {
                width: 512,
                height: 512,
            }),
            hashes: vec![StickerHash {
                algo: HashAlgo::Sha256,
                value_b64: "gw+6xdCgOcvCYSKuQNrXH33lV9NMzuDf/s0huByCDsY=".to_string(),
            }],
            sources: vec![
                "https://download.montague.lit/51078299-d071-46e1-b6d3-3de4a8ab67d6/sticker_marsey_kiss.png"
                    .to_string(),
            ],
            suggests: vec![],
        },
    ]
}

fn marsey_name() -> Vec<StickerText> {
    vec![StickerText::plain("Marsey the Cat")]
}

fn marsey_summary() -> Vec<StickerText> {
    vec![StickerText::plain(
        "Be cute or be cynical, this little kitten works both ways.",
    )]
}

fn marsey_doc() -> StickerPackDoc {
    StickerPackDoc {
        id: compute_pack_id(&marsey_name(), &marsey_summary(), &marsey_stickers()),
        name: marsey_name(),
        summary: marsey_summary(),
        restricted: false,
        stickers: marsey_stickers(),
        pack_hash: Some(compute_pack_hash(
            &marsey_name(),
            &marsey_summary(),
            &marsey_stickers(),
        )),
    }
}

/// Pack id/hash of the visible example content, computed by this
/// module's spec-faithful algorithm and cross-checked against an
/// independent Python reference implementation of the §"Sticker pack
/// hash calculation" prose.
const MARSEY_VISIBLE_HASH_B64: &str = "N6zJI4PTGfFfSJbwakmBihwBdP6sPFSdhgKE5hzMH4w=";
const MARSEY_VISIBLE_ID: &str = "N6zJI4PTGfFfSJbwakmBihwB";

/// XEP-0449 "Publish a new sticker pack" wire payload — visible
/// content verbatim, including the legacy XEP-0446 `<dimensions/>`
/// form and the (unreproducible) pack hash the XEP prints.
const XEP_EXAMPLE_PACK_XML: &str = "<pack xmlns='urn:xmpp:stickers:0'>\
    <name>Marsey the Cat</name>\
    <summary>Be cute or be cynical, this little kitten works both ways.</summary>\
    <item>\
        <file xmlns='urn:xmpp:file:metadata:0'>\
            <media-type>image/png</media-type>\
            <desc>👍</desc>\
            <size>71045</size>\
            <dimensions>512x512</dimensions>\
            <hash xmlns='urn:xmpp:hashes:2' algo='sha-256'>0AdP8lJOWJrugSKOIAqfEKqFatIpG5JBCjjxY253ojQ=</hash>\
        </file>\
        <sources xmlns='urn:xmpp:sfs:0'>\
            <url-data xmlns='http://jabber.org/protocol/url-data' target='https://download.montague.lit/51078299-d071-46e1-b6d3-3de4a8ab67d6/sticker_marsey_thumbs_up.png'/>\
        </sources>\
        <suggest>+1</suggest>\
    </item>\
    <item>\
        <file xmlns='urn:xmpp:file:metadata:0'>\
            <media-type>image/png</media-type>\
            <desc>😘</desc>\
            <size>67016</size>\
            <dimensions>512x512</dimensions>\
            <hash xmlns='urn:xmpp:hashes:2' algo='sha-256'>gw+6xdCgOcvCYSKuQNrXH33lV9NMzuDf/s0huByCDsY=</hash>\
        </file>\
        <sources xmlns='urn:xmpp:sfs:0'>\
            <url-data xmlns='http://jabber.org/protocol/url-data' target='https://download.montague.lit/51078299-d071-46e1-b6d3-3de4a8ab67d6/sticker_marsey_kiss.png'/>\
        </sources>\
    </item>\
    <hash xmlns='urn:xmpp:hashes:2' algo='sha-256'>EpRv28DHHzFrE4zd+xaNpVb4jbu4s74XtioExNjQzZ0=</hash>\
</pack>";

/// The XEP's example pack id, pinned as an opaque wire token (pubsub
/// item id / `<sticker pack='…'/>` value) in the shape tests below.
const XEP_EXAMPLE_ID: &str = "EpRv28DHHzFrE4zd+xaNpVb4";

fn bare(value: &str) -> BareJid {
    value.parse().expect("test bare JID parses")
}

// ── Namespace exactness ──────────────────────────────────────────────

#[test]
fn namespace_constants_match_registrar() {
    assert_eq!(NS_STICKERS, "urn:xmpp:stickers:0");
    // XEP-0449: the PEP node id IS the payload namespace.
    assert_eq!(STICKERS_NODE, NS_STICKERS);
    assert_eq!(NS_HASHES, "urn:xmpp:hashes:2");
    assert_eq!(HashAlgo::Sha256.as_str(), "sha-256");
    assert_eq!(HashAlgo::from_attr("sha-256"), Some(HashAlgo::Sha256));
    assert_eq!(HashAlgo::from_attr("sha-1"), None);
}

// ── Pack hash calculation ────────────────────────────────────────────

#[test]
fn pack_id_pins_the_reference_vector() {
    let id = compute_pack_id(&marsey_name(), &marsey_summary(), &marsey_stickers());
    assert_eq!(id.as_str(), MARSEY_VISIBLE_ID);
    let full = compute_pack_hash(&marsey_name(), &marsey_summary(), &marsey_stickers());
    assert_eq!(full.algo, HashAlgo::Sha256);
    assert_eq!(full.value_b64, MARSEY_VISIBLE_HASH_B64);
}

#[test]
fn pack_id_is_first_24_chars_of_full_hash_b64() {
    let full = compute_pack_hash(&marsey_name(), &marsey_summary(), &marsey_stickers());
    let id = compute_pack_id(&marsey_name(), &marsey_summary(), &marsey_stickers());
    assert_eq!(id.as_str().len(), 24);
    assert!(full.value_b64.starts_with(id.as_str()));
}

#[test]
fn pack_hash_is_element_order_independent() {
    // §pack-hash sorts name/summary components and item components
    // "from lesser to greater" — document order must not matter.
    let mut reversed_stickers = marsey_stickers();
    reversed_stickers.reverse();
    assert_eq!(
        compute_pack_id(&marsey_name(), &marsey_summary(), &marsey_stickers()),
        compute_pack_id(&marsey_name(), &marsey_summary(), &reversed_stickers),
    );
}

#[test]
fn pack_hash_covers_only_significant_fields() {
    // Sources, suggests, media-type, size, and dimensions are NOT part
    // of the §pack-hash input — only name/summary/desc/hashes are.
    let mut mutated = marsey_stickers();
    mutated[0].sources = vec!["https://elsewhere.example/other.png".to_string()];
    mutated[0].suggests = vec![StickerText::plain(":thumbs:")];
    mutated[0].media_type = Some("image/webp".to_string());
    mutated[0].size = None;
    mutated[0].dimensions = None;
    assert_eq!(
        compute_pack_id(&marsey_name(), &marsey_summary(), &marsey_stickers()),
        compute_pack_id(&marsey_name(), &marsey_summary(), &mutated),
    );

    let mut changed_desc = marsey_stickers();
    changed_desc[0].desc = "👎".to_string();
    assert_ne!(
        compute_pack_id(&marsey_name(), &marsey_summary(), &marsey_stickers()),
        compute_pack_id(&marsey_name(), &marsey_summary(), &changed_desc),
    );

    let mut changed_hash = marsey_stickers();
    changed_hash[1].hashes[0].value_b64 =
        "AAAAxdCgOcvCYSKuQNrXH33lV9NMzuDf/s0huByCDsY=".to_string();
    assert_ne!(
        compute_pack_id(&marsey_name(), &marsey_summary(), &marsey_stickers()),
        compute_pack_id(&marsey_name(), &marsey_summary(), &changed_hash),
    );
}

#[test]
fn pack_hash_distinguishes_xml_lang_variants() {
    let tagged = vec![StickerText {
        lang: Some("en".to_string()),
        text: "Marsey the Cat".to_string(),
    }];
    assert_ne!(
        compute_pack_id(&marsey_name(), &marsey_summary(), &marsey_stickers()),
        compute_pack_id(&tagged, &marsey_summary(), &marsey_stickers()),
    );
}

// ── Pack element build / parse round-trip ────────────────────────────

#[test]
fn build_pack_element_matches_xep_shape() {
    let doc = marsey_doc();
    let pack = build_pack_element(&doc);
    assert!(pack.is("pack", NS_STICKERS));
    assert_eq!(
        pack.get_child("name", NS_STICKERS).map(|e| e.text()),
        Some("Marsey the Cat".to_string())
    );
    assert!(pack.get_child("summary", NS_STICKERS).is_some());
    assert!(pack.get_child("restricted", NS_STICKERS).is_none());

    let items: Vec<_> = pack
        .children()
        .filter(|child| child.is("item", NS_STICKERS))
        .collect();
    assert_eq!(items.len(), 2);
    let file = items[0]
        .get_child("file", NS_FILE_METADATA)
        .expect("XEP-0446 file metadata");
    assert_eq!(
        file.get_child("desc", NS_FILE_METADATA).map(|e| e.text()),
        Some("👍".to_string())
    );
    let hash = file.get_child("hash", NS_HASHES).expect("XEP-0300 hash");
    assert_eq!(hash.attr("algo"), Some("sha-256"));
    assert_eq!(hash.text(), "0AdP8lJOWJrugSKOIAqfEKqFatIpG5JBCjjxY253ojQ=");
    // Current XEP-0446 wire shape: width/height, not legacy dimensions.
    assert_eq!(
        file.get_child("width", NS_FILE_METADATA).map(|e| e.text()),
        Some("512".to_string())
    );
    assert_eq!(
        file.get_child("height", NS_FILE_METADATA).map(|e| e.text()),
        Some("512".to_string())
    );
    assert!(file.get_child("dimensions", NS_FILE_METADATA).is_none());
    let sources = items[0]
        .get_child("sources", NS_SFS)
        .expect("XEP-0447 sources");
    assert!(sources
        .children()
        .any(|child| child.is("url-data", NS_URL_DATA)
            && child
                .attr("target")
                .is_some_and(|t| t.ends_with("thumbs_up.png"))));
    assert_eq!(
        items[0].get_child("suggest", NS_STICKERS).map(|e| e.text()),
        Some("+1".to_string())
    );
    assert!(items[1].get_child("suggest", NS_STICKERS).is_none());

    // Trailing pack-level hash is always the recomputed value.
    let pack_hash = pack
        .children()
        .filter(|child| child.is("hash", NS_HASHES))
        .last()
        .expect("pack-level hash");
    assert_eq!(pack_hash.text(), MARSEY_VISIBLE_HASH_B64);
}

#[test]
fn pack_element_survives_build_parse_round_trip() {
    let doc = marsey_doc();
    let element = build_pack_element(&doc);
    let xml = String::from(&element);
    let reparsed: Element = xml.parse().expect("built pack reparses");
    let parsed = parse_pack_element(doc.id.clone(), &reparsed).expect("round-tripped pack parses");
    assert_eq!(parsed, doc);
}

#[test]
fn localized_name_summary_repeats_round_trip() {
    let name = vec![
        StickerText::plain("Marsey the Cat"),
        StickerText {
            lang: Some("de".to_string()),
            text: "Marsey die Katze".to_string(),
        },
    ];
    let summary = vec![
        StickerText {
            lang: Some("en".to_string()),
            text: "cynical kitten".to_string(),
        },
        StickerText {
            lang: Some("de".to_string()),
            text: "zynisches Kätzchen".to_string(),
        },
    ];
    let doc = StickerPackDoc {
        id: compute_pack_id(&name, &summary, &marsey_stickers()),
        name,
        summary,
        restricted: true,
        stickers: marsey_stickers(),
        pack_hash: None,
    };
    let element = build_pack_element(&doc);
    assert!(element.get_child("restricted", NS_STICKERS).is_some());
    let names: Vec<_> = element
        .children()
        .filter(|child| child.is("name", NS_STICKERS))
        .collect();
    assert_eq!(names.len(), 2);
    assert_eq!(
        names[1].attr_ns(&minidom::rxml::Namespace::XML, "lang"),
        Some("de")
    );

    let expected = StickerPackDoc {
        pack_hash: Some(compute_pack_hash(&doc.name, &doc.summary, &doc.stickers)),
        ..doc.clone()
    };
    let parsed = parse_pack_element(doc.id.clone(), &element).expect("parses");
    assert_eq!(parsed, expected);
}

#[test]
fn parse_accepts_the_xep_example_pack_verbatim() {
    let pack: Element = XEP_EXAMPLE_PACK_XML.parse().expect("example XML parses");
    let id = PackId::new(XEP_EXAMPLE_ID).expect("example id");
    let doc = parse_pack_element(id.clone(), &pack).expect("example pack parses");

    assert_eq!(doc.id, id);
    assert_eq!(doc.name, marsey_name());
    assert_eq!(doc.summary, marsey_summary());
    assert!(!doc.restricted);
    // Legacy `<dimensions>512x512</dimensions>` normalizes into the
    // typed width/height pair, so the parsed doc equals the fixture.
    assert_eq!(doc.stickers, marsey_stickers());
    // The wire hash is preserved verbatim, even though it cannot be
    // recomputed from the visible items (see module docs).
    assert_eq!(
        doc.pack_hash,
        Some(StickerHash {
            algo: HashAlgo::Sha256,
            value_b64: "EpRv28DHHzFrE4zd+xaNpVb4jbu4s74XtioExNjQzZ0=".to_string(),
        })
    );
}

#[test]
fn parse_drops_items_missing_mandatory_children() {
    // Item 1: no <hash/>. Item 2: desc only with xml:lang (not the
    // mandated lang-less fallback). Item 3: no sources. All dropped;
    // zero valid items ⇒ the pack itself is rejected.
    let pack: Element = "<pack xmlns='urn:xmpp:stickers:0'>\
        <name>Broken</name>\
        <item>\
            <file xmlns='urn:xmpp:file:metadata:0'><desc>😀</desc></file>\
            <sources xmlns='urn:xmpp:sfs:0'>\
                <url-data xmlns='http://jabber.org/protocol/url-data' target='https://x.example/a.png'/>\
            </sources>\
        </item>\
        <item>\
            <file xmlns='urn:xmpp:file:metadata:0'>\
                <desc xml:lang='en'>grin</desc>\
                <hash xmlns='urn:xmpp:hashes:2' algo='sha-256'>aGFzaA==</hash>\
            </file>\
            <sources xmlns='urn:xmpp:sfs:0'>\
                <url-data xmlns='http://jabber.org/protocol/url-data' target='https://x.example/b.png'/>\
            </sources>\
        </item>\
        <item>\
            <file xmlns='urn:xmpp:file:metadata:0'>\
                <desc>😀</desc>\
                <hash xmlns='urn:xmpp:hashes:2' algo='sha-256'>aGFzaA==</hash>\
            </file>\
        </item>\
    </pack>"
        .parse()
        .expect("valid xml");
    assert!(parse_pack_element(PackId::new("p").expect("id"), &pack).is_none());
}

#[test]
fn parse_rejects_wrong_namespace_and_unknown_hash_algos() {
    let wrong_ns: Element = "<pack xmlns='urn:xmpp:stickers:1'><name>n</name></pack>"
        .parse()
        .expect("valid xml");
    assert!(parse_pack_element(PackId::new("p").expect("id"), &wrong_ns).is_none());

    // A sticker whose only hash uses an unsupported algorithm has no
    // usable typed hash ⇒ the item is dropped.
    let pack: Element = "<pack xmlns='urn:xmpp:stickers:0'>\
        <name>n</name>\
        <item>\
            <file xmlns='urn:xmpp:file:metadata:0'>\
                <desc>😀</desc>\
                <hash xmlns='urn:xmpp:hashes:2' algo='sha-1'>aGFzaA==</hash>\
            </file>\
            <sources xmlns='urn:xmpp:sfs:0'>\
                <url-data xmlns='http://jabber.org/protocol/url-data' target='https://x.example/a.png'/>\
            </sources>\
        </item>\
    </pack>"
        .parse()
        .expect("valid xml");
    assert!(parse_pack_element(PackId::new("p").expect("id"), &pack).is_none());
}

// ── Items result parsing ─────────────────────────────────────────────

fn items_result(node: &str, items: Vec<Element>) -> Element {
    let mut items_builder = Element::builder("items", NS_PUBSUB)
        .attr(minidom::rxml::xml_ncname!("node").to_owned(), node);
    for item in items {
        items_builder = items_builder.append(item);
    }
    Element::builder("iq", "jabber:client")
        .attr(minidom::rxml::xml_ncname!("type").to_owned(), "result")
        .attr(minidom::rxml::xml_ncname!("id").to_owned(), "fetch-1")
        .append(
            Element::builder("pubsub", NS_PUBSUB)
                .append(items_builder.build())
                .build(),
        )
        .build()
}

fn pubsub_item(id: &str, payload: Element) -> Element {
    Element::builder("item", NS_PUBSUB)
        .attr(minidom::rxml::xml_ncname!("id").to_owned(), id)
        .append(payload)
        .build()
}

#[test]
fn parse_sticker_packs_result_returns_typed_docs() {
    let pack: Element = XEP_EXAMPLE_PACK_XML.parse().expect("example XML parses");
    let iq = items_result(
        STICKERS_NODE,
        vec![
            pubsub_item(XEP_EXAMPLE_ID, pack),
            // Malformed payload rows are dropped, not surfaced.
            pubsub_item("junk", Element::builder("pack", NS_STICKERS).build()),
        ],
    );
    let docs = parse_sticker_packs_result(&iq, STICKERS_NODE);
    assert_eq!(docs.len(), 1);
    assert_eq!(docs[0].id.as_str(), XEP_EXAMPLE_ID);
    assert_eq!(docs[0].stickers.len(), 2);
}

#[test]
fn parse_sticker_packs_result_guards_the_node_attribute() {
    let pack: Element = XEP_EXAMPLE_PACK_XML.parse().expect("example XML parses");
    let iq = items_result(
        "urn:xmpp:bookmarks:1",
        vec![pubsub_item(XEP_EXAMPLE_ID, pack)],
    );
    assert!(parse_sticker_packs_result(&iq, STICKERS_NODE).is_empty());
}

// ── IQ builders ──────────────────────────────────────────────────────

#[test]
fn items_iq_for_own_pep_omits_to() {
    let iq = build_sticker_packs_items_iq("iq-1", None);
    assert_eq!(iq.attr("type"), Some("get"));
    assert_eq!(iq.attr("id"), Some("iq-1"));
    assert_eq!(iq.attr("to"), None, "own-PEP routing has no to attribute");
    let items = iq
        .get_child("pubsub", NS_PUBSUB)
        .and_then(|pubsub| pubsub.get_child("items", NS_PUBSUB))
        .expect("items child");
    assert_eq!(items.attr("node"), Some(STICKERS_NODE));
    assert_eq!(items.children().count(), 0);
}

#[test]
fn items_iq_for_other_owner_targets_their_bare_jid() {
    let iq = build_sticker_packs_items_iq("iq-2", Some(&bare("romeo@montague.lit")));
    assert_eq!(iq.attr("to"), Some("romeo@montague.lit"));
}

#[test]
fn single_pack_items_iq_carries_item_id_and_node() {
    let iq = build_sticker_pack_items_iq(
        "iq-3",
        &bare("romeo@montague.lit"),
        "generic/sticker/node",
        &PackId::new(XEP_EXAMPLE_ID).expect("id"),
    );
    assert_eq!(iq.attr("to"), Some("romeo@montague.lit"));
    let items = iq
        .get_child("pubsub", NS_PUBSUB)
        .and_then(|pubsub| pubsub.get_child("items", NS_PUBSUB))
        .expect("items child");
    assert_eq!(items.attr("node"), Some("generic/sticker/node"));
    let item = items.get_child("item", NS_PUBSUB).expect("item child");
    assert_eq!(item.attr("id"), Some(XEP_EXAMPLE_ID));
}

#[test]
fn publish_iq_uses_the_recomputed_pack_id_as_item_id() {
    // The caller's id is untrusted display state: the pubsub item id
    // MUST be the id derived from the pack content (XEP-0449
    // §pack-hash), never the single-slot `current` idiom.
    let doc = StickerPackDoc {
        id: PackId::new("caller-supplied-wrong-id").expect("id"),
        ..marsey_doc()
    };
    let (id, iq) = build_pack_publish_iq("iq-4", &doc);
    assert_eq!(id.as_str(), MARSEY_VISIBLE_ID);
    assert_eq!(iq.attr("type"), Some("set"));
    assert_eq!(iq.attr("to"), None, "publish routes to own PEP");
    let publish = iq
        .get_child("pubsub", NS_PUBSUB)
        .and_then(|pubsub| pubsub.get_child("publish", NS_PUBSUB))
        .expect("publish child");
    assert_eq!(publish.attr("node"), Some(STICKERS_NODE));
    let item = publish.get_child("item", NS_PUBSUB).expect("item child");
    assert_eq!(item.attr("id"), Some(MARSEY_VISIBLE_ID));
    assert_ne!(item.attr("id"), Some("current"));
    let pack = item.get_child("pack", NS_STICKERS).expect("pack payload");
    // Trailing pack-level hash agrees with the item id prefix.
    let hash = pack
        .children()
        .filter(|child| child.is("hash", NS_HASHES))
        .last()
        .expect("pack hash");
    assert!(hash.text().starts_with(MARSEY_VISIBLE_ID));
}

#[test]
fn retract_iq_targets_the_pack_item() {
    let iq = build_pack_retract_iq("iq-5", &PackId::new(XEP_EXAMPLE_ID).expect("id"));
    assert_eq!(iq.attr("type"), Some("set"));
    let retract = iq
        .get_child("pubsub", NS_PUBSUB)
        .and_then(|pubsub| pubsub.get_child("retract", NS_PUBSUB))
        .expect("retract child");
    assert_eq!(retract.attr("node"), Some(STICKERS_NODE));
    assert_eq!(
        retract
            .get_child("item", NS_PUBSUB)
            .and_then(|item| item.attr("id")),
        Some(XEP_EXAMPLE_ID)
    );
}

// ── Message marker ───────────────────────────────────────────────────

#[test]
fn bare_sticker_marker_has_no_attributes() {
    let marker = build_sticker_marker_element(&StickerMarker { pack: None });
    assert!(marker.is("sticker", NS_STICKERS));
    assert_eq!(marker.attr("pack"), None);
    assert_eq!(marker.attr("jid"), None);
    assert_eq!(marker.attr("node"), None);
}

#[test]
fn pack_marker_carries_pack_and_optional_location() {
    let own_pep = build_sticker_marker_element(&StickerMarker {
        pack: Some(PackRef {
            id: PackId::new(XEP_EXAMPLE_ID).expect("id"),
            jid: None,
            node: None,
        }),
    });
    assert_eq!(own_pep.attr("pack"), Some(XEP_EXAMPLE_ID));
    assert_eq!(own_pep.attr("jid"), None);
    assert_eq!(own_pep.attr("node"), None);

    let foreign = build_sticker_marker_element(&StickerMarker {
        pack: Some(PackRef {
            id: PackId::new(XEP_EXAMPLE_ID).expect("id"),
            jid: Some(bare("romeo@montague.lit")),
            node: Some("generic/sticker/node".to_string()),
        }),
    });
    assert_eq!(foreign.attr("pack"), Some(XEP_EXAMPLE_ID));
    assert_eq!(foreign.attr("jid"), Some("romeo@montague.lit"));
    assert_eq!(foreign.attr("node"), Some("generic/sticker/node"));
}

// ── PackId invariants ────────────────────────────────────────────────

#[test]
fn pack_id_rejects_blank_values() {
    assert!(PackId::new("").is_none());
    assert!(PackId::new("   ").is_none());
    assert_eq!(
        PackId::new(XEP_EXAMPLE_ID).map(|id| id.to_string()),
        Some(XEP_EXAMPLE_ID.to_string())
    );
}
