use super::*;

pub(super) fn handle_upload_disco_info<'a>(
    req: &'a DiscoInfoRequest<'a>,
) -> Option<DiscoInfoResponse<'a>> {
    if req.target_to != Some(req.upload_domain) {
        return None;
    }

    let identities = vec![Identity::upload_service(Some("HTTP File Upload"))];
    let features = upload_service_features();
    let response = build_disco_info_response(req.request_iq, &identities, &features, None);
    Some(DiscoInfoResponse::iq(response))
}
