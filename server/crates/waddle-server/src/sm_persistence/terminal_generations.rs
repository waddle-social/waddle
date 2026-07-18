use super::codec::parse_unacked_purpose;
use super::*;

struct TerminalRowGroup {
    key: SmTerminalGenerationKey,
    session: PersistedSession,
    promotion_attempts: u32,
    unacked: Vec<PersistedUnackedStanza>,
}

impl TerminalRowGroup {
    fn finish(self) -> Result<PersistedTerminalGeneration, SmPersistenceError> {
        let key = self.key.clone();
        let snapshot = PersistedSmSnapshot::new(self.session, self.unacked).map_err(|error| {
            SmPersistenceError::CorruptTerminal {
                key: key.clone(),
                detail: error.to_string(),
            }
        })?;
        PersistedTerminalGeneration::with_promotion_attempts(
            self.key,
            snapshot,
            self.promotion_attempts,
        )
        .map_err(|error| SmPersistenceError::CorruptTerminal {
            key,
            detail: error.to_string(),
        })
    }
}

enum TerminalRowGroupState {
    Healthy(TerminalRowGroup),
    Poisoned {
        key: SmTerminalGenerationKey,
        detail: String,
    },
}

#[derive(Clone, Copy)]
pub(crate) enum TerminalDecodeMode {
    /// Exact reads surface corruption to the caller so it can quarantine the
    /// structurally identified generation under its claim fence.
    Strict,
    /// Startup/targeted scans preserve a parseable poison generation as a
    /// typed exact-key entry and continue recovering independent work.
    Scan,
}

fn finish_group(
    group: TerminalRowGroupState,
    mode: TerminalDecodeMode,
    output: &mut Vec<TerminalGenerationScanEntry>,
) -> Result<(), SmPersistenceError> {
    match group {
        TerminalRowGroupState::Healthy(group) => match group.finish() {
            Ok(terminal) => output.push(TerminalGenerationScanEntry::Persisted(terminal)),
            Err(SmPersistenceError::CorruptTerminal { key, detail }) => {
                return finish_poison(key, detail, mode, output)
            }
            Err(error) => return Err(error),
        },
        TerminalRowGroupState::Poisoned { key, detail } => {
            return finish_poison(key, detail, mode, output)
        }
    }
    Ok(())
}

fn finish_poison(
    key: SmTerminalGenerationKey,
    detail: String,
    mode: TerminalDecodeMode,
    output: &mut Vec<TerminalGenerationScanEntry>,
) -> Result<(), SmPersistenceError> {
    match mode {
        TerminalDecodeMode::Strict => Err(SmPersistenceError::CorruptTerminal { key, detail }),
        TerminalDecodeMode::Scan => {
            tracing::warn!(
                terminal_generation = %key,
                %detail,
                "surfacing one corrupt terminal SM generation for exact quarantine; independent recovery continues"
            );
            output.push(TerminalGenerationScanEntry::Corrupt { key, detail });
            Ok(())
        }
    }
}

fn decode_terminal_unacked_row(
    row: &crate::db::Row,
    key: &SmTerminalGenerationKey,
) -> Result<Option<PersistedUnackedStanza>, SmPersistenceError> {
    let purpose_raw: Option<String> =
        row.get(23)
            .map_err(|error| SmPersistenceError::InvalidUnackedPurpose {
                detail: error.to_string(),
            })?;
    let Some(purpose_raw) = purpose_raw else {
        let sequence: Option<i64> =
            row.get(20)
                .map_err(|error| SmPersistenceError::InvalidUnackedPurpose {
                    detail: format!("missing purpose alongside unreadable sequence: {error}"),
                })?;
        if sequence.is_none() {
            return Ok(None);
        }
        return Err(SmPersistenceError::InvalidUnackedPurpose {
            detail: "missing value on a terminal unacked row".to_string(),
        });
    };
    // Purpose controls whether recovery may ever promote this row into
    // application delivery. Validate it before decoding sequence, XML, or
    // receipt time so compound corruption cannot hide an unknown replay
    // policy behind a generic payload error.
    let purpose = parse_unacked_purpose(&purpose_raw)?;
    let sequence: i64 = row
        .get(20)
        .map_err(|error| SmPersistenceError::Other(error.to_string()))?;
    let stanza_xml: String = row
        .get(21)
        .map_err(|error| SmPersistenceError::Other(error.to_string()))?;
    let receipt_ms: i64 = row
        .get(22)
        .map_err(|error| SmPersistenceError::Other(error.to_string()))?;
    decode_unacked_parts(
        key.stream_id().clone(),
        sequence,
        stanza_xml,
        receipt_ms,
        purpose,
    )
    .map(Some)
}

/// Decode rows produced by [`TERMINAL_JOIN_COLUMNS`], grouped by exact
/// `(stream_id, generation_id)` identity.
pub(crate) async fn decode_joined_rows(
    mut rows: crate::db::Rows,
    mode: TerminalDecodeMode,
) -> Result<Vec<TerminalGenerationScanEntry>, SmPersistenceError> {
    let mut output = Vec::new();
    let mut current: Option<TerminalRowGroupState> = None;
    let mut current_raw_key: Option<(String, String)> = None;

    while let Some(row) = rows
        .next()
        .await
        .map_err(|error| SmPersistenceError::Other(error.to_string()))?
    {
        let stream_id_raw: String = row
            .get(0)
            .map_err(|error| SmPersistenceError::Other(error.to_string()))?;
        let stream_id = SmSessionId::new(stream_id_raw.clone());
        let generation_id_raw: String =
            row.get(18)
                .map_err(|error| SmPersistenceError::CorruptTerminalIdentity {
                    stream_id: stream_id.clone(),
                    detail: error.to_string(),
                })?;
        let row_key = (stream_id_raw.clone(), generation_id_raw.clone());

        if current_raw_key.as_ref() != Some(&row_key) {
            if let Some(group) = current.take() {
                finish_group(group, mode, &mut output)?;
            }
            let generation_id =
                generation_id_raw
                    .parse::<SmSessionGenerationId>()
                    .map_err(|error| SmPersistenceError::CorruptTerminalIdentity {
                        stream_id: stream_id.clone(),
                        detail: error.to_string(),
                    })?;
            let key = SmTerminalGenerationKey::new(stream_id, generation_id);
            current = match (decode_session(&row), row.get::<i64>(19)) {
                (Ok(session), Ok(promotion_attempts)) => {
                    Some(TerminalRowGroupState::Healthy(TerminalRowGroup {
                        key,
                        session,
                        promotion_attempts: u32::try_from(promotion_attempts.max(0))
                            .unwrap_or(u32::MAX),
                        unacked: Vec::new(),
                    }))
                }
                (Err(error), _) => Some(TerminalRowGroupState::Poisoned {
                    key,
                    detail: error.to_string(),
                }),
                (_, Err(error)) => Some(TerminalRowGroupState::Poisoned {
                    key,
                    detail: error.to_string(),
                }),
            };
            current_raw_key = Some(row_key);
        }

        let poison = match current.as_mut() {
            Some(TerminalRowGroupState::Healthy(group)) => {
                match decode_terminal_unacked_row(&row, &group.key) {
                    Ok(Some(unacked)) => {
                        group.unacked.push(unacked);
                        None
                    }
                    Ok(None) => None,
                    Err(error) => Some((group.key.clone(), error.to_string())),
                }
            }
            Some(TerminalRowGroupState::Poisoned { .. }) => None,
            None => {
                return Err(SmPersistenceError::Other(
                    "terminal row appeared without a generation group".into(),
                ))
            }
        };
        if let Some((key, detail)) = poison {
            current = Some(TerminalRowGroupState::Poisoned { key, detail });
        }
    }

    if let Some(group) = current {
        finish_group(group, mode, &mut output)?;
    }
    Ok(output)
}

pub(crate) const TERMINAL_JOIN_COLUMNS: &str =
    "t.stream_id, t.user_id, t.full_jid, t.inbound_count, t.outbound_count, \
     t.last_acked, t.max_resume_secs, t.detached_at_ms, t.max_resume_duration_ms, \
     t.carbons_enabled, t.roster_interested, t.blocklist_interested, \
     t.presence_available, t.presence_show, t.presence_status, t.presence_priority, \
     t.replay_gap_through, t.presence_payloads, t.generation_id, t.promotion_attempts, \
     u.sequence, u.stanza_xml, u.original_receipt_at_ms, u.purpose";

pub(super) async fn get(
    storage: &DatabaseSmPersistence,
    key: &SmTerminalGenerationKey,
) -> Result<Option<PersistedTerminalGeneration>, SmPersistenceError> {
    let sql = format!(
        "SELECT {TERMINAL_JOIN_COLUMNS} \
         FROM sm_terminal_generations t \
         LEFT JOIN sm_terminal_unacked u \
           ON t.stream_id = u.stream_id AND t.generation_id = u.generation_id \
         WHERE t.stream_id = ? AND t.generation_id = ? \
         ORDER BY u.sequence ASC"
    );
    let rows = storage
        .query(
            &sql,
            crate::db_params![
                key.stream_id().as_str().to_string(),
                key.generation_id().to_string(),
            ],
        )
        .await?;
    let mut generations = decode_joined_rows(rows, TerminalDecodeMode::Strict).await?;
    match generations.pop() {
        Some(TerminalGenerationScanEntry::Persisted(terminal)) => Ok(Some(terminal)),
        Some(TerminalGenerationScanEntry::Corrupt { key, detail }) => {
            Err(SmPersistenceError::CorruptTerminal { key, detail })
        }
        None => Ok(None),
    }
}

pub(super) async fn list_all(
    storage: &DatabaseSmPersistence,
) -> Result<Vec<TerminalGenerationScanEntry>, SmPersistenceError> {
    let sql = format!(
        "SELECT {TERMINAL_JOIN_COLUMNS} \
         FROM sm_terminal_generations t \
         LEFT JOIN sm_terminal_unacked u \
           ON t.stream_id = u.stream_id AND t.generation_id = u.generation_id \
         ORDER BY t.stream_id ASC, t.generation_id ASC, u.sequence ASC"
    );
    let rows = storage.query(&sql, ()).await?;
    decode_joined_rows(rows, TerminalDecodeMode::Scan).await
}

pub(super) async fn list_for_stream(
    storage: &DatabaseSmPersistence,
    stream_id: &SmSessionId,
) -> Result<Vec<TerminalGenerationScanEntry>, SmPersistenceError> {
    let sql = format!(
        "SELECT {TERMINAL_JOIN_COLUMNS} \
         FROM sm_terminal_generations t \
         LEFT JOIN sm_terminal_unacked u \
           ON t.stream_id = u.stream_id AND t.generation_id = u.generation_id \
         WHERE t.stream_id = ? \
         ORDER BY t.generation_id ASC, u.sequence ASC"
    );
    let rows = storage
        .query(&sql, crate::db_params![stream_id.as_str().to_string()])
        .await?;
    decode_joined_rows(rows, TerminalDecodeMode::Scan).await
}

pub(super) async fn delete(
    storage: &DatabaseSmPersistence,
    key: &SmTerminalGenerationKey,
) -> Result<(), SmPersistenceError> {
    let lock = storage.lock_for(key.stream_id());
    let guard = lock.lock().await;
    let mut tx = storage
        .db
        .begin()
        .await
        .map_err(|error| SmPersistenceError::Other(error.to_string()))?;
    let params = crate::db_params![
        key.stream_id().as_str().to_string(),
        key.generation_id().to_string(),
    ];
    tx.execute(
        "DELETE FROM sm_terminal_unacked WHERE stream_id = ? AND generation_id = ?",
        params.clone(),
    )
    .await
    .map_err(|error| SmPersistenceError::Other(error.to_string()))?;
    tx.execute(
        "DELETE FROM sm_terminal_generations WHERE stream_id = ? AND generation_id = ?",
        params,
    )
    .await
    .map_err(|error| SmPersistenceError::Other(error.to_string()))?;
    tx.commit()
        .await
        .map_err(|error| SmPersistenceError::Other(error.to_string()))?;
    drop(guard);
    drop(lock);
    storage.drop_stream_lock(key.stream_id());
    Ok(())
}

pub(super) async fn delete_unacked(
    storage: &DatabaseSmPersistence,
    key: &SmTerminalGenerationKey,
    sequences: &[u32],
) -> Result<u64, SmPersistenceError> {
    let lock = storage.lock_for(key.stream_id());
    let _guard = lock.lock().await;
    let mut tx = storage
        .db
        .begin()
        .await
        .map_err(|error| SmPersistenceError::Other(error.to_string()))?;
    let mut removed = 0;
    for sequence in sequences {
        removed += tx
            .execute(
                "DELETE FROM sm_terminal_unacked \
                 WHERE stream_id = ? AND generation_id = ? AND sequence = ?",
                crate::db_params![
                    key.stream_id().as_str().to_string(),
                    key.generation_id().to_string(),
                    i64::from(*sequence),
                ],
            )
            .await
            .map_err(|error| SmPersistenceError::Other(error.to_string()))?;
    }
    tx.commit()
        .await
        .map_err(|error| SmPersistenceError::Other(error.to_string()))?;
    Ok(removed)
}

pub(super) async fn record_failure(
    storage: &DatabaseSmPersistence,
    key: &SmTerminalGenerationKey,
) -> Result<u32, SmPersistenceError> {
    let lock = storage.lock_for(key.stream_id());
    let _guard = lock.lock().await;
    let mut tx = storage
        .db
        .begin()
        .await
        .map_err(|error| SmPersistenceError::Other(error.to_string()))?;
    let updated = tx
        .execute(
            "UPDATE sm_terminal_generations \
             SET promotion_attempts = promotion_attempts + 1 \
             WHERE stream_id = ? AND generation_id = ?",
            crate::db_params![
                key.stream_id().as_str().to_string(),
                key.generation_id().to_string(),
            ],
        )
        .await
        .map_err(|error| SmPersistenceError::Other(error.to_string()))?;
    if updated == 0 {
        tx.commit()
            .await
            .map_err(|error| SmPersistenceError::Other(error.to_string()))?;
        return Ok(0);
    }
    let count = {
        let mut rows = tx
            .query(
                "SELECT promotion_attempts FROM sm_terminal_generations \
                 WHERE stream_id = ? AND generation_id = ?",
                crate::db_params![
                    key.stream_id().as_str().to_string(),
                    key.generation_id().to_string(),
                ],
            )
            .await
            .map_err(|error| SmPersistenceError::Other(error.to_string()))?;
        rows.next()
            .await
            .map_err(|error| SmPersistenceError::Other(error.to_string()))?
            .ok_or_else(|| {
                SmPersistenceError::Other(
                    "terminal generation disappeared while recording a failure".into(),
                )
            })?
            .get::<i64>(0)
            .map_err(|error| SmPersistenceError::Other(error.to_string()))?
    };
    tx.commit()
        .await
        .map_err(|error| SmPersistenceError::Other(error.to_string()))?;
    Ok(u32::try_from(count.max(0)).unwrap_or(u32::MAX))
}

pub(super) async fn has_durable_work(
    storage: &DatabaseSmPersistence,
    stream_id: &SmSessionId,
) -> Result<bool, SmPersistenceError> {
    let mut rows = storage
        .query(
            "SELECT CASE WHEN \
                 EXISTS (SELECT 1 FROM sm_sessions WHERE stream_id = ?) \
                 OR EXISTS (SELECT 1 FROM sm_terminal_generations WHERE stream_id = ?) \
             THEN 1 ELSE 0 END",
            crate::db_params![
                stream_id.as_str().to_string(),
                stream_id.as_str().to_string(),
            ],
        )
        .await?;
    let row = rows
        .next()
        .await
        .map_err(|error| SmPersistenceError::Other(error.to_string()))?
        .ok_or_else(|| SmPersistenceError::Other("durable-work query returned no row".into()))?;
    let found: i64 = row
        .get(0)
        .map_err(|error| SmPersistenceError::Other(error.to_string()))?;
    Ok(found != 0)
}
