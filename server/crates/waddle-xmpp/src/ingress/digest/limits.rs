//! Bounds for SemanticDigest input and canonical preimages.

use crate::ingress::MAX_ORIGIN_ID_BYTES;

/// Maximum element nesting depth in one retained extension tree (root is one).
pub const MAX_DEPTH: usize = 16;
/// Maximum number of element and text nodes across all retained extensions.
pub const MAX_TOTAL_NODES: usize = 1024;
/// Maximum number of attributes on one retained extension element.
pub const MAX_ATTRS_PER_ELEMENT: usize = 32;
/// Maximum UTF-8 byte length of one text node, attribute value, body, or subject.
pub const MAX_TEXT_LEN: usize = 65_536;
/// Maximum UTF-8 byte length of a namespace, local name, or language key.
pub const MAX_NAME_LEN: usize = 1_024;
/// Maximum entries in each body or subject language map.
pub const MAX_LANG_ENTRIES: usize = 64;
/// Maximum UTF-8 byte length of an origin, thread, or reply identifier.
pub const MAX_ID_LEN: usize = MAX_ORIGIN_ID_BYTES;
/// Maximum number of bytes in the complete v1 canonical preimage.
pub const MAX_PREIMAGE_BYTES: usize = 262_144;
