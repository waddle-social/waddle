use jid::{BareJid, Jid};
use minidom::Element;
use xmpp_parsers::iq::Iq;

use super::{
    build_block_iq, build_block_list_iq, build_unblock_all_iq, build_unblock_iq,
    parse_block_list_result, parse_blocking_push, BlockingPush, EmptyBlockingJids,
};

fn jid(value: &str) -> Jid {
    value.parse().expect("valid test JID")
}

fn iq(xml: &str) -> Iq {
    Iq::try_from(xml.parse::<Element>().expect("valid XML")).expect("valid IQ")
}

fn assert_wire(stanza: Iq, expected: &str) {
    assert_eq!(
        Element::from(stanza),
        expected.parse::<Element>().expect("valid expected XML")
    );
}

fn parse_push(xml: &str) -> Option<BlockingPush> {
    let account: BareJid = "alice@example.com".parse().expect("valid account JID");
    parse_blocking_push(&iq(xml), &account)
}

#[test]
fn builds_block_list_get() {
    assert_wire(
        build_block_list_iq("list"),
        "<iq xmlns='jabber:client' type='get' id='list'>\
           <blocklist xmlns='urn:xmpp:blocking'/>\
         </iq>",
    );
}

#[test]
fn builds_block_set_with_items() {
    assert_wire(
        build_block_iq(
            "block",
            vec![jid("bob@example.net"), jid("example.org/service")],
        )
        .expect("nonempty block"),
        "<iq xmlns='jabber:client' type='set' id='block'>\
           <block xmlns='urn:xmpp:blocking'>\
             <item jid='bob@example.net'/><item jid='example.org/service'/>\
           </block>\
         </iq>",
    );
}

#[test]
fn builds_unblock_set_with_items() {
    assert_wire(
        build_unblock_iq(
            "unblock",
            vec![jid("bob@example.net/phone"), jid("example.org")],
        )
        .expect("nonempty unblock"),
        "<iq xmlns='jabber:client' type='set' id='unblock'>\
           <unblock xmlns='urn:xmpp:blocking'>\
             <item jid='bob@example.net/phone'/><item jid='example.org'/>\
           </unblock>\
         </iq>",
    );
}

#[test]
fn builds_explicit_unblock_all_set() {
    assert_wire(
        build_unblock_all_iq("clear"),
        "<iq xmlns='jabber:client' type='set' id='clear'>\
           <unblock xmlns='urn:xmpp:blocking'/>\
         </iq>",
    );
}

#[test]
fn block_and_targeted_unblock_reject_empty_lists() {
    assert_eq!(
        build_block_iq("block", vec![]).expect_err("block requires items"),
        EmptyBlockingJids
    );
    assert_eq!(
        build_unblock_iq("unblock", vec![]).expect_err("targeted unblock requires items"),
        EmptyBlockingJids
    );
}

#[test]
fn parses_block_list_and_preserves_all_jid_forms() {
    let result = iq("<iq xmlns='jabber:client' type='result' id='list'>
        <blocklist xmlns='urn:xmpp:blocking'>
          <item jid='bob@example.net'/><item jid='bob@example.net/phone'/>
          <item jid='example.org'/><item jid='example.org/service'/>
          <item/><item jid=''/><item jid='not a jid'/>
          <item jid='reported@example.net'><report xmlns='urn:xmpp:reporting:1' reason='urn:xmpp:reporting:spam'/></item>
          <item xmlns='urn:example:other' jid='mallory@example.net'/>
        </blocklist>
      </iq>");
    assert_eq!(
        parse_block_list_result(&result),
        Some(vec![
            jid("bob@example.net"),
            jid("bob@example.net/phone"),
            jid("example.org"),
            jid("example.org/service"),
            jid("reported@example.net"),
        ])
    );
}

#[test]
fn distinguishes_empty_block_list_from_missing_payload() {
    let empty_list = iq("<iq xmlns='jabber:client' type='result' id='list'>
        <blocklist xmlns='urn:xmpp:blocking'/>
      </iq>");
    let no_payload = iq("<iq xmlns='jabber:client' type='result' id='list'/>");
    assert_eq!(parse_block_list_result(&empty_list), Some(vec![]));
    assert_eq!(parse_block_list_result(&no_payload), None);
}

#[test]
fn parses_block_push_without_a_sender() {
    assert_eq!(
        parse_push(
            "<iq xmlns='jabber:client' type='set' id='push'>
            <block xmlns='urn:xmpp:blocking'><item/><item jid='bob@example.net/phone'/></block>
          </iq>"
        ),
        Some(BlockingPush::Blocked(vec![jid("bob@example.net/phone")]))
    );
}

#[test]
fn parses_targeted_unblock_push_from_own_bare_jid() {
    assert_eq!(
        parse_push(
            "<iq xmlns='jabber:client' type='set' id='push' from='alice@example.com'>
            <unblock xmlns='urn:xmpp:blocking'><item jid='example.org'/></unblock>
          </iq>"
        ),
        Some(BlockingPush::Unblocked(vec![jid("example.org")]))
    );
}

#[test]
fn parses_unblock_all_push_only_for_an_empty_payload() {
    assert_eq!(
        parse_push(
            "<iq xmlns='jabber:client' type='set' id='clear'>
            <unblock xmlns='urn:xmpp:blocking'/>
          </iq>"
        ),
        Some(BlockingPush::UnblockedAll)
    );
    assert_eq!(
        parse_push(
            "<iq xmlns='jabber:client' type='set' id='targeted'>
            <unblock xmlns='urn:xmpp:blocking'><item jid=''/></unblock>
          </iq>"
        ),
        None,
        "a push whose items are all malformed names nobody, and is not an empty unblock"
    );
    assert_eq!(
        parse_push(
            "<iq xmlns='jabber:client' type='set' id='nobody'>
            <block xmlns='urn:xmpp:blocking'><item/></block>
          </iq>"
        ),
        None
    );
    assert_eq!(
        parse_push(
            "<iq xmlns='jabber:client' type='set' id='malformed'>
            <unblock xmlns='urn:xmpp:blocking' jid='bob@example.net'/>
          </iq>"
        ),
        None
    );
}

#[test]
fn rejects_foreign_jids_and_own_full_jid() {
    let account: BareJid = "alice@example.com".parse().expect("valid account JID");
    for sender in [
        "mallory@example.com",
        "example.com",
        "alice@example.com/phone",
    ] {
        let push = build_unblock_all_iq("push").with_from(jid(sender));
        assert_eq!(parse_blocking_push(&push, &account), None);
    }
}

#[test]
fn rejects_empty_block_and_non_set_iq() {
    assert_eq!(
        parse_push(
            "<iq xmlns='jabber:client' type='set' id='push'>
            <block xmlns='urn:xmpp:blocking'/>
          </iq>"
        ),
        None
    );
    assert_eq!(
        parse_push(
            "<iq xmlns='jabber:client' type='result' id='push'>
            <unblock xmlns='urn:xmpp:blocking'/>
          </iq>"
        ),
        None
    );
}
