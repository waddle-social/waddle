use std::collections::BTreeMap;

use k8s_openapi::api::core::v1::{
    PersistentVolumeClaim, PersistentVolumeClaimSpec, VolumeResourceRequirements,
};
use k8s_openapi::apimachinery::pkg::api::resource::Quantity;
use kube::api::ObjectMeta;
use waddle_operator_crd::WaddleServer;

use crate::error::OperatorError;
use crate::resources::labels;

/// Builds the `PersistentVolumeClaim` when persistence is enabled without an `existingClaim`.
///
/// # Errors
/// Returns [`OperatorError::MissingMetadata`] if `ws` lacks `name` or `uid`.
pub fn build_pvc(ws: &WaddleServer) -> Result<Option<PersistentVolumeClaim>, OperatorError> {
    if !ws.spec.persistence.enabled || ws.spec.persistence.existing_claim.is_some() {
        return Ok(None);
    }

    let name = labels::pvc_name(ws)?;
    let owner = labels::owner_reference(ws)?;
    let obj_labels = labels::common_labels(ws)?;

    let access_modes: Vec<String> = ws
        .spec
        .persistence
        .access_modes
        .iter()
        .map(|mode| mode.as_k8s_str().to_owned())
        .collect();

    let mut requests = BTreeMap::new();
    requests.insert(
        "storage".to_owned(),
        Quantity(ws.spec.persistence.size.clone()),
    );

    Ok(Some(PersistentVolumeClaim {
        metadata: ObjectMeta {
            name: Some(name),
            labels: Some(obj_labels),
            owner_references: Some(vec![owner]),
            ..ObjectMeta::default()
        },
        spec: Some(PersistentVolumeClaimSpec {
            access_modes: Some(access_modes),
            resources: Some(VolumeResourceRequirements {
                requests: Some(requests),
                ..VolumeResourceRequirements::default()
            }),
            storage_class_name: ws.spec.persistence.storage_class_name.clone(),
            ..PersistentVolumeClaimSpec::default()
        }),
        ..PersistentVolumeClaim::default()
    }))
}
