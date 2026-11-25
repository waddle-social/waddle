//! Browser access to the account's XEP-0030 extension service.
use super::{send_iq_command, Rc, RefCell, WaddleClientInner};
pub(crate) async fn discover_extension_command_service(
    inner: Rc<RefCell<WaddleClientInner>>,
    domain: &str,
) -> String {
    waddle_xmpp_client_community::extension_routes::service(
        &|iq| send_iq_command(inner.clone(), iq),
        domain,
    )
    .await
}
