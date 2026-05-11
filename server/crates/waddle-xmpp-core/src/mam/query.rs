use chrono::{DateTime, Utc};
use jid::Jid;
use minidom::Element;
use tracing::debug;
use uuid::Uuid;
use xmpp_parsers::iq::{Iq, IqType};

use super::stanza_id_filter::MamFilterStanzaId;
use super::types::{MamQuery, RichText, ThreadId};
use super::{
    DATA_FORMS_NS, FULLTEXT_MAM_FIELD, MAM_NS, MAX_FILTER_STANZA_IDS, MAX_FILTER_STANZA_ID_LEN,
    RSM_NS, STANZA_ID_FILTER_FIELD, WADDLE_MAM_THREAD_FIELD, XDATA_VALIDATE_NS,
};
use crate::{CoreError, CoreResult};

/// Parse a MAM query from an IQ stanza.
pub fn parse_mam_query(iq: &Iq) -> CoreResult<(String, MamQuery)> {
    let query_elem = match &iq.payload {
        IqType::Set(elem) if elem.name() == "query" && elem.ns() == MAM_NS => elem,
        IqType::Set(_) | IqType::Get(_) => {
            return Err(CoreError::bad_request(Some(
                "Missing MAM query element".to_string(),
            )));
        }
        _ => {
            return Err(CoreError::bad_request(Some(
                "Invalid IQ type for MAM query".to_string(),
            )));
        }
    };

    let query_id = query_elem
        .attr("queryid")
        .map(str::to_owned)
        .unwrap_or_else(|| Uuid::now_v7().to_string());

    let mut mam_query = MamQuery::default();

    for child in query_elem.children() {
        if child.name() == "x" && child.ns() == DATA_FORMS_NS {
            parse_data_form(child, &mut mam_query)?;
        } else if child.name() == "set" && child.ns() == RSM_NS {
            parse_rsm(child, &mut mam_query)?;
        }
    }

    debug!(
        query_id = %query_id,
        with = ?mam_query.with,
        thread = ?mam_query.thread_id,
        fulltext_len = mam_query.fulltext.as_ref().map(|t| t.as_str().len()),
        ids_count = mam_query.ids.len(),
        stanza_ids_count = mam_query.stanza_ids.len(),
        start = ?mam_query.start,
        end = ?mam_query.end,
        max = ?mam_query.max,
        "Parsed MAM query"
    );

    Ok((query_id, mam_query))
}

/// Check if an IQ asks for the XEP-0313 supported query fields form.
pub fn is_mam_query_form_request(iq: &Iq) -> bool {
    matches!(
        &iq.payload,
        IqType::Get(elem) if elem.name() == "query" && elem.ns() == MAM_NS
    )
}

/// Check if an IQ is a MAM query.
pub fn is_mam_query(iq: &Iq) -> bool {
    matches!(
        &iq.payload,
        IqType::Set(elem) | IqType::Get(elem)
            if elem.name() == "query" && elem.ns() == MAM_NS
    )
}

/// Build the XEP-0313 supported query fields response.
pub fn build_query_form_iq(original_iq: &Iq) -> Iq {
    let form = Element::builder("x", DATA_FORMS_NS)
        .attr("type", "form")
        .append(
            Element::builder("field", DATA_FORMS_NS)
                .attr("var", "FORM_TYPE")
                .attr("type", "hidden")
                .append(
                    Element::builder("value", DATA_FORMS_NS)
                        .append(MAM_NS)
                        .build(),
                )
                .build(),
        )
        .append(
            Element::builder("field", DATA_FORMS_NS)
                .attr("var", "with")
                .attr("type", "jid-single")
                .build(),
        )
        .append(
            Element::builder("field", DATA_FORMS_NS)
                .attr("var", "start")
                .attr("type", "text-single")
                .build(),
        )
        .append(
            Element::builder("field", DATA_FORMS_NS)
                .attr("var", "end")
                .attr("type", "text-single")
                .build(),
        )
        .append(
            Element::builder("field", DATA_FORMS_NS)
                .attr("var", "before-id")
                .attr("type", "text-single")
                .build(),
        )
        .append(
            Element::builder("field", DATA_FORMS_NS)
                .attr("var", "after-id")
                .attr("type", "text-single")
                .build(),
        )
        .append(
            Element::builder("field", DATA_FORMS_NS)
                .attr("var", "ids")
                .attr("type", "list-multi")
                .append(
                    Element::builder("validate", XDATA_VALIDATE_NS)
                        .attr("datatype", "xs:string")
                        .append(Element::builder("open", XDATA_VALIDATE_NS).build())
                        .build(),
                )
                .build(),
        )
        .append(
            Element::builder("field", DATA_FORMS_NS)
                .attr("var", STANZA_ID_FILTER_FIELD)
                .attr("type", "text-multi")
                .build(),
        )
        .append(
            Element::builder("field", DATA_FORMS_NS)
                .attr("var", WADDLE_MAM_THREAD_FIELD)
                .attr("type", "text-single")
                .build(),
        )
        .append(
            Element::builder("field", DATA_FORMS_NS)
                .attr("var", FULLTEXT_MAM_FIELD)
                .attr("type", "text-single")
                .build(),
        )
        .build();
    let query = Element::builder("query", MAM_NS).append(form).build();
    Iq {
        from: original_iq.to.clone(),
        to: original_iq.from.clone(),
        id: original_iq.id.clone(),
        payload: IqType::Result(Some(query)),
    }
}

fn parse_data_form(form: &Element, query: &mut MamQuery) -> CoreResult<()> {
    for field in form.children() {
        if field.name() != "field" {
            continue;
        }

        let var = field.attr("var").unwrap_or("");
        let values = field
            .children()
            .filter(|child| child.name() == "value")
            .map(Element::text)
            .collect::<Vec<_>>();
        let value = values.iter().find(|value| !value.is_empty()).cloned();

        match var {
            "" | "FORM_TYPE" => {}
            "start" => {
                if let Some(value) = value {
                    query.start = Some(parse_datetime(&value)?);
                }
            }
            "end" => {
                if let Some(value) = value {
                    query.end = Some(parse_datetime(&value)?);
                }
            }
            "with" => {
                if let Some(value) = value {
                    let parsed = value.parse::<Jid>().map_err(|error| {
                        CoreError::bad_request(Some(format!(
                            "Invalid `with` JID in MAM query: {error}"
                        )))
                    })?;
                    query.with = Some(parsed);
                }
            }
            "before-id" => {
                query.filter_before_id = value;
            }
            "after-id" => {
                query.filter_after_id = value;
            }
            "ids" => {
                query.ids = values
                    .into_iter()
                    .filter(|value| !value.is_empty())
                    .collect();
            }
            STANZA_ID_FILTER_FIELD => {
                let mut tokens: Vec<MamFilterStanzaId> = Vec::with_capacity(values.len());
                for value in values {
                    if value.is_empty() {
                        continue;
                    }
                    let Some(token) = MamFilterStanzaId::new(value) else {
                        return Err(CoreError::bad_request(Some(format!(
                            "MAM stanza-id filter value exceeds max length {MAX_FILTER_STANZA_ID_LEN}"
                        ))));
                    };
                    tokens.push(token);
                }
                if tokens.len() > MAX_FILTER_STANZA_IDS {
                    return Err(CoreError::bad_request(Some(format!(
                        "MAM stanza-id filter exceeds cap: {} > {MAX_FILTER_STANZA_IDS}",
                        tokens.len()
                    ))));
                }
                query.stanza_ids = tokens;
            }
            WADDLE_MAM_THREAD_FIELD => {
                query.thread_id = value.and_then(ThreadId::new);
            }
            FULLTEXT_MAM_FIELD => {
                query.fulltext = value.and_then(RichText::new);
            }
            _ => {
                return Err(CoreError::NotImplemented);
            }
        }
    }

    Ok(())
}

fn parse_rsm(rsm: &Element, query: &mut MamQuery) -> CoreResult<()> {
    for child in rsm.children() {
        match child.name() {
            "max" => {
                let value = child.text();
                if !value.is_empty() {
                    query.max = Some(value.parse().map_err(|_| {
                        CoreError::bad_request(Some(format!("Invalid RSM max value: {}", value)))
                    })?);
                }
            }
            "after" => {
                let value = child.text();
                if !value.is_empty() {
                    query.after_id = Some(value);
                }
            }
            "before" => {
                query.before_id = Some(child.text());
            }
            _ => {}
        }
    }

    Ok(())
}

fn parse_datetime(value: &str) -> CoreResult<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(value)
        .map(|dt| dt.with_timezone(&Utc))
        .map_err(|error| CoreError::bad_request(Some(format!("Invalid datetime: {}", error))))
}
