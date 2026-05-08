use super::extension_forms::extension_namespaces_for_disco;
use super::*;

#[cfg(test)]
mod extension_disco_tests {
    use super::*;

    #[test]
    fn extension_namespaces_are_advertised_without_provider_gate() {
        let features = extension_namespaces_for_disco(vec![
            "urn:waddle:bot:1".to_string(),
            "urn:example:extension:1".to_string(),
        ]);

        assert_eq!(
            features,
            vec![
                Feature::new("urn:waddle:bot:1"),
                Feature::new("urn:example:extension:1")
            ]
        );
    }
}
