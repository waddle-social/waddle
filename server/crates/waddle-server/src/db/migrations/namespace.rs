/// Versions at or above this deliberately offset boundary belong to the
/// per-waddle schema namespace, so both migration catalogs share one ledger
/// without version collisions.
pub const WADDLE_NAMESPACE_START: i64 = 1000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MigrationNamespace {
    Global,
    Waddle,
}

impl MigrationNamespace {
    pub const fn of(version: i64) -> Self {
        if version < WADDLE_NAMESPACE_START {
            Self::Global
        } else {
            Self::Waddle
        }
    }

    const fn as_str(self) -> &'static str {
        match self {
            Self::Global => "global",
            Self::Waddle => "waddle",
        }
    }
}

impl std::fmt::Display for MigrationNamespace {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::{MigrationNamespace, WADDLE_NAMESPACE_START};

    #[test]
    fn of_uses_the_namespace_boundary() {
        assert_eq!(MigrationNamespace::of(1), MigrationNamespace::Global);
        assert_eq!(MigrationNamespace::of(-1), MigrationNamespace::Global);
        assert_eq!(MigrationNamespace::of(0), MigrationNamespace::Global);
        assert_eq!(
            MigrationNamespace::of(WADDLE_NAMESPACE_START - 1),
            MigrationNamespace::Global
        );
        assert_eq!(
            MigrationNamespace::of(WADDLE_NAMESPACE_START),
            MigrationNamespace::Waddle
        );
        assert_eq!(MigrationNamespace::of(1007), MigrationNamespace::Waddle);
    }
}
