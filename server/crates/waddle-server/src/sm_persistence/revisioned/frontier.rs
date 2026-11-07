use waddle_xmpp::stream_management::persistence::{PersistedSession, PersistedUnackedStanza};

use super::types::RevisionError;

const HALF_RANGE: u64 = 1_u64 << 31;
const MAX_ORDINAL: u64 = i64::MAX as u64;

/// Generation-local ordinals for acknowledged, exposed and appended frames.
///
/// Creation anchors the first ordinal to the supplied wrapping ACK floor; it
/// does not infer how many wraps occurred before that generation was captured.
/// Exposure is a recorded authorization for a write attempt, not evidence of
/// an actual write or peer receipt. This value is not a socket or claim permit.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ExposureFrontier {
    acknowledged: u64,
    exposed: u64,
    appended: u64,
}

impl ExposureFrontier {
    pub fn acknowledged(&self) -> u64 {
        self.acknowledged
    }

    pub fn exposed(&self) -> u64 {
        self.exposed
    }

    pub fn appended(&self) -> u64 {
        self.appended
    }

    pub(super) fn initial(session: &PersistedSession) -> Result<Self, RevisionError> {
        let acknowledged = u64::from(session.last_acked);
        let outstanding = u64::from(session.outbound_count.wrapping_sub(session.last_acked));
        let frontier = Self {
            acknowledged,
            exposed: acknowledged,
            appended: acknowledged + outstanding,
        };
        frontier.validate(session)?;
        Ok(frontier)
    }

    pub(super) fn restored(
        acknowledged: i64,
        exposed: i64,
        appended: i64,
        session: &PersistedSession,
    ) -> Result<Self, RevisionError> {
        let frontier = Self {
            acknowledged: u64::try_from(acknowledged).map_err(|_| RevisionError::InvalidInput)?,
            exposed: u64::try_from(exposed).map_err(|_| RevisionError::InvalidInput)?,
            appended: u64::try_from(appended).map_err(|_| RevisionError::InvalidInput)?,
        };
        frontier.validate(session)?;
        Ok(frontier)
    }

    pub(super) fn sql_values(self) -> Result<(i64, i64, i64), RevisionError> {
        Ok((
            i64::try_from(self.acknowledged).map_err(|_| RevisionError::OrdinalExhausted)?,
            i64::try_from(self.exposed).map_err(|_| RevisionError::OrdinalExhausted)?,
            i64::try_from(self.appended).map_err(|_| RevisionError::OrdinalExhausted)?,
        ))
    }

    pub(super) fn extend(mut self, amount: u32) -> Result<Self, RevisionError> {
        self.appended = self
            .appended
            .checked_add(u64::from(amount))
            .filter(|value| *value <= MAX_ORDINAL)
            .ok_or(RevisionError::OrdinalExhausted)?;
        if self.appended - self.acknowledged >= HALF_RANGE {
            return Err(RevisionError::InvalidInput);
        }
        Ok(self)
    }

    pub(super) fn expose(
        mut self,
        ordinal: u64,
        retained: &[PersistedUnackedStanza],
    ) -> Result<Self, RevisionError> {
        let next = self
            .exposed
            .checked_add(1)
            .ok_or(RevisionError::OrdinalExhausted)?;
        if ordinal != next
            || ordinal > self.appended
            || !retained.iter().any(|frame| frame.sequence == wire(ordinal))
        {
            return Err(RevisionError::InvalidExposure);
        }
        self.exposed = ordinal;
        Ok(self)
    }

    pub(super) fn acknowledge(mut self, h: u32) -> Result<Self, RevisionError> {
        let advance = u64::from(h.wrapping_sub(wire(self.acknowledged)));
        if advance >= HALF_RANGE || advance > self.exposed - self.acknowledged {
            return Err(RevisionError::AcknowledgementBeyondExposure);
        }
        self.acknowledged += advance;
        Ok(self)
    }

    fn validate(self, session: &PersistedSession) -> Result<(), RevisionError> {
        if self.acknowledged > self.exposed
            || self.exposed > self.appended
            || self.appended > MAX_ORDINAL
            || self.appended - self.acknowledged >= HALF_RANGE
            || wire(self.acknowledged) != session.last_acked
            || wire(self.appended) != session.outbound_count
            // No supported transition can expose a missing prefix. Legacy
            // gaps need independent evidence before an ACK can cover them.
            || (session.replay_gap_through.is_some() && self.exposed != self.acknowledged)
        {
            return Err(RevisionError::InvalidInput);
        }
        Ok(())
    }
}

fn wire(ordinal: u64) -> u32 {
    // The XEP counter is the low 32 bits of the generation-local ordinal.
    let bytes = ordinal.to_le_bytes();
    u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]])
}
