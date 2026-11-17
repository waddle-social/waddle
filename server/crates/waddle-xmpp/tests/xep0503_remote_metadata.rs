//! XEP-0503 0.3.0 (xsf/xeps 4132c531487fe9450ae90b905b8df950d0d478b2): what
//! a space node's `disco#info` tells an asker at another server, and the
//! identifiers the version adds.

use minidom::Element;
use waddle_xmpp::xep::{
    build_spaces_metadata_form_for_remote, ITEM_SPACE_AVATAR_METADATA, ITEM_SPACE_BANNER_METADATA,
    NS_SPACES,
};

const NS_DATA: &str = "jabber:x:data";

fn field<'a>(form: &'a Element, var: &str) -> Option<&'a Element> {
    form.children()
        .find(|child| child.is("field", NS_DATA) && child.attr("var") == Some(var))
}

fn value(form: &Element, var: &str) -> Option<String> {
    field(form, var)?
        .get_child("value", NS_DATA)
        .map(Element::text)
}

/// "Fetching the metadata associated to a space": a Space is recognised by
/// the `pubsub#type` of its node's meta-data form.
#[test]
fn the_remote_form_names_the_node_as_a_space() {
    let form = build_spaces_metadata_form_for_remote("The developers corner");

    assert!(form.is("x", NS_DATA));
    assert_eq!(form.attr("type"), Some("result"));
    assert_eq!(
        value(&form, "FORM_TYPE").as_deref(),
        Some("http://jabber.org/protocol/pubsub#meta-data")
    );
    assert_eq!(value(&form, "pubsub#type").as_deref(), Some(NS_SPACES));
    assert_eq!(
        value(&form, "pubsub#title").as_deref(),
        Some("The developers corner")
    );
}

/// The form names no account and no policy of the deployment that hosts
/// the Space.
#[test]
fn the_remote_form_names_no_account_and_no_policy() {
    let form = build_spaces_metadata_form_for_remote("Sales team");

    for var in [
        "pubsub#owner",
        "pubsub#creator",
        "pubsub#creation_date",
        "pubsub#access_model",
        "pubsub#affiliation",
        "pubsub#description",
    ] {
        assert!(field(&form, var).is_none(), "{var} is not told");
    }
    let fields = form
        .children()
        .filter(|child| child.is("field", NS_DATA))
        .count();
    assert_eq!(fields, 3, "FORM_TYPE, pubsub#type and pubsub#title only");
}

/// "Spaces images (avatar and banner)": the two item identifiers version
/// 0.3.0 registers.
#[test]
fn the_image_item_identifiers_are_those_of_version_0_3_0() {
    assert_eq!(
        ITEM_SPACE_AVATAR_METADATA,
        "urn:xmpp:spaces:avatar:metadata:0"
    );
    assert_eq!(
        ITEM_SPACE_BANNER_METADATA,
        "urn:xmpp:spaces:banner:metadata:0"
    );
}
