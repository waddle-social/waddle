pub mod fixtures {
    use std::fs;
    use std::io;
    use std::path::{Path, PathBuf};

    pub fn root() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("..")
            .join("tests")
            .join("fixtures")
    }

    pub fn path(relative: impl AsRef<Path>) -> PathBuf {
        root().join(relative.as_ref())
    }

    pub fn read(relative: impl AsRef<Path>) -> io::Result<String> {
        fs::read_to_string(path(relative))
    }

    pub fn stanza(name: &str) -> String {
        read_or_panic(Path::new("stanzas").join(name))
    }

    pub fn roster(name: &str) -> String {
        read_or_panic(Path::new("roster").join(name))
    }

    pub fn config(name: &str) -> String {
        read_or_panic(Path::new("config").join(name))
    }

    fn read_or_panic(relative: impl AsRef<Path>) -> String {
        let relative = relative.as_ref();
        read(relative).unwrap_or_else(|error| {
            panic!(
                "failed to read fixture {}: {error}",
                relative.to_string_lossy()
            )
        })
    }
}

#[cfg(test)]
mod tests {
    use super::fixtures;

    #[test]
    fn fixture_root_exists() {
        assert!(fixtures::root().is_dir());
    }

    #[test]
    fn loads_stanza_fixture() {
        let stanza = fixtures::stanza("message-chat.xml");
        assert!(stanza.contains("<message"));
    }

    #[test]
    fn loads_roster_fixture() {
        let roster = fixtures::roster("basic-roster.json");
        let json: serde_json::Value =
            serde_json::from_str(&roster).expect("basic-roster.json should be valid json");
        assert!(json.is_array());
    }

    #[test]
    fn loads_config_fixture() {
        let config = fixtures::config("minimal-config.toml");
        let toml: toml::Value =
            toml::from_str(&config).expect("minimal-config.toml should be valid toml");
        assert!(toml.is_table());
    }
}
