use std::collections::BTreeMap;

use k8s_openapi::api::core::v1::Secret;
use k8s_openapi::ByteString;
use kube::api::ObjectMeta;
use waddle_operator_crd::WaddleServer;

use crate::error::OperatorError;
use crate::resources::labels;
use crate::yolo;

/// Secret key holding the server session encryption/signing key.
pub const KEY_SESSION_KEY: &str = "WADDLE_SESSION_KEY";
/// Secret key holding the XEP-0421 HMAC occupant identifier secret.
pub const KEY_OCCUPANT_ID_SECRET: &str = "WADDLE_OCCUPANT_ID_SECRET";
/// Secret key holding the SpiceDB preshared authentication key.
pub const KEY_SPICEDB_PRESHARED_KEY: &str = "WADDLE_SPICEDB_PRESHARED_KEY";
/// Secret key persisting the generated `WADDLE_DEPLOYMENT_UUID` in YOLO mode.
pub const KEY_DEPLOYMENT_UUID: &str = "WADDLE_DEPLOYMENT_UUID";

/// Builds a fresh YOLO runtime `Secret` with CSPRNG cryptographic keys and deployment UUID.
///
/// Always persists `WADDLE_DEPLOYMENT_UUID` (using `ws.spec.deployment.uuid` if supplied,
/// otherwise generating a fresh RFC-4122 v4 UUID) so single-node restarts preserve DB lineage.
///
/// # Errors
/// Returns [`OperatorError`] on missing metadata or CSPRNG failure.
pub fn build_yolo_secret(ws: &WaddleServer) -> Result<Secret, OperatorError> {
    let secret_name = labels::yolo_secret_name(ws)?;
    let owner = labels::owner_reference(ws)?;
    let obj_labels = labels::common_labels(ws)?;

    let session_key = yolo::generate_base64_secret_48()?;
    let occupant_id = yolo::generate_base64_secret_48()?;
    let deployment_uuid = ws
        .spec
        .deployment
        .uuid
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map_or_else(
            || yolo::generate_deployment_uuid().to_string(),
            str::to_owned,
        );

    let mut data = BTreeMap::new();
    data.insert(
        KEY_SESSION_KEY.to_owned(),
        ByteString(session_key.expose_secret().as_bytes().to_vec()),
    );
    data.insert(
        KEY_OCCUPANT_ID_SECRET.to_owned(),
        ByteString(occupant_id.expose_secret().as_bytes().to_vec()),
    );
    data.insert(
        KEY_DEPLOYMENT_UUID.to_owned(),
        ByteString(deployment_uuid.into_bytes()),
    );

    Ok(Secret {
        metadata: ObjectMeta {
            name: Some(secret_name),
            labels: Some(obj_labels),
            owner_references: Some(vec![owner]),
            ..ObjectMeta::default()
        },
        type_: Some("Opaque".to_owned()),
        data: Some(data),
        ..Secret::default()
    })
}

/// Fills only missing or empty required keys in an existing YOLO `Secret`.
///
/// Existing non-empty values are NEVER rotated or overwritten so sealed VAPID keys,
/// XEP-0421 occupant IDs, and DB lineage stay intact. Returns `true` if `existing` changed.
///
/// # Errors
/// Returns [`OperatorError::RandomGeneration`] on CSPRNG failure.
pub fn fill_missing_yolo_keys(
    ws: &WaddleServer,
    existing: &mut Secret,
) -> Result<bool, OperatorError> {
    let data = existing.data.get_or_insert_with(BTreeMap::new);
    let mut changed = false;

    if data.get(KEY_SESSION_KEY).is_none_or(|val| val.0.len() < 32) {
        let generated = yolo::generate_base64_secret_48()?;
        data.insert(
            KEY_SESSION_KEY.to_owned(),
            ByteString(generated.expose_secret().as_bytes().to_vec()),
        );
        changed = true;
    }

    if data
        .get(KEY_OCCUPANT_ID_SECRET)
        .is_none_or(|val| val.0.len() < 32)
    {
        let generated = yolo::generate_base64_secret_48()?;
        data.insert(
            KEY_OCCUPANT_ID_SECRET.to_owned(),
            ByteString(generated.expose_secret().as_bytes().to_vec()),
        );
        changed = true;
    }

    if data
        .get(KEY_DEPLOYMENT_UUID)
        .is_none_or(|val| val.0.is_empty())
    {
        let uuid = ws
            .spec
            .deployment
            .uuid
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map_or_else(
                || yolo::generate_deployment_uuid().to_string(),
                str::to_owned,
            );
        data.insert(
            KEY_DEPLOYMENT_UUID.to_owned(),
            ByteString(uuid.into_bytes()),
        );
        changed = true;
    }

    Ok(changed)
}
