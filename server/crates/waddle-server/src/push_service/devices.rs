//! Push device lifecycle: registration upserts with sealed provider
//! credentials, per-node quotas, disable + credential clearing, and
//! registration-time endpoint validation (SSRF gate).

use jid::BareJid;
use waddle_xmpp::XmppError;

use super::dispatch;
use super::nodes::{ensure_node_tx, get_node_tx, MAX_NODE_ID_LEN};
use super::publish_jobs::wake_queued_publish_jobs_for_node_tx;
use super::secrets::PushSecretCipher;
use super::store::{lock_node_tx, DatabasePushServiceStore};
use super::types::{
    PushDevicePlatform, PushDeviceRegistration, PushNodeStatus, PushServiceDevice,
    DEVICE_STATUS_ACTIVE, DEVICE_STATUS_DISABLED,
};

pub(super) const MAX_PUSH_DEVICES_PER_NODE: i64 = 32;

pub(super) const MAX_RETAINED_DISABLED_DEVICES_PER_NODE: i64 = 128;

pub(super) const MAX_DEVICE_ID_LEN: usize = 128;

pub(super) const MAX_ENVIRONMENT_LEN: usize = 64;

pub(super) const MAX_PROVIDER_ENDPOINT_LEN: usize = 2_048;

pub(super) const MAX_PROVIDER_TOKEN_LEN: usize = 4_096;

pub(super) const MAX_PROVIDER_KEY_MATERIAL_LEN: usize = 4_096;

fn validate_device_registration(registration: &PushDeviceRegistration) -> Result<(), XmppError> {
    validate_len(
        "Push Service device-id",
        &registration.device_id,
        MAX_DEVICE_ID_LEN,
    )?;
    validate_len("Push Service node", &registration.node, MAX_NODE_ID_LEN)?;
    validate_len(
        "Push Service environment",
        &registration.environment,
        MAX_ENVIRONMENT_LEN,
    )?;
    validate_optional_len(
        "Push Service provider endpoint",
        registration.provider_endpoint.as_deref(),
        MAX_PROVIDER_ENDPOINT_LEN,
    )?;
    if registration.platform == PushDevicePlatform::Web {
        if let Some(endpoint) = registration.provider_endpoint.as_deref() {
            validate_web_push_endpoint(endpoint)?;
        }
    }
    validate_optional_len(
        "Push Service provider token",
        registration.provider_token.as_deref(),
        MAX_PROVIDER_TOKEN_LEN,
    )?;
    validate_optional_len(
        "Push Service provider key material",
        registration.provider_key_material.as_deref(),
        MAX_PROVIDER_KEY_MATERIAL_LEN,
    )?;
    Ok(())
}

/// Reject Web Push endpoints that would let a registering client
/// weaponize the publish-job worker as an SSRF probe against
/// internal-network hosts. Accepted shape: `https://<host>/...` where
/// `<host>` is a non-IP domain whose lowest label is not
/// `localhost`. Literal IP addresses (any family) are rejected
/// outright — legitimate relays use named hosts and DNS handles their
/// addressing.
///
/// Note: this is register-time only; it does not prevent DNS-rebinding
/// at delivery time. A full mitigation requires inspecting the
/// resolved peer address inside the reqwest connection callback
/// (deferred to a follow-up PR).
fn validate_web_push_endpoint(endpoint: &str) -> Result<(), XmppError> {
    let parsed = url::Url::parse(endpoint).map_err(|_| {
        XmppError::bad_request(Some(
            "Push Service provider endpoint is not a valid URL".to_string(),
        ))
    })?;
    if parsed.scheme() != "https" {
        return Err(XmppError::bad_request(Some(
            "Push Service provider endpoint must use https".to_string(),
        )));
    }
    // Port restriction intentionally omitted: RFC 8030 / RFC 8292
    // place no constraint on the Web Push endpoint's port. VAPID
    // `aud` per RFC 8292 §2 is computed from the URL's origin
    // (scheme + host + port), so a legitimate self-hosted Mozilla
    // autopush relay on `:8443` MUST round-trip through this gate
    // for VAPID signing to produce the right `aud` claim. A
    // hard-coded `:443` requirement would reject those conformant
    // deployments. SSRF protection is layered:
    //
    //   1. Scheme check above (`https` only) prevents `http://` to
    //      anything.
    //   2. The literal-IP and `*.localhost` rejections below block
    //      the obvious internal targets regardless of port.
    //   3. Network-level egress policy (out of scope here) is the
    //      proper defence against SSRF to internal DNS names on
    //      arbitrary ports — the application layer cannot enumerate
    //      every internal service.
    let host = parsed.host_str().ok_or_else(|| {
        XmppError::bad_request(Some(
            "Push Service provider endpoint must include a host".to_string(),
        ))
    })?;
    // Reject `localhost` and the IPv6 equivalent regardless of how the
    // operator's resolver maps them.
    let host_lower = host.to_ascii_lowercase();
    if host_lower == "localhost" || host_lower.ends_with(".localhost") {
        return Err(XmppError::bad_request(Some(
            "Push Service provider endpoint must not target localhost".to_string(),
        )));
    }
    // Literal IPs: any family. `url::Host` already classifies them.
    match parsed.host() {
        Some(url::Host::Ipv4(_)) | Some(url::Host::Ipv6(_)) => Err(XmppError::bad_request(Some(
            "Push Service provider endpoint must be a hostname, not an IP literal".to_string(),
        ))),
        _ => Ok(()),
    }
}

fn validate_optional_len(field: &str, value: Option<&str>, max: usize) -> Result<(), XmppError> {
    if let Some(value) = value {
        validate_len(field, value, max)?;
    }
    Ok(())
}

pub(super) fn validate_len(field: &str, value: &str, max: usize) -> Result<(), XmppError> {
    if value.len() > max {
        return Err(XmppError::bad_request(Some(format!(
            "{field} exceeds {max} bytes"
        ))));
    }
    Ok(())
}

async fn active_device_exists_on_node_tx(
    tx: &mut crate::db::Transaction<'_>,
    node: &str,
    device_id: &str,
) -> Result<bool, XmppError> {
    let mut rows = tx
        .query(
            "SELECT 1 FROM push_devices WHERE node = ? AND device_id = ? AND status = ? LIMIT 1",
            crate::db_params![node, device_id, DEVICE_STATUS_ACTIVE],
        )
        .await
        .map_err(|error| XmppError::internal(error.to_string()))?;
    Ok(rows
        .next()
        .await
        .map_err(|error| XmppError::internal(error.to_string()))?
        .is_some())
}

pub(super) async fn count_active_devices_for_node_tx(
    tx: &mut crate::db::Transaction<'_>,
    node: &str,
) -> Result<i64, XmppError> {
    let mut rows = tx
        .query(
            "SELECT COUNT(*) FROM push_devices WHERE node = ? AND status = ?",
            crate::db_params![node, DEVICE_STATUS_ACTIVE],
        )
        .await
        .map_err(|error| XmppError::internal(error.to_string()))?;
    let row = rows
        .next()
        .await
        .map_err(|error| XmppError::internal(error.to_string()))?
        .ok_or_else(|| XmppError::internal("Push device count query returned no row"))?;
    row.get(0)
        .map_err(|error| XmppError::internal(error.to_string()))
}

async fn prune_disabled_devices_for_node_tx(
    tx: &mut crate::db::Transaction<'_>,
    node: &str,
    retain: i64,
) -> Result<(), XmppError> {
    let mut rows = tx
        .query(
            r#"
            SELECT device_id
            FROM push_devices
            WHERE node = ? AND status = ?
            ORDER BY updated_at_ms DESC, device_id DESC
            "#,
            crate::db_params![node, DEVICE_STATUS_DISABLED],
        )
        .await
        .map_err(|error| XmppError::internal(error.to_string()))?;
    let mut seen = 0_i64;
    let mut stale_devices = Vec::new();
    while let Some(row) = rows
        .next()
        .await
        .map_err(|error| XmppError::internal(error.to_string()))?
    {
        seen += 1;
        if seen > retain {
            stale_devices.push(
                row.get::<String>(0)
                    .map_err(|error| XmppError::internal(error.to_string()))?,
            );
        }
    }
    for device_id in stale_devices {
        tx.execute(
            "DELETE FROM push_devices WHERE node = ? AND device_id = ? AND status = ?",
            crate::db_params![node, device_id, DEVICE_STATUS_DISABLED],
        )
        .await
        .map_err(|error| XmppError::internal(error.to_string()))?;
    }
    Ok(())
}

pub(super) async fn active_devices_with_subscription_for_node_tx(
    tx: &mut crate::db::Transaction<'_>,
    node: &str,
) -> Result<Vec<dispatch::SealedActiveDevice>, XmppError> {
    let mut rows = tx
        .query(
            r#"
            SELECT device_id, platform, provider_endpoint, provider_token, provider_key_material
            FROM push_devices
            WHERE node = ? AND status = ?
            ORDER BY device_id ASC
            "#,
            crate::db_params![node, DEVICE_STATUS_ACTIVE],
        )
        .await
        .map_err(|error| XmppError::internal(error.to_string()))?;
    let mut devices = Vec::new();
    while let Some(row) = rows
        .next()
        .await
        .map_err(|error| XmppError::internal(error.to_string()))?
    {
        let device_id: String = row
            .get(0)
            .map_err(|error| XmppError::internal(error.to_string()))?;
        let platform = PushDevicePlatform::parse(
            &row.get::<String>(1)
                .map_err(|error| XmppError::internal(error.to_string()))?,
        )?;
        let sealed_endpoint: Option<String> = row
            .get(2)
            .map_err(|error| XmppError::internal(error.to_string()))?;
        let sealed_auth: Option<String> = row
            .get(3)
            .map_err(|error| XmppError::internal(error.to_string()))?;
        let sealed_key_material: Option<String> = row
            .get(4)
            .map_err(|error| XmppError::internal(error.to_string()))?;
        devices.push(dispatch::SealedActiveDevice {
            device_id,
            platform,
            sealed_endpoint,
            sealed_auth,
            sealed_key_material,
        });
    }
    Ok(devices)
}

/// XEP-0357 §6 forward cleanup: flip a `push_devices` row from
/// `active` to `disabled` when its push subscription is permanently
/// unusable (relay returned 404/410, stored keys are structurally
/// invalid, etc.). Idempotent — already-disabled rows stay disabled.
/// `reason` is stored on the row as `last_error` so an operator can
/// see which permanent-failure outcome triggered the disable.
pub(super) async fn mark_device_disabled_tx(
    tx: &mut crate::db::Transaction<'_>,
    node: &str,
    device_id: &str,
    reason: &str,
    now_ms: i64,
) -> Result<(), XmppError> {
    tx.execute(
        r#"
        UPDATE push_devices
        SET status = ?,
            failure_count = failure_count + 1,
            last_error = ?,
            updated_at_ms = ?
        WHERE node = ? AND device_id = ? AND status = ?
        "#,
        crate::db_params![
            DEVICE_STATUS_DISABLED,
            reason,
            now_ms,
            node,
            device_id,
            DEVICE_STATUS_ACTIVE,
        ],
    )
    .await
    .map_err(|error| XmppError::internal(error.to_string()))?;
    Ok(())
}

async fn get_device_for_owner_on_node_tx(
    tx: &mut crate::db::Transaction<'_>,
    owner_bare_jid: &BareJid,
    node: &str,
    device_id: &str,
    secrets: &PushSecretCipher,
) -> Result<Option<PushServiceDevice>, XmppError> {
    let mut rows = tx
        .query(
            r#"
            SELECT d.device_id, d.node, d.platform, d.environment,
                   d.provider_endpoint, d.provider_token, d.provider_key_material
            FROM push_devices d
            JOIN push_nodes n ON n.node = d.node
            WHERE n.owner_bare_jid = ? AND d.node = ? AND d.device_id = ?
            "#,
            crate::db_params![owner_bare_jid.to_string(), node, device_id],
        )
        .await
        .map_err(|error| XmppError::internal(error.to_string()))?;
    let Some(row) = rows
        .next()
        .await
        .map_err(|error| XmppError::internal(error.to_string()))?
    else {
        return Ok(None);
    };
    Ok(Some(decode_device(&row, secrets)?))
}

fn decode_device(
    row: &crate::db::Row,
    secrets: &PushSecretCipher,
) -> Result<PushServiceDevice, XmppError> {
    let provider_endpoint = secrets.open_optional(
        row.get(4)
            .map_err(|error| XmppError::internal(error.to_string()))?,
    )?;
    #[cfg(test)]
    let provider_token = secrets.open_optional(
        row.get(5)
            .map_err(|error| XmppError::internal(error.to_string()))?,
    )?;
    #[cfg(test)]
    let provider_key_material = secrets.open_optional(
        row.get(6)
            .map_err(|error| XmppError::internal(error.to_string()))?,
    )?;
    Ok(PushServiceDevice {
        device_id: row
            .get(0)
            .map_err(|error| XmppError::internal(error.to_string()))?,
        node: row
            .get(1)
            .map_err(|error| XmppError::internal(error.to_string()))?,
        platform: PushDevicePlatform::parse(
            &row.get::<String>(2)
                .map_err(|error| XmppError::internal(error.to_string()))?,
        )?,
        environment: row
            .get(3)
            .map_err(|error| XmppError::internal(error.to_string()))?,
        provider_endpoint,
        #[cfg(test)]
        provider_token,
        #[cfg(test)]
        provider_key_material,
    })
}

pub(super) async fn upsert_device_tx(
    tx: &mut crate::db::Transaction<'_>,
    owner_bare_jid: &BareJid,
    registration: PushDeviceRegistration,
    secrets: &PushSecretCipher,
    now_ms: i64,
) -> Result<PushServiceDevice, XmppError> {
    if registration.device_id.is_empty()
        || registration.node.is_empty()
        || registration.environment.is_empty()
    {
        return Err(XmppError::bad_request(Some(
            "Push Service device-id, node, and environment are required".to_string(),
        )));
    }
    validate_device_registration(&registration)?;
    if get_node_tx(tx, &registration.node).await?.is_none() {
        return Err(XmppError::item_not_found(Some(
            "Push node not found".to_string(),
        )));
    }
    lock_node_tx(tx, &registration.node, now_ms).await?;
    let push_node = get_node_tx(tx, &registration.node)
        .await?
        .ok_or_else(|| XmppError::internal("Push Service node was not persisted"))?;
    if push_node.owner_bare_jid != *owner_bare_jid {
        return Err(XmppError::forbidden(Some(
            "Push node belongs to another user".to_string(),
        )));
    }
    if push_node.status != PushNodeStatus::Active {
        return Err(XmppError::item_not_found(Some(
            "Push node not active".to_string(),
        )));
    }
    prune_disabled_devices_for_node_tx(
        tx,
        &registration.node,
        MAX_RETAINED_DISABLED_DEVICES_PER_NODE,
    )
    .await?;
    if !active_device_exists_on_node_tx(tx, &registration.node, &registration.device_id).await?
        && count_active_devices_for_node_tx(tx, &registration.node).await?
            >= MAX_PUSH_DEVICES_PER_NODE
    {
        return Err(XmppError::bad_request(Some(format!(
            "Push Service active device quota exceeded; max {MAX_PUSH_DEVICES_PER_NODE} active devices per node"
        ))));
    }

    let sealed_provider_endpoint = secrets.seal_optional(registration.provider_endpoint.clone())?;
    let sealed_provider_token = secrets.seal_optional(registration.provider_token.clone())?;
    let sealed_provider_key_material =
        secrets.seal_optional(registration.provider_key_material.clone())?;
    let device_id = registration.device_id.clone();
    tx.execute(
        r#"
        INSERT INTO push_devices (
            device_id,
            node,
            platform,
            environment,
            provider_endpoint,
            provider_token,
            provider_key_material,
            status,
            failure_count,
            last_error,
            created_at_ms,
            updated_at_ms
        ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, 0, NULL, ?, ?)
        ON CONFLICT(node, device_id) DO UPDATE SET
            platform = excluded.platform,
            environment = excluded.environment,
            provider_endpoint = excluded.provider_endpoint,
            provider_token = excluded.provider_token,
            provider_key_material = excluded.provider_key_material,
            status = excluded.status,
            failure_count = 0,
            last_error = NULL,
            updated_at_ms = excluded.updated_at_ms
        "#,
        crate::db_params![
            device_id.clone(),
            registration.node.clone(),
            registration.platform.to_string(),
            registration.environment.clone(),
            sealed_provider_endpoint,
            sealed_provider_token,
            sealed_provider_key_material,
            DEVICE_STATUS_ACTIVE,
            now_ms,
            now_ms,
        ],
    )
    .await
    .map_err(|error| XmppError::internal(error.to_string()))?;
    wake_queued_publish_jobs_for_node_tx(tx, &registration.node, now_ms).await?;
    get_device_for_owner_on_node_tx(tx, owner_bare_jid, &registration.node, &device_id, secrets)
        .await?
        .ok_or_else(|| XmppError::internal("Push Service device was not persisted"))
}

impl DatabasePushServiceStore {
    pub async fn upsert_device(
        &self,
        owner_bare_jid: &BareJid,
        registration: PushDeviceRegistration,
    ) -> Result<PushServiceDevice, XmppError> {
        let now_ms = crate::time::now_ms();
        let mut tx = self
            .db
            .begin_immediate()
            .await
            .map_err(|error| XmppError::internal(error.to_string()))?;
        let device =
            upsert_device_tx(&mut tx, owner_bare_jid, registration, &self.secrets, now_ms).await?;
        tx.commit()
            .await
            .map_err(|error| XmppError::internal(error.to_string()))?;
        Ok(device)
    }

    pub async fn register_device_for_owner(
        &self,
        owner_bare_jid: &BareJid,
        app_id: &str,
        registration_for_node: impl FnOnce(&str) -> PushDeviceRegistration,
    ) -> Result<(super::types::PushServiceNode, PushServiceDevice), XmppError> {
        let now_ms = crate::time::now_ms();
        let mut tx = self
            .db
            .begin_immediate()
            .await
            .map_err(|error| XmppError::internal(error.to_string()))?;
        let node = ensure_node_tx(&mut tx, owner_bare_jid, app_id, now_ms).await?;
        let registration = registration_for_node(node.node());
        let device =
            upsert_device_tx(&mut tx, owner_bare_jid, registration, &self.secrets, now_ms).await?;
        tx.commit()
            .await
            .map_err(|error| XmppError::internal(error.to_string()))?;
        if let Err(error) = self
            .ensure_xep0060_push_node_for_owner(owner_bare_jid, node.node())
            .await
        {
            let _ = self
                .disable_device_for_owner(
                    owner_bare_jid,
                    node.node(),
                    device.device_id(),
                    Some("failed to provision XEP-0060 Push Service node"),
                )
                .await;
            return Err(error);
        }
        Ok((node, device))
    }

    /// Disable a single device on an owner's push node, clearing its
    /// provider credentials.
    ///
    /// Returns:
    /// - `Err(item-not-found)` if the node does not exist, **or** if no
    ///   `(node, device-id)` row exists for it. The two are deliberately
    ///   indistinguishable on the wire — both mean "there is nothing here
    ///   to disable" — so a caller submitting a stale/typo'd device-id is
    ///   told so rather than receiving a misleading success.
    /// - `Err(forbidden)` if the node belongs to another user.
    /// - `Ok(())` if a matching device row existed and is now `disabled`.
    ///   Idempotent: re-disabling an already-disabled device succeeds
    ///   (the row still matches), and disabling a device on a no-longer-
    ///   active node still flips that device row.
    pub async fn disable_device_for_owner(
        &self,
        owner_bare_jid: &BareJid,
        node: &str,
        device_id: &str,
        error: Option<&str>,
    ) -> Result<(), XmppError> {
        validate_len("Push Service node", node, MAX_NODE_ID_LEN)?;
        validate_len("Push Service device-id", device_id, MAX_DEVICE_ID_LEN)?;
        let now_ms = crate::time::now_ms();
        let mut tx = self
            .db
            .begin_immediate()
            .await
            .map_err(|error| XmppError::internal(error.to_string()))?;
        if get_node_tx(&mut tx, node).await?.is_none() {
            return Err(XmppError::item_not_found(Some(
                "Push node not found".to_string(),
            )));
        }
        lock_node_tx(&mut tx, node, now_ms).await?;
        let push_node = get_node_tx(&mut tx, node)
            .await?
            .ok_or_else(|| XmppError::internal("Push Service node was not persisted"))?;
        if push_node.owner_bare_jid != *owner_bare_jid {
            return Err(XmppError::forbidden(Some(
                "Push node belongs to another user".to_string(),
            )));
        }
        let affected = tx
            .execute(
                r#"
                UPDATE push_devices
                SET status = ?,
                    provider_endpoint = NULL,
                    provider_token = NULL,
                    provider_key_material = NULL,
                    last_error = ?,
                    updated_at_ms = ?
                WHERE device_id = ?
                  AND node = ?
                "#,
                crate::db_params![
                    DEVICE_STATUS_DISABLED,
                    error.map(str::to_owned),
                    now_ms,
                    device_id,
                    node,
                ],
            )
            .await
            .map_err(|error| XmppError::internal(error.to_string()))?;
        tx.commit()
            .await
            .map_err(|error| XmppError::internal(error.to_string()))?;
        // `affected == 0` ⇒ no row matched `(node, device-id)`. The node
        // exists (checked above) and is owned by the caller, so the
        // device-id itself is unknown — surface item-not-found rather
        // than a false success (XEP-0050 §4.4 NONE-condition item-not-found,
        // symmetric with the node-not-found path above).
        if affected == 0 {
            return Err(XmppError::item_not_found(Some(
                "Push device not found on this node".to_string(),
            )));
        }
        Ok(())
    }

    #[cfg(test)]
    pub async fn get_device_for_owner(
        &self,
        owner_bare_jid: &BareJid,
        device_id: &str,
    ) -> Result<Option<PushServiceDevice>, XmppError> {
        let mut rows = self
            .query(
                r#"
                SELECT d.device_id, d.node, d.platform, d.environment,
                       d.provider_endpoint, d.provider_token, d.provider_key_material
                FROM push_devices d
                JOIN push_nodes n ON n.node = d.node
                WHERE n.owner_bare_jid = ? AND d.device_id = ?
                ORDER BY d.node ASC
                LIMIT 1
                "#,
                crate::db_params![owner_bare_jid.to_string(), device_id],
            )
            .await?;
        let Some(row) = rows
            .next()
            .await
            .map_err(|error| XmppError::internal(error.to_string()))?
        else {
            return Ok(None);
        };
        Ok(Some(decode_device(&row, &self.secrets)?))
    }

    /// Test-only: list every ACTIVE `push_devices` row registered
    /// against the given `(owner, node)` pair. Used by the XEP-0050
    /// cutover tests as a convenience for sites that only need to
    /// assert "a device exists" for an owner/node without pinning the
    /// specific server-assigned `device_id`. (The chat client DOES
    /// learn `device_id` — the stage-4 `register-device` result form
    /// returns it and the per-device `disable-device` opt-out requires
    /// it; tests that care about the exact id read it from that result
    /// form instead.)
    #[cfg(test)]
    pub async fn test_only_active_devices_for_owner_node(
        &self,
        owner_bare_jid: &BareJid,
        node: &str,
    ) -> Result<Vec<PushServiceDevice>, XmppError> {
        let mut rows = self
            .query(
                r#"
                SELECT d.device_id, d.node, d.platform, d.environment,
                       d.provider_endpoint, d.provider_token, d.provider_key_material
                FROM push_devices d
                JOIN push_nodes n ON n.node = d.node
                WHERE n.owner_bare_jid = ? AND d.node = ? AND d.status = ?
                ORDER BY d.created_at_ms ASC
                "#,
                crate::db_params![owner_bare_jid.to_string(), node, DEVICE_STATUS_ACTIVE],
            )
            .await?;
        let mut devices = Vec::new();
        while let Some(row) = rows
            .next()
            .await
            .map_err(|error| XmppError::internal(error.to_string()))?
        {
            devices.push(decode_device(&row, &self.secrets)?);
        }
        Ok(devices)
    }

    #[cfg(test)]
    pub(super) async fn get_device_for_owner_on_node(
        &self,
        owner_bare_jid: &BareJid,
        node: &str,
        device_id: &str,
    ) -> Result<Option<PushServiceDevice>, XmppError> {
        let mut rows = self
            .query(
                r#"
                SELECT d.device_id, d.node, d.platform, d.environment,
                       d.provider_endpoint, d.provider_token, d.provider_key_material
                FROM push_devices d
                JOIN push_nodes n ON n.node = d.node
                WHERE n.owner_bare_jid = ? AND d.node = ? AND d.device_id = ?
                "#,
                crate::db_params![owner_bare_jid.to_string(), node, device_id],
            )
            .await?;
        let Some(row) = rows
            .next()
            .await
            .map_err(|error| XmppError::internal(error.to_string()))?
        else {
            return Ok(None);
        };
        Ok(Some(decode_device(&row, &self.secrets)?))
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use async_trait::async_trait;
    use jid::Jid;
    use waddle_xmpp::pubsub::{
        Affiliation, InMemoryPubSubStorage, NodeConfig, PubSubItem, PubSubNode, PubSubStorage,
        PublishResult, StoredItem, SubId, Subscription,
    };
    use waddle_xmpp::push::{PushSubscription, PushSubscriptionStore};

    use crate::db::Database;
    use crate::push_service::secrets::SEALED_PROVIDER_VALUE_PREFIX;
    use crate::push_service::test_support::{
        assert_item_not_found, notification_item, owner, store,
    };

    struct FailingNodeConfigPubSubStorage {
        inner: InMemoryPubSubStorage,
    }

    impl FailingNodeConfigPubSubStorage {
        fn new() -> Self {
            Self {
                inner: InMemoryPubSubStorage::new(),
            }
        }
    }

    #[async_trait]
    impl PubSubStorage for FailingNodeConfigPubSubStorage {
        async fn get_or_create_node(
            &self,
            owner: &BareJid,
            node_name: &str,
        ) -> Result<(PubSubNode, bool), XmppError> {
            self.inner.get_or_create_node(owner, node_name).await
        }

        async fn get_node(
            &self,
            owner: &BareJid,
            node_name: &str,
        ) -> Result<Option<PubSubNode>, XmppError> {
            self.inner.get_node(owner, node_name).await
        }

        async fn delete_node(&self, owner: &BareJid, node_name: &str) -> Result<bool, XmppError> {
            self.inner.delete_node(owner, node_name).await
        }

        async fn publish_item(
            &self,
            owner: &BareJid,
            node_name: &str,
            item: &PubSubItem,
            publisher: Option<&BareJid>,
            auto_create: bool,
        ) -> Result<PublishResult, XmppError> {
            self.inner
                .publish_item(owner, node_name, item, publisher, auto_create)
                .await
        }

        async fn publish_item_if_missing_or_publisher(
            &self,
            owner: &BareJid,
            node_name: &str,
            item: &PubSubItem,
            publisher: &BareJid,
            auto_create: bool,
        ) -> Result<PublishResult, XmppError> {
            self.inner
                .publish_item_if_missing_or_publisher(
                    owner,
                    node_name,
                    item,
                    publisher,
                    auto_create,
                )
                .await
        }

        async fn get_items(
            &self,
            owner: &BareJid,
            node_name: &str,
            max_items: Option<u32>,
            item_ids: &[String],
        ) -> Result<Vec<StoredItem>, XmppError> {
            self.inner
                .get_items(owner, node_name, max_items, item_ids)
                .await
        }

        async fn retract_item(
            &self,
            owner: &BareJid,
            node_name: &str,
            item_id: &str,
        ) -> Result<bool, XmppError> {
            self.inner.retract_item(owner, node_name, item_id).await
        }

        async fn list_nodes(&self, owner: &BareJid) -> Result<Vec<String>, XmppError> {
            self.inner.list_nodes(owner).await
        }

        async fn find_node_for_item(
            &self,
            owner: &BareJid,
            item_id: &str,
        ) -> Result<Option<PubSubNode>, XmppError> {
            self.inner.find_node_for_item(owner, item_id).await
        }

        async fn list_node_names_for_item(
            &self,
            owner: &BareJid,
            item_id: &str,
        ) -> Result<Vec<String>, XmppError> {
            self.inner.list_node_names_for_item(owner, item_id).await
        }

        async fn update_node_config(
            &self,
            _owner: &BareJid,
            _node_name: &str,
            _config: &NodeConfig,
        ) -> Result<(), XmppError> {
            Err(XmppError::internal("forced XEP-0060 node config failure"))
        }

        async fn purge_node(&self, owner: &BareJid, node_name: &str) -> Result<u64, XmppError> {
            self.inner.purge_node(owner, node_name).await
        }

        async fn subscribe(
            &self,
            owner: &BareJid,
            node_name: &str,
            subscriber: &Jid,
        ) -> Result<Subscription, XmppError> {
            self.inner.subscribe(owner, node_name, subscriber).await
        }

        async fn unsubscribe(
            &self,
            owner: &BareJid,
            node_name: &str,
            subscriber: &Jid,
            subid: Option<&SubId>,
        ) -> Result<bool, XmppError> {
            self.inner
                .unsubscribe(owner, node_name, subscriber, subid)
                .await
        }

        async fn list_node_subscriptions(
            &self,
            owner: &BareJid,
            node_name: &str,
        ) -> Result<Vec<Subscription>, XmppError> {
            self.inner.list_node_subscriptions(owner, node_name).await
        }

        async fn list_subscriber_subscriptions(
            &self,
            owner: &BareJid,
            subscriber: &Jid,
        ) -> Result<Vec<(String, Subscription)>, XmppError> {
            self.inner
                .list_subscriber_subscriptions(owner, subscriber)
                .await
        }

        async fn get_subscription(
            &self,
            owner: &BareJid,
            node_name: &str,
            subid: &SubId,
        ) -> Result<Option<Subscription>, XmppError> {
            self.inner.get_subscription(owner, node_name, subid).await
        }

        async fn list_deliverable_subscribers(
            &self,
            owner: &BareJid,
            node_name: &str,
        ) -> Result<Vec<Subscription>, XmppError> {
            self.inner
                .list_deliverable_subscribers(owner, node_name)
                .await
        }

        async fn set_affiliation(
            &self,
            owner: &BareJid,
            node_name: &str,
            entity: &BareJid,
            affiliation: Affiliation,
        ) -> Result<Affiliation, XmppError> {
            self.inner
                .set_affiliation(owner, node_name, entity, affiliation)
                .await
        }

        async fn get_affiliation(
            &self,
            owner: &BareJid,
            node_name: &str,
            entity: &BareJid,
        ) -> Result<Affiliation, XmppError> {
            self.inner.get_affiliation(owner, node_name, entity).await
        }

        async fn list_node_affiliations(
            &self,
            owner: &BareJid,
            node_name: &str,
        ) -> Result<Vec<(BareJid, Affiliation)>, XmppError> {
            self.inner.list_node_affiliations(owner, node_name).await
        }

        async fn list_entity_affiliations(
            &self,
            owner: &BareJid,
            entity: &BareJid,
        ) -> Result<Vec<(String, Affiliation)>, XmppError> {
            self.inner.list_entity_affiliations(owner, entity).await
        }
    }

    #[tokio::test]
    async fn provider_credentials_live_in_push_service_not_xep0357_registration_store() {
        let store = store().await;
        let owner = owner();
        let node = store.ensure_node(&owner, "web").await.expect("push node");
        let device = store
            .upsert_device(
                &owner,
                PushDeviceRegistration::new("web-1", node.node(), PushDevicePlatform::Web, "test")
                    .with_provider_endpoint(Some("https://push.example.com/endpoint".to_string()))
                    .with_provider_token(Some("provider-secret".to_string()))
                    .with_provider_key_material(Some("provider-key".to_string())),
            )
            .await
            .expect("device");

        assert_eq!(
            device.provider_endpoint(),
            Some("https://push.example.com/endpoint")
        );
        assert_eq!(device.provider_token(), Some("provider-secret"));
        assert_eq!(device.provider_key_material(), Some("provider-key"));

        let mut raw_rows = store
            .query(
                "SELECT provider_endpoint, provider_token, provider_key_material \
                 FROM push_devices WHERE node = ? AND device_id = ?",
                crate::db_params![node.node(), "web-1"],
            )
            .await
            .expect("raw provider secret query");
        let raw_row = raw_rows
            .next()
            .await
            .expect("raw provider secret row")
            .expect("raw provider secret row");
        for idx in 0..3 {
            let raw: String = raw_row.get(idx).expect("sealed provider secret column");
            assert!(
                raw.starts_with(SEALED_PROVIDER_VALUE_PREFIX),
                "provider secret should be sealed at rest: {raw}"
            );
            assert!(
                !raw.contains("push.example.com/endpoint")
                    && !raw.contains("provider-secret")
                    && !raw.contains("provider-key"),
                "provider secret should not be persisted in plaintext: {raw}"
            );
        }

        let registration_store =
            crate::push_registrations::DatabasePushRegistrationStore::new(store.database())
                .await
                .expect("registration store");
        registration_store
            .register(PushSubscription {
                user_jid: owner.to_string(),
                service_jid: "push.example.com".to_string(),
                node: Some(node.node().to_string()),
                publish_options: None,
                endpoint: Some("https://legacy.example.com/should-not-persist".to_string()),
                p256dh: Some("legacy-key".to_string()),
                auth_key: Some("legacy-auth".to_string()),
            })
            .await
            .expect("register");
        let registrations = registration_store
            .get_for_user(&owner.to_string())
            .await
            .expect("registrations");

        assert_eq!(registrations.len(), 1);
        assert!(registrations[0].endpoint.is_none());
        assert!(registrations[0].p256dh.is_none());
        assert!(registrations[0].auth_key.is_none());
    }

    #[tokio::test]
    async fn register_device_for_owner_rolls_back_node_when_device_insert_fails() {
        let store = store().await;
        let owner = owner();
        store
            .execute(
                r#"
                CREATE TRIGGER fail_push_device_insert
                BEFORE INSERT ON push_devices
                BEGIN
                    SELECT RAISE(ABORT, 'forced push device insert failure');
                END
                "#,
                (),
            )
            .await
            .expect("failure trigger");

        store
            .register_device_for_owner(&owner, "web", |node| {
                PushDeviceRegistration::new("web-1", node, PushDevicePlatform::Web, "test")
            })
            .await
            .expect_err("device insert trigger aborts registration");

        assert!(
            store
                .list_node_names_for_owner(&owner)
                .await
                .expect("node list")
                .is_empty(),
            "node insert must roll back with the failed device insert"
        );
        assert!(
            store
                .get_device_for_owner(&owner, "web-1")
                .await
                .expect("device lookup")
                .is_none(),
            "device insert failed, so no device row should exist"
        );
    }

    #[tokio::test]
    async fn register_device_for_owner_allows_disable_after_combined_commit() {
        let store = store().await;
        let owner = owner();
        let (node, device) = store
            .register_device_for_owner(&owner, "web", |node| {
                PushDeviceRegistration::new("web-1", node, PushDevicePlatform::Web, "test")
                    .with_provider_token(Some("provider-secret".to_string()))
            })
            .await
            .expect("register device");

        assert_eq!(device.node(), node.node());
        store
            .disable_nodes_for_owner(&owner, Some(node.node()))
            .await
            .expect("disable node after combined registration commit");
        assert_item_not_found(
            store
                .upsert_device(
                    &owner,
                    PushDeviceRegistration::new(
                        "web-2",
                        node.node(),
                        PushDevicePlatform::Web,
                        "test",
                    ),
                )
                .await
                .expect_err("disabled node rejects later device registration"),
        );
    }

    #[tokio::test]
    async fn register_device_for_owner_compensates_device_when_pubsub_backing_fails() {
        let owner = owner();
        let store = DatabasePushServiceStore::new_with_secret_key_and_pubsub(
            Database::in_memory("push-service-failing-pubsub")
                .await
                .expect("push service db"),
            b"waddle-push-service-test-secret-key",
            "push.example.com".parse().expect("push service jid"),
            Arc::new(FailingNodeConfigPubSubStorage::new()),
        )
        .await
        .expect("push service store");

        store
            .register_device_for_owner(&owner, "web", |node| {
                PushDeviceRegistration::new("web-1", node, PushDevicePlatform::Web, "test")
                    .with_provider_token(Some("provider-secret".to_string()))
            })
            .await
            .expect_err("post-commit XEP-0060 provisioning fails");

        let nodes = store
            .list_node_names_for_owner(&owner)
            .await
            .expect("node list");
        assert_eq!(nodes.len(), 1, "node commit should match ensure_node shape");
        assert!(
            store
                .test_only_active_devices_for_owner_node(&owner, &nodes[0])
                .await
                .expect("active device list")
                .is_empty(),
            "failed post-commit provisioning must not leave an active device"
        );
        let compensated_device = store
            .get_device_for_owner_on_node(&owner, &nodes[0], "web-1")
            .await
            .expect("compensated device lookup")
            .expect("compensated disabled device");
        assert_eq!(compensated_device.provider_token(), None);
    }

    #[tokio::test]
    async fn device_ids_are_scoped_to_push_node_not_global_user_boundary() {
        let store = store().await;
        let alice: BareJid = "alice@example.com".parse().expect("alice");
        let bob: BareJid = "bob@example.com".parse().expect("bob");
        let alice_node = store.ensure_node(&alice, "web").await.expect("alice node");
        let bob_node = store.ensure_node(&bob, "web").await.expect("bob node");

        store
            .upsert_device(
                &alice,
                PushDeviceRegistration::new(
                    "shared-device-id",
                    alice_node.node(),
                    PushDevicePlatform::Web,
                    "test",
                )
                .with_provider_token(Some("alice-secret".to_string())),
            )
            .await
            .expect("alice device");
        store
            .upsert_device(
                &bob,
                PushDeviceRegistration::new(
                    "shared-device-id",
                    bob_node.node(),
                    PushDevicePlatform::Web,
                    "test",
                )
                .with_provider_token(Some("bob-secret".to_string())),
            )
            .await
            .expect("bob device");

        let alice_device = store
            .get_device_for_owner_on_node(&alice, alice_node.node(), "shared-device-id")
            .await
            .expect("alice lookup")
            .expect("alice device");
        let bob_device = store
            .get_device_for_owner_on_node(&bob, bob_node.node(), "shared-device-id")
            .await
            .expect("bob lookup")
            .expect("bob device");

        assert_eq!(alice_device.provider_token(), Some("alice-secret"));
        assert_eq!(bob_device.provider_token(), Some("bob-secret"));
        assert_eq!(alice_device.node(), alice_node.node());
        assert_eq!(bob_device.node(), bob_node.node());
    }

    #[tokio::test]
    async fn disable_device_is_scoped_to_one_push_node() {
        let store = store().await;
        let owner = owner();
        let first_node = store.ensure_node(&owner, "web").await.expect("first node");
        let second_node = store
            .ensure_node(&owner, "mobile")
            .await
            .expect("second node");
        for node in [first_node.node(), second_node.node()] {
            store
                .upsert_device(
                    &owner,
                    PushDeviceRegistration::new(
                        "shared-device-id",
                        node,
                        PushDevicePlatform::Web,
                        "test",
                    )
                    .with_provider_endpoint(Some("https://push.example.com/endpoint".to_string()))
                    .with_provider_token(Some("provider-secret".to_string()))
                    .with_provider_key_material(Some("provider-key".to_string())),
                )
                .await
                .expect("device");
        }

        store
            .disable_device_for_owner(&owner, first_node.node(), "shared-device-id", None)
            .await
            .expect("disable first node device");

        let first_result = store
            .publish_notification_from_user_server(
                first_node.node(),
                &notification_item("push-first"),
                &owner,
            )
            .await
            .expect("first publish");
        let second_result = store
            .publish_notification_from_user_server(
                second_node.node(),
                &notification_item("push-second"),
                &owner,
            )
            .await
            .expect("second publish");

        assert_eq!(first_result.attempted_devices(), 0);
        assert_eq!(second_result.attempted_devices(), 1);
        let disabled_device = store
            .get_device_for_owner_on_node(&owner, first_node.node(), "shared-device-id")
            .await
            .expect("disabled device lookup")
            .expect("disabled device");
        let active_device = store
            .get_device_for_owner_on_node(&owner, second_node.node(), "shared-device-id")
            .await
            .expect("active device lookup")
            .expect("active device");
        assert_eq!(disabled_device.provider_endpoint(), None);
        assert_eq!(disabled_device.provider_token(), None);
        assert_eq!(disabled_device.provider_key_material(), None);
        assert_eq!(
            active_device.provider_endpoint(),
            Some("https://push.example.com/endpoint")
        );
        assert_eq!(active_device.provider_token(), Some("provider-secret"));
    }
}
