use minidom::rxml::Namespace;
use minidom::Element;

use super::address::parse_jid;
use super::error::IdentityError;

/// Apply the identity profile to the addresses a client wrote into one
/// decoded stanza, before typed parsing prepares them.
///
/// The stanza's `to` and `from` attributes and every unprefixed `jid`
/// attribute in its payload (roster, MUC and PubSub items, bookmarks,
/// blocking lists and other XEP payloads that name an entity) are read as
/// XML-decoded text, so a numeric character reference cannot hide a
/// spelling the parser would fold. Each must pass [`parse_jid`] and is
/// rewritten in canonical form: a trailing dot is removed and a U-label
/// domain becomes its A-label form. The first address outside the profile
/// refuses the whole stanza.
pub fn enforce_stanza_addresses(stanza: &mut Element) -> Result<(), IdentityError> {
    canonicalize_attribute(stanza, "to")?;
    canonicalize_attribute(stanza, "from")?;
    let mut pending: Vec<&mut Element> = stanza.children_mut().collect();
    while let Some(element) = pending.pop() {
        canonicalize_attribute(element, "jid")?;
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
