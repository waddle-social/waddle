use std::collections::BTreeMap;

use jid::{BareJid, DomainPart, NodePart};
use serde::Serialize;
use waddle_xmpp::protocol::frame::{inspect_stanza_addresses, AddressPreparation};

use super::model::{AccountRow, AccountSource, AuditError, Finding, OriginalValue, RecordKey};
use super::profile::candidate_findings;

#[derive(Serialize)]
pub struct AccountObservation {
    pub original: AccountRow,
    pub current_account_prepared_jid: Option<BareJid>,
    pub current_stanza_parsed_jid: Option<BareJid>,
    pub findings: Vec<Finding>,
}

#[derive(Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CollisionBasis {
    AccountPreparation,
    StanzaParsing,
    RecordedBinding,
}

#[derive(Serialize)]
pub struct Collision {
    pub basis: CollisionBasis,
    /// Indexes into this report's accounts; never inferred to be one person.
    pub account_indexes: Vec<usize>,
}

pub fn accounts(rows: Vec<AccountRow>) -> (Vec<AccountObservation>, Vec<Collision>) {
    let mut observations = Vec::with_capacity(rows.len());
    let mut parser_groups: BTreeMap<String, Vec<usize>> = BTreeMap::new();
    let mut stanza_groups: BTreeMap<String, Vec<usize>> = BTreeMap::new();
    let mut stored_groups: BTreeMap<String, Vec<usize>> = BTreeMap::new();
    for (index, row) in rows.into_iter().enumerate() {
        let localpart = row.localpart.as_str();
        let domain = row.domain.as_str();
        let mut findings = candidate_findings(localpart, domain);
        let raw_address = format!("{localpart}@{domain}");
        let prepared = NodePart::new(localpart).ok().and_then(|node| {
            DomainPart::new(domain)
                .ok()
                .map(|domain| BareJid::from_parts(Some(&node), &domain))
        });
        let stanza_parsed = BareJid::new(&raw_address).ok();
        if prepared != stanza_parsed {
            findings.push(Finding::PreparationPathsDisagree);
        }
        if let Some(jid) = &stanza_parsed {
            stanza_groups
                .entry(jid.to_string())
                .or_default()
                .push(index);
        }
        if let Some(jid) = &prepared {
            if jid.as_str() != raw_address {
                findings.push(Finding::ChangedByCurrentParser);
            }
            parser_groups
                .entry(jid.to_string())
                .or_default()
                .push(index);
        } else {
            findings.push(Finding::InvalidCurrentParserIdentity);
        }
        if let RecordKey::Oidc(source_jid) = &row.record {
            if prepared
                .as_ref()
                .is_none_or(|jid| jid.as_str() != source_jid.as_str())
            {
                findings.push(Finding::SourceJidDisagrees);
            }
        }
        match &row.stored_key {
            None => findings.push(Finding::MissingPreparedKey),
            Some(key) => {
                let expected = prepared.as_ref().map(|jid| match row.source {
                    AccountSource::Oidc => jid.node().map_or("", |node| node.as_str()),
                    AccountSource::Native => jid.as_str(),
                });
                if expected != Some(key.as_str()) {
                    findings.push(Finding::IncompatiblePreparedKey);
                }
                let binding = match row.source {
                    AccountSource::Oidc => format!("{}@{}", key.as_str(), domain),
                    AccountSource::Native => key.as_str().to_owned(),
                };
                stored_groups.entry(binding).or_default().push(index);
            }
        }
        observations.push(AccountObservation {
            original: row,
            current_account_prepared_jid: prepared,
            current_stanza_parsed_jid: stanza_parsed,
            findings,
        });
    }
    let mut collisions = Vec::new();
    for members in parser_groups
        .into_values()
        .filter(|members| members.len() > 1)
    {
        collisions.push(Collision {
            basis: CollisionBasis::AccountPreparation,
            account_indexes: members,
        });
    }
    for members in stanza_groups
        .into_values()
        .filter(|members| members.len() > 1)
    {
        collisions.push(Collision {
            basis: CollisionBasis::StanzaParsing,
            account_indexes: members,
        });
    }
    for members in stored_groups
        .into_values()
        .filter(|members| members.len() > 1)
    {
        collisions.push(Collision {
            basis: CollisionBasis::RecordedBinding,
            account_indexes: members,
        });
    }
    (observations, collisions)
}

#[derive(Serialize)]
pub struct AddressObservation {
    pub original_xml_decoded: Option<OriginalValue>,
    pub current_parser_jid: Option<jid::Jid>,
    pub findings: Vec<Finding>,
}

#[derive(Serialize)]
pub struct FrameObservation {
    pub corpus_index: usize,
    pub from: AddressObservation,
    pub to: AddressObservation,
}

fn address(value: AddressPreparation) -> AddressObservation {
    let mut findings = Vec::new();
    if let Some(original) = &value.original {
        let raw = original.as_str();
        let bare = raw.split_once('/').map_or(raw, |(bare, _)| {
            findings.push(Finding::ResourceAddress);
            bare
        });
        if let Some((node, domain)) = bare.split_once('@') {
            findings.extend(candidate_findings(node, domain));
        } else {
            findings.push(Finding::ServiceAddress);
        }
        if value
            .prepared
            .as_ref()
            .is_some_and(|jid| jid.as_str() != raw)
        {
            findings.push(Finding::ChangedByCurrentParser);
        }
    }
    AddressObservation {
        original_xml_decoded: value
            .original
            .map(|value| OriginalValue::new(value.as_str().to_owned())),
        current_parser_jid: value.prepared,
        findings,
    }
}

pub fn frames(corpus: &[String]) -> Result<Vec<FrameObservation>, AuditError> {
    corpus
        .iter()
        .enumerate()
        .map(|(index, frame)| {
            let audit = inspect_stanza_addresses(frame).map_err(|_| AuditError::Frames)?;
            Ok(FrameObservation {
                corpus_index: index,
                from: address(audit.from),
                to: address(audit.to),
            })
        })
        .collect()
}

#[cfg(test)]
mod tests;
