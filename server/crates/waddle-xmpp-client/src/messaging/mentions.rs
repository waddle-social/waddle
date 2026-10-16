//! XEP-0372 mention references resolved from plain-text `@label` tokens.
//!
//! Composers write mentions as body text (`@alice`). Receivers decide
//! "this message mentions me" from `<reference type='mention'/>` elements
//! only, so a body token without a reference is invisible to row
//! highlighting, notifications, mention counters and push. The builder
//! closes that gap: given the sender's [`MentionDirectory`] it emits one
//! mention reference per resolvable token, with body offsets in Unicode
//! scalar values (`begin` at the `@`, `end` exclusive), the same unit the
//! reply fallback and markup offsets use.

use jid::BareJid;

use super::types::{MarkupSpanData, ReferenceData, SendMessageOptions};

/// XEP-0372 `type` attribute value for a mention reference.
pub const REFERENCE_TYPE_MENTION: &str = "mention";

/// Broadcast mention URI addressing every member of a room.
pub const MENTION_URI_EVERYONE: &str = "xmpp:@everyone";

/// Broadcast mention URI addressing the occupants currently present.
pub const MENTION_URI_HERE: &str = "xmpp:@here";

/// Broadcast labels and their URIs, in the order composers list them.
const BROADCASTS: [(&str, &str); 2] = [
    ("everyone", MENTION_URI_EVERYONE),
    ("here", MENTION_URI_HERE),
];

/// Markup span types whose text is literal and never carries mentions.
const LITERAL_SPAN_TYPES: [&str; 2] = ["code", "code_block"];

/// One mentionable entity: the label a sender types after `@` and the
/// account it resolves to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MentionTarget {
    /// Display label as typed after `@` (a room nick or username). Human
    /// text, matched case-insensitively; never sent on the wire.
    pub label: String,
    /// Real bare JID the mention addresses (`xmpp:<jid>` on the wire).
    pub jid: BareJid,
}

/// The sender-side directory used to resolve typed `@label` tokens.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MentionDirectory {
    /// Mentionable members. A label shared by two different JIDs is
    /// ambiguous and resolves to nothing rather than to a guess.
    pub targets: Vec<MentionTarget>,
    /// Resolve `@everyone` / `@here` to broadcast mention URIs. Room
    /// conversations set this; one-to-one chats do not.
    pub broadcasts: bool,
}

struct Candidate {
    label: Vec<char>,
    uri: String,
}

/// Mention references for every resolvable `@label` token in `body` that
/// does not fall inside the reply fallback, a code span, or a range an
/// explicit reference in `options` already covers.
pub(crate) fn resolve_typed_mentions(
    body: &str,
    directory: &MentionDirectory,
    options: &SendMessageOptions,
) -> Vec<ReferenceData> {
    let candidates = candidates_of(directory);
    if candidates.is_empty() {
        return Vec::new();
    }
    let chars: Vec<char> = body.chars().collect();
    let excluded = excluded_ranges(options);
    let mut resolved = Vec::new();
    let mut index = 0;
    while index < chars.len() {
        let Some((end, uri)) = match_at(&chars, index, &candidates) else {
            index += 1;
            continue;
        };
        let (Ok(begin), Ok(end_u32)) = (u32::try_from(index), u32::try_from(end)) else {
            break;
        };
        if !excluded
            .iter()
            .any(|&(start, stop)| begin < stop && start < end_u32)
        {
            resolved.push(ReferenceData {
                ref_type: REFERENCE_TYPE_MENTION.to_string(),
                uri: uri.to_string(),
                begin,
                end: end_u32,
                anchor: None,
            });
        }
        index = end;
    }
    resolved
}

/// Candidates ordered longest label first so `@Ann Lee` wins over `@Ann`.
fn candidates_of(directory: &MentionDirectory) -> Vec<Candidate> {
    let mut by_label: Vec<(String, Option<String>)> = Vec::new();
    let mut insert = |label: String, uri: String| match by_label
        .iter_mut()
        .find(|(existing, _)| *existing == label)
    {
        Some((_, slot)) => {
            if slot.as_deref() != Some(uri.as_str()) {
                *slot = None;
            }
        }
        None => by_label.push((label, Some(uri))),
    };
    let broadcast_labels: Vec<&str> = BROADCASTS.iter().map(|(label, _)| *label).collect();
    for target in &directory.targets {
        let label = canonical_label(&target.label);
        // Member labels never shadow broadcasts (web composer parity).
        if label.is_empty() || broadcast_labels.contains(&label.as_str()) {
            continue;
        }
        insert(label, format!("xmpp:{}", target.jid));
    }
    if directory.broadcasts {
        for (label, uri) in BROADCASTS {
            insert(label.to_string(), uri.to_string());
        }
    }
    let mut candidates: Vec<Candidate> = by_label
        .into_iter()
        .filter_map(|(label, uri)| {
            uri.map(|uri| Candidate {
                label: label.chars().collect(),
                uri,
            })
        })
        .collect();
    candidates.sort_by_key(|candidate| std::cmp::Reverse(candidate.label.len()));
    candidates
}

/// `(end, uri)` of the candidate whose `@label` token starts at `index`.
fn match_at<'a>(
    chars: &[char],
    index: usize,
    candidates: &'a [Candidate],
) -> Option<(usize, &'a str)> {
    if chars[index] != '@' || !boundary_before(chars, index) {
        return None;
    }
    let start = index + 1;
    candidates.iter().find_map(|candidate| {
        let end = start + candidate.label.len();
        let typed = chars.get(start..end)?;
        let matches = typed
            .iter()
            .zip(&candidate.label)
            .all(|(typed, label)| typed.to_lowercase().eq(label.to_lowercase()));
        (matches && boundary_after(chars, end)).then_some((end, candidate.uri.as_str()))
    })
}

/// The `@` must open a token: not part of an email address or a word.
fn boundary_before(chars: &[char], index: usize) -> bool {
    match index.checked_sub(1).and_then(|prev| chars.get(prev)) {
        None => true,
        Some(prev) => !(prev.is_alphanumeric() || matches!(prev, '_' | '.' | '-' | '+' | '@')),
    }
}

/// The label must end the token: end of body, whitespace, or closing
/// punctuation (the set the web renderer treats as a mention boundary).
fn boundary_after(chars: &[char], end: usize) -> bool {
    match chars.get(end) {
        None => true,
        Some(next) => {
            next.is_whitespace()
                || matches!(
                    next,
                    '<' | '.' | ',' | ';' | ':' | '!' | '?' | '\'' | '"' | ')' | ']' | '}' | '&'
                )
        }
    }
}

fn excluded_ranges(options: &SendMessageOptions) -> Vec<(u32, u32)> {
    let mut ranges = Vec::new();
    if let Some(fallback) = options.fallback.as_ref() {
        ranges.push((fallback.start, fallback.end));
    }
    ranges.extend(
        options
            .markup_spans
            .iter()
            .filter(|span: &&MarkupSpanData| LITERAL_SPAN_TYPES.contains(&span.span_type.as_str()))
            .map(|span| (span.start, span.end)),
    );
    ranges.extend(
        options
            .references
            .iter()
            .filter(|reference| reference.begin < reference.end)
            .map(|reference| (reference.begin, reference.end)),
    );
    ranges
}

/// Web `canonicalMentionIdentifier` parity for labels: trim, drop leading
/// `@`s, lowercase.
fn canonical_label(label: &str) -> String {
    label.trim().trim_start_matches('@').to_lowercase()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::xep::reply::FallbackRange;

    fn jid(value: &str) -> BareJid {
        value.parse().expect("test JID")
    }

    fn room(targets: &[(&str, &str)]) -> MentionDirectory {
        MentionDirectory {
            targets: targets
                .iter()
                .map(|(label, bare)| MentionTarget {
                    label: (*label).to_string(),
                    jid: jid(bare),
                })
                .collect(),
            broadcasts: true,
        }
    }

    fn spans(
        body: &str,
        directory: &MentionDirectory,
        options: &SendMessageOptions,
    ) -> Vec<(u32, u32, String)> {
        resolve_typed_mentions(body, directory, options)
            .into_iter()
            .map(|reference| {
                assert_eq!(reference.ref_type, REFERENCE_TYPE_MENTION);
                assert_eq!(reference.anchor, None);
                (reference.begin, reference.end, reference.uri)
            })
            .collect()
    }

    #[test]
    fn resolves_each_typed_label_with_code_point_offsets() {
        let directory = room(&[
            ("randax", "randax@waddle.social"),
            ("rawkode", "rawkode@waddle.social"),
        ]);
        let body = "@randax @rawkode okay if I try to get us through";
        assert_eq!(
            spans(body, &directory, &SendMessageOptions::default()),
            vec![
                (0, 7, "xmpp:randax@waddle.social".to_string()),
                (8, 16, "xmpp:rawkode@waddle.social".to_string()),
            ]
        );
    }

    #[test]
    fn offsets_count_unicode_scalar_values() {
        let directory = room(&[("bob", "bob@example.com")]);
        // Each emoji is one scalar value (two UTF-16 units, four UTF-8 bytes).
        assert_eq!(
            spans("🐧🐧 @bob", &directory, &SendMessageOptions::default()),
            vec![(3, 7, "xmpp:bob@example.com".to_string())]
        );
    }

    #[test]
    fn matches_case_insensitively_and_before_closing_punctuation() {
        let directory = room(&[("Alice", "alice@example.com")]);
        assert_eq!(
            spans(
                "thanks @ALICE, and (@alice)!",
                &directory,
                &SendMessageOptions::default()
            ),
            vec![
                (7, 13, "xmpp:alice@example.com".to_string()),
                (20, 26, "xmpp:alice@example.com".to_string()),
            ]
        );
    }

    #[test]
    fn prefers_the_longest_label() {
        let directory = room(&[
            ("ann", "ann@example.com"),
            ("ann lee", "annlee@example.com"),
        ]);
        assert_eq!(
            spans(
                "@Ann Lee hi @ann",
                &directory,
                &SendMessageOptions::default()
            ),
            vec![
                (0, 8, "xmpp:annlee@example.com".to_string()),
                (12, 16, "xmpp:ann@example.com".to_string()),
            ]
        );
    }

    #[test]
    fn ignores_partial_words_and_email_addresses() {
        let directory = room(&[("bob", "bob@example.com")]);
        let options = SendMessageOptions::default();
        assert!(spans(
            "@bobby mail me at alice@bob or x.@bob",
            &directory,
            &options
        )
        .is_empty());
    }

    #[test]
    fn broadcasts_only_resolve_when_enabled() {
        let mut directory = room(&[]);
        assert_eq!(
            spans(
                "@everyone and @here",
                &directory,
                &SendMessageOptions::default()
            ),
            vec![
                (0, 9, MENTION_URI_EVERYONE.to_string()),
                (14, 19, MENTION_URI_HERE.to_string()),
            ]
        );
        directory.broadcasts = false;
        assert!(spans(
            "@everyone and @here",
            &directory,
            &SendMessageOptions::default()
        )
        .is_empty());
    }

    #[test]
    fn member_labels_never_shadow_broadcasts() {
        let directory = room(&[("here", "here@example.com")]);
        assert_eq!(
            spans("@here", &directory, &SendMessageOptions::default()),
            vec![(0, 5, MENTION_URI_HERE.to_string())]
        );
    }

    #[test]
    fn ambiguous_labels_resolve_to_nothing() {
        let directory = room(&[("sam", "sam@one.example"), ("Sam", "sam@two.example")]);
        assert!(spans("@sam", &directory, &SendMessageOptions::default()).is_empty());
        let same = room(&[("sam", "sam@one.example"), ("Sam", "sam@one.example")]);
        assert_eq!(
            spans("@sam", &same, &SendMessageOptions::default()).len(),
            1
        );
    }

    #[test]
    fn skips_fallback_code_and_explicitly_referenced_ranges() {
        let directory = room(&[("bob", "bob@example.com")]);
        // fallback 0..12 | code span 12..21 | explicit ref 26..30 | free 31..35
        let body = "> @bob said\ncode @bob end @bob @bob";
        let options = SendMessageOptions {
            fallback: Some(FallbackRange { start: 0, end: 12 }),
            markup_spans: vec![MarkupSpanData {
                span_type: "code".to_string(),
                start: 12,
                end: 21,
                uri: None,
            }],
            references: vec![ReferenceData {
                ref_type: REFERENCE_TYPE_MENTION.to_string(),
                uri: "xmpp:bob@example.com".to_string(),
                begin: 26,
                end: 30,
                anchor: None,
            }],
            ..SendMessageOptions::default()
        };
        assert_eq!(
            spans(body, &directory, &options),
            vec![(31, 35, "xmpp:bob@example.com".to_string())]
        );
        // Without the exclusions every token resolves.
        assert_eq!(
            spans(body, &directory, &SendMessageOptions::default()).len(),
            4
        );
    }

    #[test]
    fn empty_directory_resolves_nothing() {
        let directory = MentionDirectory::default();
        assert!(spans("@everyone @bob", &directory, &SendMessageOptions::default()).is_empty());
    }
}
