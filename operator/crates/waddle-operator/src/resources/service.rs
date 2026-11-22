use k8s_openapi::api::core::v1::{Service, ServicePort, ServiceSpec as K8sServiceSpec};
use k8s_openapi::apimachinery::pkg::util::intstr::IntOrString;
use kube::api::ObjectMeta;
use waddle_operator_crd::WaddleServer;

use crate::error::OperatorError;
use crate::resources::labels;

/// Builds the primary HTTP/WebSocket Kubernetes `Service`.
///
/// # Errors
/// Returns [`OperatorError::MissingMetadata`] if `ws` lacks `name` or `uid`.
pub fn build_http_service(ws: &WaddleServer) -> Result<Service, OperatorError> {
    let name = labels::workload_name(ws)?.to_owned();
    let owner = labels::owner_reference(ws)?;
    let obj_labels = labels::common_labels(ws)?;
    let selector = labels::selector_labels(ws)?;
    let annotations =
        (!ws.spec.service.annotations.is_empty()).then(|| ws.spec.service.annotations.clone());

    Ok(Service {
        metadata: ObjectMeta {
            name: Some(name),
            labels: Some(obj_labels),
            annotations,
            owner_references: Some(vec![owner]),
            ..ObjectMeta::default()
        },
        spec: Some(K8sServiceSpec {
            type_: Some(ws.spec.service.kind.as_k8s_str().to_owned()),
            selector: Some(selector),
            ports: Some(vec![ServicePort {
                name: Some("http".to_owned()),
                port: ws.spec.service.http_port,
                target_port: Some(IntOrString::String("http".to_owned())),
                protocol: Some("TCP".to_owned()),
                ..ServicePort::default()
            }]),
            ..K8sServiceSpec::default()
        }),
        ..Service::default()
    })
}

/// Builds the headless swarm `Service` (`publishNotReadyAddresses: true`) when clustering is enabled.
///
/// # Errors
/// Returns [`OperatorError::MissingMetadata`] if `ws` lacks `name` or `uid`.
pub fn build_headless_service(ws: &WaddleServer) -> Result<Option<Service>, OperatorError> {
    if !ws.spec.clustering.enabled {
        return Ok(None);
    }

    let name = labels::headless_service_name(ws)?;
    let owner = labels::owner_reference(ws)?;
    let obj_labels = labels::common_labels(ws)?;
    let selector = labels::selector_labels(ws)?;

    Ok(Some(Service {
        metadata: ObjectMeta {
            name: Some(name),
            labels: Some(obj_labels),
            owner_references: Some(vec![owner]),
            ..ObjectMeta::default()
        },
        spec: Some(K8sServiceSpec {
            cluster_ip: Some("None".to_owned()),
            publish_not_ready_addresses: Some(true),
            selector: Some(selector),
            ports: Some(vec![ServicePort {
                name: Some("swarm".to_owned()),
                port: i32::from(ws.spec.clustering.swarm_port),
                target_port: Some(IntOrString::String("swarm".to_owned())),
                protocol: Some("TCP".to_owned()),
                ..ServicePort::default()
            }]),
            ..K8sServiceSpec::default()
        }),
        ..Service::default()
    }))
}

/// Builds the optional XMPP S2S federation `Service` when `spec.s2s.enabled` is true.
///
/// # Errors
/// Returns [`OperatorError::MissingMetadata`] if `ws` lacks `name` or `uid`.
pub fn build_s2s_service(ws: &WaddleServer) -> Result<Option<Service>, OperatorError> {
    if !ws.spec.s2s.enabled {
        return Ok(None);
    }

    let name = labels::s2s_service_name(ws)?;
    let owner = labels::owner_reference(ws)?;
    let obj_labels = labels::common_labels(ws)?;
    let selector = labels::selector_labels(ws)?;
    let external_ips =
        (!ws.spec.s2s.external_ips.is_empty()).then(|| ws.spec.s2s.external_ips.clone());

    Ok(Some(Service {
        metadata: ObjectMeta {
            name: Some(name),
            labels: Some(obj_labels),
            owner_references: Some(vec![owner]),
            ..ObjectMeta::default()
        },
        spec: Some(K8sServiceSpec {
            type_: Some(ws.spec.s2s.service_type.as_k8s_str().to_owned()),
            external_ips,
            selector: Some(selector),
            ports: Some(vec![ServicePort {
                name: Some("xmpp-s2s".to_owned()),
                port: ws.spec.s2s.port,
                target_port: Some(IntOrString::Int(ws.spec.s2s.port)),
                node_port: ws.spec.s2s.node_port,
                protocol: Some("TCP".to_owned()),
                ..ServicePort::default()
            }]),
            ..K8sServiceSpec::default()
        }),
        ..Service::default()
    }))
}
