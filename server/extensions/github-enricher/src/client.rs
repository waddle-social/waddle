use crate::EmbedElement;

pub fn build_repo_embed(url: &str) -> EmbedElement {
    EmbedElement {
        element_name: "repo".to_string(),
        namespace: "urn:waddle:github:0".to_string(),
        attributes: vec![
            ("url".to_string(), url.to_string()),
            ("owner".to_string(), "unknown".to_string()),
            ("name".to_string(), "unknown".to_string()),
        ],
        children: Vec::new(),
    }
}
