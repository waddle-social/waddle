//! Private iCalendar subscriptions across the authenticated XMPP and public HTTP listeners.

use minidom::Element;
use reqwest::{header, StatusCode, Url};
use waddle_ws_test_support::iq::round_trip;
use waddle_ws_test_support::{TestServer, WsXmppClient};
use waddle_xmpp::xep::xep0004::{DataForm, Field, FieldType, FormType, FromElement, ToElement};
use waddle_xmpp::xep::xep0050::{Action, Command, Status, NODE_COMMANDS};
use waddle_xmpp_core::calendar_subscription::{ISSUE, REVOKE, URL_FIELD};
use waddle_xmpp_core::disco::DISCO_ITEMS_NS;
use xmpp_parsers::iq::Iq;

const DOMAIN: &str = "localhost";
const PUBLIC_HOST: &str = "calendars.example.test";

async fn login(server: &TestServer, account: &str, password: &str, resource: &str) -> WsXmppClient {
    WsXmppClient::connect_and_auth(&server.ws_url(), DOMAIN, account, password, resource)
        .await
        .expect("authenticate a local calendar user")
}

async fn execute(client: &mut WsXmppClient, to: &str, command: Command) -> Iq {
    round_trip(
        client,
        Iq::Set {
            id: uuid::Uuid::new_v4().to_string(),
            from: None,
            to: Some(to.parse().unwrap()),
            payload: command.to_element(),
        },
    )
    .await
}

async fn completed(client: &mut WsXmppClient, node: &str) -> Command {
    let result = execute(
        client,
        DOMAIN,
        Command::new(node).with_action(Action::Execute),
    )
    .await;
    let Iq::Result {
        from,
        payload: Some(payload),
        ..
    } = result
    else {
        panic!("the registered calendar command must return a result");
    };
    assert_eq!(from.as_ref().map(|jid| jid.as_str()), Some(DOMAIN));
    let command = Command::from_element(&payload).expect("XEP-0050 completion");
    assert_eq!(command.node, node);
    assert_eq!(command.status, Some(Status::Completed));
    command
}

async fn issue(client: &mut WsXmppClient) -> Url {
    let form = completed(client, ISSUE)
        .await
        .form
        .expect("calendar grant result form");
    assert_eq!(form.form_type, FormType::Result);
    assert_eq!(
        form.field("FORM_TYPE").unwrap().field_type,
        FieldType::Hidden
    );
    assert_eq!(form.form_type_value(), Some(ISSUE));
    let field = form.field(URL_FIELD).expect("calendar URL field");
    assert_eq!(field.field_type, FieldType::TextSingle);
    assert_eq!(field.values.len(), 1);
    let url = Url::parse(field.value().unwrap()).expect("absolute calendar URL");
    assert_eq!(url.scheme(), "https");
    assert_eq!(url.host_str(), Some(PUBLIC_HOST));
    assert!(url.path().starts_with("/api/calendar/community/"));
    assert!(url.path().ends_with("/events.ics"));
    url
}

async fn assert_feed(server: &TestServer, issued: &Url, status: StatusCode) {
    // Use the harness listener for the configured external origin.
    let mut local = Url::parse(&server.http_base_url()).unwrap();
    local.set_path(issued.path());
    let response = reqwest::get(local).await.expect("HTTP calendar request");
    assert_eq!(response.status(), status);
    if status == StatusCode::OK {
        assert_eq!(response.headers()[header::CACHE_CONTROL], "no-store");
        assert!(response.headers()[header::CONTENT_TYPE]
            .to_str()
            .unwrap()
            .starts_with("text/calendar"));
        assert!(response
            .text()
            .await
            .unwrap()
            .starts_with("BEGIN:VCALENDAR"));
    }
}

#[tokio::test]
async fn issue_and_revoke_only_the_bound_accounts_feed_across_restarts() {
    let directory = tempfile::tempdir().unwrap();
    let database = format!(
        "sqlite://{}?mode=rwc",
        directory.path().join("calendar.db").display()
    );
    let start = || {
        TestServer::start_persistent_with_extra_envs(
            &database,
            &[
                ("alice", "alice-calendar-password"),
                ("bob", "bob-calendar-password"),
            ],
            &[("WADDLE_BASE_URL", "https://calendars.example.test")],
        )
    };
    let server = start();
    let mut alice = login(&server, "alice", "alice-calendar-password", "phone").await;
    let mut desktop = login(&server, "alice", "alice-calendar-password", "desktop").await;
    let mut bob = login(&server, "bob", "bob-calendar-password", "phone").await;

    let query = Element::builder("query", DISCO_ITEMS_NS)
        .attr(minidom::rxml::xml_ncname!("node").to_owned(), NODE_COMMANDS)
        .build();
    let listed = round_trip(
        &mut alice,
        Iq::Get {
            id: "calendar-commands".into(),
            from: None,
            to: Some(DOMAIN.parse().unwrap()),
            payload: query,
        },
    )
    .await;
    let Iq::Result {
        payload: Some(items),
        ..
    } = listed
    else {
        panic!("command discovery response");
    };
    for node in [ISSUE, REVOKE] {
        assert!(items.children().any(|item| item.is("item", DISCO_ITEMS_NS)
            && item.attr("node") == Some(node)
            && item.attr("jid") == Some(DOMAIN)));
    }
    let wrong_target = execute(&mut alice, "community.localhost", Command::new(ISSUE)).await;
    assert!(matches!(wrong_target, Iq::Error { .. }));
    let (first, from_desktop) = tokio::join!(issue(&mut alice), issue(&mut desktop));
    assert!(
        first == from_desktop,
        "issuing on two resources returns the same active grant"
    );
    let bobs = issue(&mut bob).await;
    assert!(
        bobs != first,
        "separate accounts receive independent grants"
    );
    assert_feed(&server, &first, StatusCode::OK).await;

    let supplied_owner = DataForm::new(FormType::Submit)
        .add_field(Field::form_type(REVOKE))
        .add_field(Field::text_single("owner", "bob@localhost"));
    let invalid = execute(
        &mut alice,
        DOMAIN,
        Command::new(REVOKE).with_form(supplied_owner),
    )
    .await;
    assert!(matches!(invalid, Iq::Error { .. }));
    assert_feed(&server, &first, StatusCode::OK).await;
    for _ in 0..2 {
        assert!(completed(&mut desktop, REVOKE).await.form.is_none());
    }
    assert_feed(&server, &first, StatusCode::NOT_FOUND).await;
    assert_feed(&server, &bobs, StatusCode::OK).await;
    let replacement = issue(&mut alice).await;
    assert!(
        replacement != first,
        "reissuing cannot revive a revoked bearer"
    );
    drop((alice, desktop, bob, server));

    let restarted = start();
    assert_feed(&restarted, &first, StatusCode::NOT_FOUND).await;
    assert_feed(&restarted, &replacement, StatusCode::OK).await;
    assert_feed(&restarted, &bobs, StatusCode::OK).await;
}
