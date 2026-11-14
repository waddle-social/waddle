use minidom::rxml::Namespace;
use minidom::Element;

use super::address::parse_jid;
use super::error::IdentityError;
use crate::muc::presence::NS_MUC_USER;
use crate::xep::xep0004::NS_DATA_FORMS;

/// Apply the identity profile to the addresses a client wrote into one
/// decoded stanza, before typed parsing prepares them.
///
/// These addresses are read as XML-decoded text, so a numeric character
/// reference cannot hide a spelling the parser would fold:
///
/// - the stanza's `to` and `from` attributes;
/// - every unprefixed `jid` attribute in its payload (roster, MUC and
///   PubSub items, bookmarks, blocking lists, XEP-0033 addresses and other
///   XEP payloads that name an entity);
/// - the `to` and `from` attributes of XEP-0045 mediated invitations and
///   declines (`<invite/>` and `<decline/>` in the `muc#user` namespace);
/// - each non-empty `<value/>` of an XEP-0004 data-form field declared
///   `jid-single` or `jid-multi`, with surrounding whitespace ignored.
///
/// Each must pass [`parse_jid`] and is rewritten in canonical form: a
/// trailing dot is removed and a U-label domain becomes its A-label form.
/// The first address outside the profile refuses the whole stanza.
///
/// Data-form fields submitted without a `type` attribute cannot be
/// recognised here; a handler that reads such a field as an address applies
/// the same rule (for example through `DataForm::bare_jid`).
pub fn enforce_stanza_addresses(stanza: &mut Element) -> Result<(), IdentityError> {
    canonicalize_attribute(stanza, "to")?;
    canonicalize_attribute(stanza, "from")?;
    let mut pending: Vec<&mut Element> = stanza.children_mut().collect();
    while let Some(element) = pending.pop() {
        canonicalize_attribute(element, "jid")?;
        if element.is("invite", NS_MUC_USER) || element.is("decline", NS_MUC_USER) {
            canonicalize_attribute(element, "to")?;
            canonicalize_attribute(element, "from")?;
        }
        if is_jid_field(element) {
            for value in element.children_mut() {
                if value.is("value", NS_DATA_FORMS) {
                    canonicalize_text(value)?;
                }
            }
        }
        pending.extend(element.children_mut());
    }
    Ok(())
}

fn canonicalize_attribute(element: &mut Element, name: &str) -> Result<(), IdentityError> {
    let Some(value) = element.attrs_mut().get_mut(&Namespace::NONE, name) else {
        return Ok(());
    };
    let canonical = parse_jid(value)?;
    if canonical.as_str() != value.as_str() {
        *value = canonical.into_inner();
    }
    Ok(())
}

fn is_jid_field(element: &Element) -> bool {
    element.is("field", NS_DATA_FORMS)
        && matches!(element.attr("type"), Some("jid-single" | "jid-multi"))
}

/// Check one data-form `<value/>` and rewrite its text in canonical form.
fn canonicalize_text(value: &mut Element) -> Result<(), IdentityError> {
    let text = value.text();
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Ok(());
    }
    let canonical = parse_jid(trimmed)?;
    if canonical.as_str() != text {
        let mut texts = value.texts_mut();
        if let Some(first) = texts.next() {
            *first = canonical.into_inner();
        }
        for rest in texts {
            rest.clear();
        }
    }
    Ok(())
}
