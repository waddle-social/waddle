use minidom::Element;

use super::super::namespaces::*;
use super::super::types::*;
use crate::xep::encrypted_file as xep_encrypted_file;

pub(super) fn parse_shared_file(reference_el: &Element) -> Option<SharedFile> {
    // Look for nested file metadata; structure varies by implementation.
    // Try XEP-0447 <sources> / <url-data> layout first.
    let mut url: Option<String> = None;
    let mut name: Option<String> = None;
    let mut media_type: Option<String> = None;
    let mut size: Option<u64> = None;
    let mut width: Option<u32> = None;
    let mut height: Option<u32> = None;
    let mut disposition: Option<String> = None;

    for child in reference_el.children() {
        match child.name() {
            "url-data" => {
                url = child.attr("target").map(String::from);
            }
            "file" => {
                name = child
                    .get_child("name", child.ns().as_str())
                    .map(|e| e.text());
                media_type = child
                    .get_child("media-type", child.ns().as_str())
                    .map(|e| e.text());
                size = child
                    .get_child("size", child.ns().as_str())
                    .and_then(|e| e.text().parse().ok());
                if let Some(thumb) = child.get_child("thumbnail", child.ns().as_str()) {
                    width = thumb.attr("width").and_then(|v| v.parse().ok());
                    height = thumb.attr("height").and_then(|v| v.parse().ok());
                }
                if let Some(disp) = child.get_child("disposition", child.ns().as_str()) {
                    disposition = Some(disp.text());
                }
            }
            // Simple <url> child fallback
            "url" => {
                url = Some(child.text());
            }
            _ => {}
        }
    }

    let disposition =
        SharedFileDisposition::from_text_or_infer(disposition.as_deref(), media_type.as_deref());
    url.map(|u| SharedFile {
        url: u,
        name,
        media_type,
        size,
        width,
        height,
        desc: None,
        hashes: Vec::new(),
        disposition,
        encrypted: None,
    })
}

pub(super) fn parse_file_sharing_element(file_sharing_el: &Element) -> Option<SharedFile> {
    let mut url: Option<String> = None;
    let mut name: Option<String> = None;
    let mut media_type: Option<String> = None;
    let mut size: Option<u64> = None;
    let mut width: Option<u32> = None;
    let mut height: Option<u32> = None;
    let disposition_attr = file_sharing_el.attr("disposition");
    let mut encrypted = None;
    let mut desc: Option<String> = None;
    let mut hashes: Vec<crate::stickers::StickerHash> = Vec::new();

    if let Some(file_el) = file_sharing_el.get_child("file", NS_FILE_METADATA) {
        name = file_el
            .get_child("name", NS_FILE_METADATA)
            .map(|e| e.text());
        media_type = file_el
            .get_child("media-type", NS_FILE_METADATA)
            .map(|e| e.text());
        size = file_el
            .get_child("size", NS_FILE_METADATA)
            .and_then(|e| e.text().parse().ok());
        width = file_el
            .get_child("width", NS_FILE_METADATA)
            .and_then(|e| e.text().parse().ok());
        height = file_el
            .get_child("height", NS_FILE_METADATA)
            .and_then(|e| e.text().parse().ok());
        // XEP-0446 lang-less textual fallback — for XEP-0449 stickers
        // this is the emoji rendered by non-image UIs. Only the
        // lang-less variant is the mandated fallback.
        desc = file_el
            .children()
            .find(|child| {
                child.is("desc", NS_FILE_METADATA)
                    && child
                        .attr_ns(&minidom::rxml::Namespace::XML, "lang")
                        .is_none()
            })
            .map(Element::text);
        // XEP-0300 plaintext content hashes inside <file/>. Unknown
        // algorithms are dropped at this boundary (typed HashAlgo).
        hashes = file_el
            .children()
            .filter_map(crate::stickers::parse_file_hash_element)
            .collect();
    }

    if let Some(sources_el) = file_sharing_el.get_child("sources", NS_SFS) {
        for source_el in sources_el.children() {
            if source_el.is("url-data", NS_URL_DATA) {
                if url.is_none() {
                    url = source_el.attr("target").map(String::from);
                }
            } else if xep_encrypted_file::is_encrypted_element(source_el) {
                encrypted = xep_encrypted_file::parse_encrypted_element(source_el);
            }
        }
    }
    if url.is_none() {
        url = encrypted
            .as_ref()
            .and_then(|encrypted| encrypted.sources.first().cloned());
    }

    let disposition =
        SharedFileDisposition::from_text_or_infer(disposition_attr, media_type.as_deref());
    url.map(|u| SharedFile {
        url: u,
        name,
        media_type,
        size,
        width,
        height,
        desc,
        hashes,
        disposition,
        encrypted,
    })
}
