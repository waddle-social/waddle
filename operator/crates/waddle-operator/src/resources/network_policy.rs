use k8s_openapi::api::networking::v1::{
    NetworkPolicy, NetworkPolicyIngressRule, NetworkPolicyPeer, NetworkPolicyPort,
    NetworkPolicySpec,
};
use k8s_openapi::apimachinery::pkg::apis::meta::v1::LabelSelector;
use k8s_openapi::apimachinery::pkg::util::intstr::IntOrString;
use kube::api::ObjectMeta;
use waddle_operator_crd::WaddleServer;

use crate::error::OperatorError;
use crate::resources::labels;

/// Builds the clustering `NetworkPolicy` restricting swarm traffic to sibling pods.
///
/// # Errors
/// Returns [`OperatorError::MissingMetadata`] if `ws` lacks `name` or `uid`.
pub fn build_network_policy(ws: &WaddleServer) -> Result<Option<NetworkPolicy>, OperatorError> {
    if !ws.spec.clustering.enabled || !ws.spec.network_policy.enabled {
        return Ok(None);
    }

    let name = format!("{}-swarm", labels::workload_name(ws)?);
    let owner = labels::owner_reference(ws)?;
    let obj_labels = labels::common_labels(ws)?;
    let selector = labels::selector_labels(ws)?;

    let http_rule = NetworkPolicyIngressRule {
        ports: Some(vec![NetworkPolicyPort {
            protocol: Some("TCP".to_owned()),
            port: Some(IntOrString::Int(ws.spec.service.http_port)),
            ..NetworkPolicyPort::default()
        }]),
        ..NetworkPolicyIngressRule::default()
    };

    let swarm_rule = NetworkPolicyIngressRule {
        from: Some(vec![NetworkPolicyPeer {
            pod_selector: Some(LabelSelector {
                match_labels: Some(selector.clone()),
                ..LabelSelector::default()
            }),
            ..NetworkPolicyPeer::default()
        }]),
        ports: Some(vec![NetworkPolicyPort {
            protocol: Some("TCP".to_owned()),
            port: Some(IntOrString::Int(i32::from(ws.spec.clustering.swarm_port))),
            ..NetworkPolicyPort::default()
        }]),
    };

    Ok(Some(NetworkPolicy {
        metadata: ObjectMeta {
            name: Some(name),
            labels: Some(obj_labels),
            owner_references: Some(vec![owner]),
            ..ObjectMeta::default()
        },
        spec: Some(NetworkPolicySpec {
            pod_selector: Some(LabelSelector {
                match_labels: Some(selector),
                ..LabelSelector::default()
            }),
            policy_types: Some(vec!["Ingress".to_owned()]),
            ingress: Some(vec![http_rule, swarm_rule]),
            ..NetworkPolicySpec::default()
        }),
    }))
}
