use k8s_openapi::api::policy::v1::{PodDisruptionBudget, PodDisruptionBudgetSpec};
use k8s_openapi::apimachinery::pkg::apis::meta::v1::LabelSelector;
use k8s_openapi::apimachinery::pkg::util::intstr::IntOrString;
use kube::api::ObjectMeta;
use waddle_operator_crd::WaddleServer;

use crate::error::OperatorError;
use crate::resources::labels;

/// Builds a `PodDisruptionBudget` when both clustering and PDB support are enabled.
///
/// # Errors
/// Returns [`OperatorError::MissingMetadata`] if `ws` lacks `name` or `uid`.
pub fn build_pdb(ws: &WaddleServer) -> Result<Option<PodDisruptionBudget>, OperatorError> {
    if !ws.spec.clustering.enabled || !ws.spec.pdb.enabled {
        return Ok(None);
    }

    let name = labels::workload_name(ws)?.to_owned();
    let owner = labels::owner_reference(ws)?;
    let obj_labels = labels::common_labels(ws)?;
    let selector = labels::selector_labels(ws)?;

    Ok(Some(PodDisruptionBudget {
        metadata: ObjectMeta {
            name: Some(name),
            labels: Some(obj_labels),
            owner_references: Some(vec![owner]),
            ..ObjectMeta::default()
        },
        spec: Some(PodDisruptionBudgetSpec {
            max_unavailable: Some(IntOrString::Int(ws.spec.pdb.max_unavailable)),
            selector: Some(LabelSelector {
                match_labels: Some(selector),
                ..LabelSelector::default()
            }),
            ..PodDisruptionBudgetSpec::default()
        }),
        ..PodDisruptionBudget::default()
    }))
}
