use crate::bindings::waddle::extension::types;
use crate::decisions::JudgmentBatch;

pub const NAMESPACE: &str = "urn:waddle:safety-scores:1";
pub const ROOT: &str = "safety-scores";

fn namespace() -> types::PayloadNamespace {
    types::PayloadNamespace {
        value: NAMESPACE.to_string(),
    }
}

fn attribute(name: &str, value: String) -> types::XmlAttribute {
    types::XmlAttribute {
        namespace: None,
        local_name: name.to_string(),
        value,
    }
}

/// Room identity, target identifiers, and the fastening wrapper are host-owned.
pub fn safety_scores(batch: &JudgmentBatch) -> types::ExtensionPayload {
    let mut tokens = Vec::with_capacity(2 + batch.judgments.len() * 2);
    tokens.push(types::XmlToken::StartElement(types::XmlElement {
        namespace: namespace(),
        local_name: ROOT.to_string(),
        attributes: vec![attribute("model-version", batch.model_version.clone())],
    }));
    for judgment in &batch.judgments {
        tokens.push(types::XmlToken::StartElement(types::XmlElement {
            namespace: namespace(),
            local_name: "score".to_string(),
            attributes: vec![
                attribute("category", judgment.kind.as_str().to_string()),
                attribute("probability", judgment.probability.to_string()),
                attribute("taxonomy-version", judgment.taxonomy_version.to_string()),
            ],
        }));
        tokens.push(types::XmlToken::EndElement);
    }
    tokens.push(types::XmlToken::EndElement);
    types::ExtensionPayload {
        namespace: namespace(),
        root: types::PayloadRoot {
            namespace: namespace(),
            local_name: ROOT.to_string(),
        },
        tokens,
    }
}
