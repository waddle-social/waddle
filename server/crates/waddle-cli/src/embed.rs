// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2025 Waddle Social

//! Embed pipeline — transforms raw XML payloads into renderable embeds
//! by routing through plugin `message_transformer` hooks.
//!
//! The CLI extracts `RawEmbed` structs from stanzas. This module provides:
//! - `EmbedProcessor` trait: abstraction over the WASM plugin runtime
//! - `NoopEmbedProcessor`: default when no plugins are loaded
//!
//! The actual WASM plugin integration lives in `waddle-plugins` and can be
//! wired in by implementing `EmbedProcessor` for the plugin runtime.

use crate::stanza::RawEmbed;

/// Result of embed processing — either a recognized embed with display data,
/// or the raw embed unchanged.
#[derive(Debug, Clone)]
pub enum ProcessedEmbed {
    /// Plugin recognized and enriched this embed.
    Rendered {
        namespace: String,
        /// JSON data from the plugin (ready for TUI/GUI rendering).
        data_json: String,
    },
    /// No plugin handled this embed — pass through raw.
    Raw(RawEmbed),
}

/// Trait for processing raw embeds through the plugin system.
///
/// Implementors call into the WASM plugin runtime's `message_transformer`
/// hook and return enriched embed data.
pub trait EmbedProcessor: Send + Sync {
    /// Process a message body + raw embeds through the plugin pipeline.
    ///
    /// Returns a list of `ProcessedEmbed`s — plugins may produce new embeds
    /// from URL detection in the body, or enrich existing raw embeds.
    fn process(&self, body: &str, raw_embeds: &[RawEmbed]) -> Vec<ProcessedEmbed>;
}

/// No-op processor that passes all embeds through unchanged.
/// Used when no WASM plugin runtime is available.
#[derive(Debug, Default)]
pub struct NoopEmbedProcessor;

impl EmbedProcessor for NoopEmbedProcessor {
    fn process(&self, _body: &str, raw_embeds: &[RawEmbed]) -> Vec<ProcessedEmbed> {
        raw_embeds
            .iter()
            .map(|e| ProcessedEmbed::Raw(e.clone()))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn noop_processor_passes_through() {
        let processor = NoopEmbedProcessor;
        let embeds = vec![RawEmbed {
            namespace: "urn:test:0".into(),
            name: "widget".into(),
            xml: "<widget xmlns='urn:test:0'/>".into(),
        }];

        let result = processor.process("hello", &embeds);
        assert_eq!(result.len(), 1);
        assert!(matches!(&result[0], ProcessedEmbed::Raw(e) if e.namespace == "urn:test:0"));
    }

    #[test]
    fn noop_processor_empty_embeds() {
        let processor = NoopEmbedProcessor;
        let result = processor.process("no embeds here", &[]);
        assert!(result.is_empty());
    }

    #[test]
    fn processed_embed_rendered_variant() {
        let embed = ProcessedEmbed::Rendered {
            namespace: "urn:waddle:github:0".into(),
            data_json: r#"{"type":"repo","name":"waddle"}"#.into(),
        };
        assert!(matches!(embed, ProcessedEmbed::Rendered { .. }));
    }
}
