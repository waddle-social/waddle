use k8s_openapi::api::core::v1::ServiceAccount;
use kube::api::ObjectMeta;
use waddle_operator_crd::WaddleServer;

use crate::error::OperatorError;
use crate::resources::labels;

/// Builds the dedicated `ServiceAccount` for the Waddle server pods.
///
/// # Errors
/// Returns [`OperatorError::MissingMetadata`] if `metadata.name` or `uid` is absent.
pub fn build_service_account(ws: &WaddleServer) -> Result<ServiceAccount, OperatorError> {
    let name = labels::workload_name(ws)?;
    let owner = labels::owner_reference(ws)?;
    let obj_labels = labels::common_labels(ws)?;

    Ok(ServiceAccount {
        metadata: ObjectMeta {
            name: Some(name.to_owned()),
            labels: Some(obj_labels),
            owner_references: Some(vec![owner]),
            ..ObjectMeta::default()
        },
        automount_service_account_token: Some(false),
        ..ServiceAccount::default()
    })
}
