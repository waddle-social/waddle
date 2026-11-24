//! XEP-0363 §4: the slot `<put/>` may only carry Authorization, Cookie and
//! Expires headers.

use minidom::Element;
use tokio::sync::Mutex;
use waddle_ws_test_support::{TestServer, WsXmppClient};

const DOMAIN: &str = "localhost";
const NS_UPLOAD: &str = "urn:xmpp:http:upload:0";
const ALLOWED_HEADERS: [&str; 3] = ["Authorization", "Cookie", "Expires"];
static TEST_SERIAL: Mutex<()> = Mutex::const_new(());

#[tokio::test]
async fn slot_put_headers_are_limited_to_allowed_names() {
    let _guard = TEST_SERIAL.lock().await;
    let server = TestServer::start();
    let password = server.fixed_account_password().to_string();
    let mut client = WsXmppClient::connect_and_auth(
        &server.ws_url(),
        DOMAIN,
        "admin",
        &password,
        &format!("xep0363-{}", uuid::Uuid::new_v4()),
    )
    .await
    .expect("connect and auth");

    client
        .send(&format!(
            r#"<iq type="get" id="slot-1" to="upload.{DOMAIN}"><request xmlns="{NS_UPLOAD}" filename="note.txt" size="12" content-type="text/plain"/></iq>"#
        ))
        .await
        .expect("send slot request");
    let reply = client
        .recv_matching(|frame| frame.contains("slot-1"))
        .await
        .expect("slot response");

    let iq: Element = reply.parse().expect("typed slot response");
    let slot = iq.get_child("slot", NS_UPLOAD).expect("slot element");
    let put = slot.get_child("put", NS_UPLOAD).expect("put element");
    assert!(put.attr("url").is_some(), "put url: {reply}");
    for header in put.children().filter(|c| c.is("header", NS_UPLOAD)) {
        let name = header.attr("name").expect("header name");
        assert!(
            ALLOWED_HEADERS
                .iter()
                .any(|allowed| allowed.eq_ignore_ascii_case(name)),
            "disallowed slot header {name}: {reply}"
        );
    }
}
