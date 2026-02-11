use std::str::FromStr;

use xmpp_parsers::minidom::Element;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CarbonsState {
    #[default]
    Disabled,
    Enabling,
    Enabled,
    Disabling,
}

#[derive(Debug)]
pub struct CarbonsManager {
    state: CarbonsState,
}

const CARBONS_NS: &str = "urn:xmpp:carbons:2";

impl CarbonsManager {
    pub fn new() -> Self {
        Self {
            state: CarbonsState::Disabled,
        }
    }

    pub fn state(&self) -> CarbonsState {
        self.state
    }

    pub fn enable(&mut self) -> Option<Vec<u8>> {
        if !matches!(self.state, CarbonsState::Disabled) {
            return None;
        }

        self.state = CarbonsState::Enabling;
        Some(build_enable_iq())
    }

    pub fn disable(&mut self) -> Option<Vec<u8>> {
        if !matches!(self.state, CarbonsState::Enabled) {
            return None;
        }

        self.state = CarbonsState::Disabling;
        Some(build_disable_iq())
    }

    pub fn on_enable_result(&mut self, success: bool) {
        if !matches!(self.state, CarbonsState::Enabling) {
            return;
        }

        self.state = if success {
            CarbonsState::Enabled
        } else {
            CarbonsState::Disabled
        };
    }

    pub fn on_disable_result(&mut self, success: bool) {
        if !matches!(self.state, CarbonsState::Disabling) {
            return;
        }

        self.state = if success {
            CarbonsState::Disabled
        } else {
            CarbonsState::Enabled
        };
    }

    pub fn reset(&mut self) {
        self.state = CarbonsState::Disabled;
    }

    pub fn is_carbon(stanza: &[u8]) -> Option<CarbonDirection> {
        let xml = std::str::from_utf8(stanza).ok()?.trim();
        if xml.is_empty() {
            return None;
        }

        let element = Element::from_str(xml).ok()?;
        if element.name() != "message" {
            return None;
        }

        for child in element.children() {
            if child.ns() != CARBONS_NS {
                continue;
            }

            match child.name() {
                "received" => return Some(CarbonDirection::Received),
                "sent" => return Some(CarbonDirection::Sent),
                _ => {}
            }
        }

        None
    }

    pub fn unwrap_carbon(stanza: &[u8]) -> Option<UnwrappedCarbon> {
        let xml = std::str::from_utf8(stanza).ok()?.trim();
        if xml.is_empty() {
            return None;
        }

        let element = Element::from_str(xml).ok()?;
        if element.name() != "message" {
            return None;
        }

        for child in element.children() {
            if child.ns() != CARBONS_NS {
                continue;
            }

            let direction = match child.name() {
                "received" => CarbonDirection::Received,
                "sent" => CarbonDirection::Sent,
                _ => continue,
            };

            let forwarded = child
                .children()
                .find(|c| c.name() == "forwarded" && c.ns() == "urn:xmpp:forward:0")?;

            let inner_message = forwarded.children().find(|c| c.name() == "message")?;

            let mut payload = Vec::new();
            inner_message.write_to(&mut payload).ok()?;

            return Some(UnwrappedCarbon {
                direction,
                forwarded_stanza: payload,
            });
        }

        None
    }
}

impl Default for CarbonsManager {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CarbonDirection {
    Received,
    Sent,
}

#[derive(Debug, Clone, PartialEq)]
pub struct UnwrappedCarbon {
    pub direction: CarbonDirection,
    pub forwarded_stanza: Vec<u8>,
}

const CARBONS_ENABLE_IQ_ID: &str = "carbons-enable";
const CARBONS_DISABLE_IQ_ID: &str = "carbons-disable";

pub fn carbons_enable_iq_id() -> &'static str {
    CARBONS_ENABLE_IQ_ID
}

pub fn carbons_disable_iq_id() -> &'static str {
    CARBONS_DISABLE_IQ_ID
}

fn build_enable_iq() -> Vec<u8> {
    format!(
        "<iq xmlns='jabber:client' type='set' id='{CARBONS_ENABLE_IQ_ID}'>\
         <enable xmlns='{CARBONS_NS}'/>\
         </iq>"
    )
    .into_bytes()
}

fn build_disable_iq() -> Vec<u8> {
    format!(
        "<iq xmlns='jabber:client' type='set' id='{CARBONS_DISABLE_IQ_ID}'>\
         <disable xmlns='{CARBONS_NS}'/>\
         </iq>"
    )
    .into_bytes()
}

pub fn is_carbons_iq_response(stanza: &[u8]) -> Option<(bool, bool)> {
    let xml = std::str::from_utf8(stanza).ok()?.trim();
    if xml.is_empty() {
        return None;
    }

    let element = Element::from_str(xml).ok()?;
    if element.name() != "iq" {
        return None;
    }

    let id = element.attr("id")?;
    let is_enable = match id {
        CARBONS_ENABLE_IQ_ID => true,
        CARBONS_DISABLE_IQ_ID => false,
        _ => return None,
    };

    let iq_type = element.attr("type")?;
    let success = iq_type == "result";

    Some((is_enable, success))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_manager_starts_disabled() {
        let manager = CarbonsManager::new();
        assert_eq!(manager.state(), CarbonsState::Disabled);
    }

    #[test]
    fn enable_transitions_to_enabling_and_returns_iq() {
        let mut manager = CarbonsManager::new();
        let iq = manager.enable();
        assert!(iq.is_some());
        assert_eq!(manager.state(), CarbonsState::Enabling);

        let iq_str = String::from_utf8(iq.unwrap()).unwrap();
        assert!(iq_str.contains("type='set'"));
        assert!(iq_str.contains("<enable"));
        assert!(iq_str.contains(CARBONS_NS));
    }

    #[test]
    fn enable_while_enabling_returns_none() {
        let mut manager = CarbonsManager::new();
        manager.enable();
        assert!(manager.enable().is_none());
    }

    #[test]
    fn enable_while_enabled_returns_none() {
        let mut manager = CarbonsManager::new();
        manager.enable();
        manager.on_enable_result(true);
        assert!(manager.enable().is_none());
    }

    #[test]
    fn on_enable_result_success_transitions_to_enabled() {
        let mut manager = CarbonsManager::new();
        manager.enable();
        manager.on_enable_result(true);
        assert_eq!(manager.state(), CarbonsState::Enabled);
    }

    #[test]
    fn on_enable_result_failure_transitions_to_disabled() {
        let mut manager = CarbonsManager::new();
        manager.enable();
        manager.on_enable_result(false);
        assert_eq!(manager.state(), CarbonsState::Disabled);
    }

    #[test]
    fn disable_transitions_to_disabling_and_returns_iq() {
        let mut manager = CarbonsManager::new();
        manager.enable();
        manager.on_enable_result(true);

        let iq = manager.disable();
        assert!(iq.is_some());
        assert_eq!(manager.state(), CarbonsState::Disabling);

        let iq_str = String::from_utf8(iq.unwrap()).unwrap();
        assert!(iq_str.contains("type='set'"));
        assert!(iq_str.contains("<disable"));
        assert!(iq_str.contains(CARBONS_NS));
    }

    #[test]
    fn disable_while_disabled_returns_none() {
        let mut manager = CarbonsManager::new();
        assert!(manager.disable().is_none());
    }

    #[test]
    fn on_disable_result_success_transitions_to_disabled() {
        let mut manager = CarbonsManager::new();
        manager.enable();
        manager.on_enable_result(true);
        manager.disable();
        manager.on_disable_result(true);
        assert_eq!(manager.state(), CarbonsState::Disabled);
    }

    #[test]
    fn on_disable_result_failure_transitions_to_enabled() {
        let mut manager = CarbonsManager::new();
        manager.enable();
        manager.on_enable_result(true);
        manager.disable();
        manager.on_disable_result(false);
        assert_eq!(manager.state(), CarbonsState::Enabled);
    }

    #[test]
    fn reset_returns_to_disabled() {
        let mut manager = CarbonsManager::new();
        manager.enable();
        manager.on_enable_result(true);
        manager.reset();
        assert_eq!(manager.state(), CarbonsState::Disabled);
    }

    #[test]
    fn is_carbon_detects_received_carbon() {
        let stanza = br#"<message xmlns='jabber:client' from='alice@example.com' to='alice@example.com/desktop'>
            <received xmlns='urn:xmpp:carbons:2'>
                <forwarded xmlns='urn:xmpp:forward:0'>
                    <message from='bob@example.com' to='alice@example.com/mobile' type='chat'>
                        <body>Hello</body>
                    </message>
                </forwarded>
            </received>
        </message>"#;

        assert_eq!(
            CarbonsManager::is_carbon(stanza),
            Some(CarbonDirection::Received)
        );
    }

    #[test]
    fn is_carbon_detects_sent_carbon() {
        let stanza = br#"<message xmlns='jabber:client' from='alice@example.com' to='alice@example.com/desktop'>
            <sent xmlns='urn:xmpp:carbons:2'>
                <forwarded xmlns='urn:xmpp:forward:0'>
                    <message from='alice@example.com/mobile' to='bob@example.com' type='chat'>
                        <body>Hi there</body>
                    </message>
                </forwarded>
            </sent>
        </message>"#;

        assert_eq!(
            CarbonsManager::is_carbon(stanza),
            Some(CarbonDirection::Sent)
        );
    }

    #[test]
    fn is_carbon_returns_none_for_normal_message() {
        let stanza = br#"<message xmlns='jabber:client' from='bob@example.com' to='alice@example.com' type='chat'>
            <body>Hello</body>
        </message>"#;

        assert_eq!(CarbonsManager::is_carbon(stanza), None);
    }

    #[test]
    fn is_carbon_returns_none_for_non_message() {
        let stanza = br#"<presence xmlns='jabber:client' from='bob@example.com'/>"#;
        assert_eq!(CarbonsManager::is_carbon(stanza), None);
    }

    #[test]
    fn unwrap_carbon_extracts_forwarded_message() {
        let stanza = br#"<message xmlns='jabber:client' from='alice@example.com' to='alice@example.com/desktop'>
            <received xmlns='urn:xmpp:carbons:2'>
                <forwarded xmlns='urn:xmpp:forward:0'>
                    <message xmlns='jabber:client' from='bob@example.com' to='alice@example.com/mobile' type='chat'>
                        <body>Hello</body>
                    </message>
                </forwarded>
            </received>
        </message>"#;

        let result = CarbonsManager::unwrap_carbon(stanza);
        assert!(result.is_some());

        let unwrapped = result.unwrap();
        assert_eq!(unwrapped.direction, CarbonDirection::Received);

        let inner = String::from_utf8(unwrapped.forwarded_stanza).unwrap();
        assert!(inner.contains("bob@example.com"));
        assert!(inner.contains("Hello"));
    }

    #[test]
    fn unwrap_carbon_returns_none_for_normal_message() {
        let stanza = br#"<message xmlns='jabber:client' from='bob@example.com' to='alice@example.com' type='chat'>
            <body>Hello</body>
        </message>"#;

        assert!(CarbonsManager::unwrap_carbon(stanza).is_none());
    }

    #[test]
    fn is_carbons_iq_response_detects_enable_result() {
        let stanza =
            format!("<iq xmlns='jabber:client' type='result' id='{CARBONS_ENABLE_IQ_ID}'/>");
        let result = is_carbons_iq_response(stanza.as_bytes());
        assert_eq!(result, Some((true, true)));
    }

    #[test]
    fn is_carbons_iq_response_detects_enable_error() {
        let stanza = format!(
            "<iq xmlns='jabber:client' type='error' id='{CARBONS_ENABLE_IQ_ID}'>\
             <error type='cancel'><service-unavailable xmlns='urn:ietf:params:xml:ns:xmpp-stanzas'/></error>\
             </iq>"
        );
        let result = is_carbons_iq_response(stanza.as_bytes());
        assert_eq!(result, Some((true, false)));
    }

    #[test]
    fn is_carbons_iq_response_detects_disable_result() {
        let stanza =
            format!("<iq xmlns='jabber:client' type='result' id='{CARBONS_DISABLE_IQ_ID}'/>");
        let result = is_carbons_iq_response(stanza.as_bytes());
        assert_eq!(result, Some((false, true)));
    }

    #[test]
    fn is_carbons_iq_response_returns_none_for_unrelated_iq() {
        let stanza = b"<iq xmlns='jabber:client' type='result' id='something-else'/>";
        assert!(is_carbons_iq_response(stanza).is_none());
    }

    #[test]
    fn is_carbons_iq_response_returns_none_for_non_iq() {
        let stanza = b"<message xmlns='jabber:client'/>";
        assert!(is_carbons_iq_response(stanza).is_none());
    }
}
