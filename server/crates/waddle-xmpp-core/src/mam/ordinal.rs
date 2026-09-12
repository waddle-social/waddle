/// Durable position within one archive, independent of message timestamps.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, serde::Serialize)]
#[serde(transparent)]
pub struct ArchiveOrdinal(i64);

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum ArchiveOrdinalError {
    #[error("archive ordinal must not be zero")]
    Zero,
    #[error("archive ordinal must not be negative: {0}")]
    Negative(i64),
}

impl ArchiveOrdinal {
    /// The first position in any archive.
    pub const FIRST: Self = Self(1);

    pub fn from_storage(value: i64) -> Result<Self, ArchiveOrdinalError> {
        match value {
            0 => Err(ArchiveOrdinalError::Zero),
            value if value < 0 => Err(ArchiveOrdinalError::Negative(value)),
            value => Ok(Self(value)),
        }
    }

    pub fn to_storage(self) -> i64 {
        self.0
    }

    pub fn next(self) -> Option<Self> {
        self.0.checked_add(1).map(Self)
    }
}

impl<'de> serde::Deserialize<'de> for ArchiveOrdinal {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let value = <i64 as serde::Deserialize>::deserialize(deserializer)?;
        Self::from_storage(value).map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::{ArchiveOrdinal, ArchiveOrdinalError};

    #[test]
    fn storage_values_enforce_positive_checked_positions() {
        assert_eq!(
            ArchiveOrdinal::from_storage(0),
            Err(ArchiveOrdinalError::Zero)
        );
        assert_eq!(
            ArchiveOrdinal::from_storage(-1),
            Err(ArchiveOrdinalError::Negative(-1))
        );
        let first = ArchiveOrdinal::from_storage(1).unwrap();
        assert_eq!(first.to_storage(), 1);
        assert_eq!(first.next().unwrap().to_storage(), 2);
        assert!(first < first.next().unwrap());
        assert_eq!(ArchiveOrdinal::from_storage(i64::MAX).unwrap().next(), None);
    }
}
