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

/// Request a slot and return its (PUT, GET) URLs rewritten to the test
/// server's HTTP origin, plus the raw GET URL from the slot.
async fn request_slot(
    server: &TestServer,
    client: &mut WsXmppClient,
    filename: &str,
    content_type: &str,
    size: usize,
) -> (String, String, String) {
    let id = format!("slot-{}", uuid::Uuid::new_v4());
    client
        .send(&format!(
            r#"<iq type="get" id="{id}" to="upload.{DOMAIN}"><request xmlns="{NS_UPLOAD}" filename="{filename}" size="{size}" content-type="{content_type}"/></iq>"#
        ))
        .await
        .expect("send slot request");
    let reply = client
        .recv_matching(|frame| frame.contains(&id))
        .await
        .expect("slot response");
    let iq: Element = reply.parse().expect("typed slot response");
    let slot = iq
        .get_child("slot", NS_UPLOAD)
        .unwrap_or_else(|| panic!("slot element: {reply}"));
    let url_of = |name: &str| {
        slot.get_child(name, NS_UPLOAD)
            .unwrap()
            .attr("url")
            .unwrap()
    };
    let local = |url: &str| {
        let rest = url.split_once("://").expect("absolute url").1;
        format!(
            "{}{}",
            server.http_base_url(),
            &rest[rest.find('/').unwrap()..]
        )
    };
    (
        local(url_of("put")),
        local(url_of("get")),
        url_of("get").to_string(),
    )
}

async fn put_body(put_url: &str, content_type: &str, body: &[u8]) -> reqwest::Response {
    reqwest::Client::new()
        .put(put_url)
        .header("Content-Type", content_type)
        .body(body.to_vec())
        .send()
        .await
        .expect("PUT upload")
}

/// Request a slot, PUT `body`, and return the GET response.
async fn upload_and_download(
    server: &TestServer,
    client: &mut WsXmppClient,
    filename: &str,
    content_type: &str,
    body: &[u8],
) -> reqwest::Response {
    let (put_url, get_url, _) =
        request_slot(server, client, filename, content_type, body.len()).await;
    let put = put_body(&put_url, content_type, body).await;
    assert!(put.status().is_success(), "PUT status {}", put.status());
    reqwest::get(&get_url).await.expect("GET download")
}

async fn connect(server: &TestServer) -> WsXmppClient {
    let password = server.fixed_account_password().to_string();
    WsXmppClient::connect_and_auth(
        &server.ws_url(),
        DOMAIN,
        "admin",
        &password,
        &format!("xep0363-{}", uuid::Uuid::new_v4()),
    )
    .await
    .expect("connect and auth")
}

fn header<'a>(resp: &'a reqwest::Response, name: &str) -> &'a str {
    resp.headers()
        .get(name)
        .unwrap_or_else(|| panic!("missing {name} header"))
        .to_str()
        .unwrap()
}

#[tokio::test]
async fn dangerous_uploads_are_served_as_sandboxed_attachments() {
    let _guard = TEST_SERIAL.lock().await;
    let server = TestServer::start();
    let mut client = connect(&server).await;

    for (name, ctype, body) in [
        ("x.html", "text/html", &b"<script>alert(1)</script>"[..]),
        (
            "x.svg",
            "image/svg+xml",
            &b"<svg xmlns='http://www.w3.org/2000/svg' onload='alert(1)'/>"[..],
        ),
        ("x.xml", "application/xhtml+xml", &b"<html/>"[..]),
    ] {
        let resp = upload_and_download(&server, &mut client, name, ctype, body).await;
        assert_eq!(resp.status(), 200, "{name}");
        assert!(
            header(&resp, "content-disposition").starts_with("attachment;"),
            "{name}"
        );
        assert_eq!(
            header(&resp, "content-type"),
            "application/octet-stream",
            "{name}"
        );
        assert_eq!(header(&resp, "x-content-type-options"), "nosniff");
        let csp = header(&resp, "content-security-policy");
        assert!(
            csp.contains("default-src 'none'") && csp.contains("sandbox"),
            "{csp}"
        );
        assert_eq!(resp.bytes().await.unwrap().as_ref(), body);
    }
}

#[tokio::test]
async fn passive_media_uploads_stay_inline_with_hardening_headers() {
    let _guard = TEST_SERIAL.lock().await;
    let server = TestServer::start();
    let mut client = connect(&server).await;

    let resp = upload_and_download(
        &server,
        &mut client,
        "pic.png",
        "image/png",
        b"\x89PNG\r\n\x1a\n",
    )
    .await;
    assert_eq!(resp.status(), 200);
    assert!(header(&resp, "content-disposition").starts_with("inline;"));
    assert_eq!(header(&resp, "content-type"), "image/png");
    assert_eq!(header(&resp, "x-content-type-options"), "nosniff");
    assert!(header(&resp, "content-security-policy").contains("default-src 'none'"));
}

#[tokio::test]
async fn long_multibyte_filename_gets_a_slot_and_keeps_extension() {
    let _guard = TEST_SERIAL.lock().await;
    let server = TestServer::start();
    let mut client = connect(&server).await;

    let filename = format!("{}.png", "\u{e9}".repeat(200));
    let resp = upload_and_download(&server, &mut client, &filename, "image/png", b"png").await;
    assert_eq!(resp.status(), 200);
    assert!(
        resp.url().path().ends_with(".png"),
        "extension kept: {}",
        resp.url()
    );
}

#[tokio::test]
async fn get_url_percent_encodes_non_ascii_filename() {
    let _guard = TEST_SERIAL.lock().await;
    let server = TestServer::start();
    let mut client = connect(&server).await;

    let (_, _, raw_get) =
        request_slot(&server, &mut client, "tr\u{e8}s cool.jpg", "image/jpeg", 3).await;
    assert!(raw_get.is_ascii(), "GET url must be ASCII: {raw_get}");
    assert!(raw_get.ends_with("/tr%C3%A8s_cool.jpg"), "{raw_get}");

    let resp = upload_and_download(
        &server,
        &mut client,
        "tr\u{e8}s cool.jpg",
        "image/jpeg",
        b"jpg",
    )
    .await;
    assert_eq!(resp.status(), 200, "encoded GET url resolves to the file");
}

#[tokio::test]
async fn put_with_mismatched_content_type_is_rejected() {
    let _guard = TEST_SERIAL.lock().await;
    let server = TestServer::start();
    let mut client = connect(&server).await;

    let (put_url, _, _) = request_slot(&server, &mut client, "a.png", "image/png", 3).await;
    let bad = put_body(&put_url, "text/html", b"png").await;
    assert_eq!(bad.status(), 415, "mismatched Content-Type is rejected");

    // Parameters and case are ignored; the slot is still usable.
    let ok = put_body(&put_url, "Image/PNG; charset=binary", b"png").await;
    assert!(ok.status().is_success(), "PUT status {}", ok.status());
}

#[tokio::test]
async fn upload_disco_info_advertises_max_file_size_form() {
    let _guard = TEST_SERIAL.lock().await;
    let server = TestServer::start();
    let mut client = connect(&server).await;

    client
        .send(&format!(
            r#"<iq type="get" id="disco-up" to="upload.{DOMAIN}"><query xmlns="http://jabber.org/protocol/disco#info"/></iq>"#
        ))
        .await
        .expect("send disco#info");
    let reply = client
        .recv_matching(|frame| frame.contains("disco-up"))
        .await
        .expect("disco#info response");
    let iq: Element = reply.parse().expect("typed disco response");
    let query = iq
        .get_child("query", "http://jabber.org/protocol/disco#info")
        .expect("query");
    let form = query
        .children()
        .find(|c| c.is("x", "jabber:x:data"))
        .unwrap_or_else(|| panic!("data form: {reply}"));
    let value_of = |var: &str| {
        form.children()
            .find(|f| f.attr("var") == Some(var))
            .and_then(|f| f.get_child("value", "jabber:x:data"))
            .map(Element::text)
    };
    assert_eq!(value_of("FORM_TYPE").as_deref(), Some(NS_UPLOAD));
    let max: u64 = value_of("max-file-size")
        .expect("max-file-size field")
        .parse()
        .expect("size in bytes");
    assert!(max > 0);
}
