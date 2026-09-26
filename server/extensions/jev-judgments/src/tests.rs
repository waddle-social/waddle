use serde_json::{json, Value};

use crate::config::{ConfigError, JevConfig, DEFAULT_ENDPOINT, DEFAULT_MODEL, MAX_RESPONSE_BYTES};
use crate::decisions::{parse_http_response, request_body, Failure, InvalidResponse, JudgmentKind};

const KINDS: [JudgmentKind; 8] = [
    JudgmentKind::IsQuestion,
    JudgmentKind::SafetyHateSpeech,
    JudgmentKind::SafetyExplicit,
    JudgmentKind::SafetyHarassment,
    JudgmentKind::SafetyViolence,
    JudgmentKind::SafetySelfHarm,
    JudgmentKind::SafetySpam,
    JudgmentKind::SafetyScam,
];

fn complete_response() -> Value {
    let answers: serde_json::Map<String, Value> = KINDS
        .iter()
        .map(|kind| {
            (
                kind.as_str().to_string(),
                json!({ "noul": 0.25, "type": "noul" }),
            )
        })
        .collect();
    json!({
        "answers": answers,
        "id": "provider-id-ignored",
        "model": "typesafe/jev-1.13-20260917",
        "usage": { "cost": 0.00002, "input_tokens": 100, "output_tokens": 25 },
    })
}

#[test]
fn request_asks_all_eight_questions_in_one_decisions_call() {
    let request: Value = serde_json::from_str(&request_body(DEFAULT_MODEL, "are we there yet?"))
        .expect("request JSON");
    assert_eq!(request["model"], DEFAULT_MODEL);
    assert_eq!(request["state"], "are we there yet?");
    let questions = request["questions"].as_object().expect("questions");
    assert_eq!(questions.len(), KINDS.len());
    for kind in KINDS {
        let question = &questions[kind.as_str()];
        assert_eq!(question["type"], "noul");
        assert!(question["instructions"]
            .as_str()
            .is_some_and(|text| !text.is_empty()));
        assert!(question["criteria"]["true"]
            .as_str()
            .is_some_and(|text| !text.is_empty()));
        assert!(question["criteria"]["false"]
            .as_str()
            .is_some_and(|text| !text.is_empty()));
    }
    assert_eq!(
        questions[JudgmentKind::IsQuestion.as_str()]["criteria"]["true"],
        "The message asks something and expects a reply from someone else."
    );
}

#[test]
fn complete_response_preserves_order_taxonomy_model_and_one_call_cost() {
    let batch = parse_http_response(200, &complete_response().to_string(), DEFAULT_MODEL)
        .expect("complete response");
    assert_eq!(batch.judgments.len(), KINDS.len());
    assert_eq!(batch.model_version, "typesafe/jev-1.13-20260917");
    assert_eq!(batch.cost_usd, 0.00002);
    assert_eq!(batch.cost_micro_usd(), Ok(20));
    for (judgment, kind) in batch.judgments.iter().zip(KINDS) {
        assert_eq!(judgment.kind, kind);
        assert_eq!(judgment.probability, 0.25);
        assert!(judgment.taxonomy_version.ends_with("-v1"));
    }
    assert_eq!(batch.judgments[0].taxonomy_version, "is-question-v1");
    assert_eq!(batch.judgments[5].taxonomy_version, "safety-self-harm-v1");
    assert_eq!(batch.judgments[6].taxonomy_version, "safety-spam-v1");
    assert_eq!(batch.judgments[7].taxonomy_version, "safety-scam-v1");
}

#[test]
fn empty_response_model_uses_configured_model() {
    let mut response = complete_response();
    response["model"] = json!("");
    let batch = parse_http_response(200, &response.to_string(), "configured-model")
        .expect("fallback model");
    assert_eq!(batch.model_version, "configured-model");
}

#[test]
fn missing_or_out_of_range_answer_rejects_entire_batch() {
    let mut response = complete_response();
    response["answers"]
        .as_object_mut()
        .expect("answers")
        .remove(JudgmentKind::SafetyExplicit.as_str());
    assert_eq!(
        parse_http_response(200, &response.to_string(), DEFAULT_MODEL),
        Err(Failure::InvalidResponse(InvalidResponse::MissingAnswer(
            JudgmentKind::SafetyExplicit,
        )))
    );

    let mut response = complete_response();
    response["answers"][JudgmentKind::SafetyViolence.as_str()]["noul"] = json!(1.5);
    assert_eq!(
        parse_http_response(200, &response.to_string(), DEFAULT_MODEL),
        Err(Failure::InvalidResponse(
            InvalidResponse::ProbabilityOutOfRange(JudgmentKind::SafetyViolence),
        ))
    );
}

#[test]
fn invalid_cost_and_shape_reject_entire_batch() {
    let mut response = complete_response();
    response["usage"]["cost"] = json!(-0.01);
    assert_eq!(
        parse_http_response(200, &response.to_string(), DEFAULT_MODEL),
        Err(Failure::InvalidResponse(InvalidResponse::InvalidCost))
    );
    assert_eq!(
        parse_http_response(200, "not JSON", DEFAULT_MODEL),
        Err(Failure::InvalidResponse(InvalidResponse::DocumentShape))
    );

    let mut response = complete_response();
    response["usage"]["cost"] = json!(1e20);
    let batch =
        parse_http_response(200, &response.to_string(), DEFAULT_MODEL).expect("finite cost parses");
    assert_eq!(
        batch.cost_micro_usd(),
        Err(Failure::InvalidResponse(InvalidResponse::InvalidCost))
    );
}

#[test]
fn status_and_size_failures_never_include_provider_text() {
    let echoed = "private message body and bearer secret";
    let failure = parse_http_response(429, echoed, DEFAULT_MODEL).expect_err("status failure");
    assert_eq!(failure, Failure::HttpStatus(429));
    assert_eq!(failure.category(), "http_status");
    assert_eq!(failure.http_status(), Some(429));
    assert!(!failure.to_string().contains(echoed));
    assert!(!format!("{failure:?}").contains(echoed));

    let oversized = "x".repeat(MAX_RESPONSE_BYTES + 1);
    assert_eq!(
        parse_http_response(200, &oversized, DEFAULT_MODEL),
        Err(Failure::ResponseTooLarge)
    );
}

#[test]
fn config_defaults_validate_https_and_redact_key() {
    let config = JevConfig::parse(r#"{"api_key":" super-secret-token\n"}"#).expect("config");
    assert_eq!(config.endpoint.as_str(), DEFAULT_ENDPOINT);
    assert_eq!(config.model, DEFAULT_MODEL);
    assert_eq!(config.api_key.as_str(), "super-secret-token");
    assert_eq!(format!("{:?}", config.api_key), "<redacted>");
    assert_eq!(MAX_RESPONSE_BYTES, 64 * 1024);

    assert!(matches!(
        JevConfig::parse(r#"{"api_key":"", "endpoint":"https://openrouter.ai"}"#),
        Err(ConfigError::MissingApiKey)
    ));
    assert!(matches!(
        JevConfig::parse(r#"{"api_key":"key", "endpoint":"http://openrouter.ai"}"#),
        Err(ConfigError::InvalidEndpoint)
    ));
    assert!(matches!(
        JevConfig::parse(r#"{"api_key":"key", "model":" "}"#),
        Err(ConfigError::MissingModel)
    ));
    assert!(matches!(
        JevConfig::parse("[]"),
        Err(ConfigError::InvalidJson)
    ));
}

#[test]
fn score_payload_keeps_target_identity_host_owned() {
    use crate::bindings::waddle::extension::types::XmlToken;
    let batch =
        parse_http_response(200, &complete_response().to_string(), DEFAULT_MODEL).expect("batch");
    let payload = crate::payload::safety_scores(&batch);
    assert_eq!(payload.namespace.value, crate::payload::NAMESPACE);
    assert_eq!(payload.root.local_name, crate::payload::ROOT);
    assert_eq!(payload.tokens.len(), 2 + KINDS.len() * 2);
    let XmlToken::StartElement(root) = &payload.tokens[0] else {
        panic!("root element");
    };
    assert_eq!(root.attributes.len(), 1);
    assert_eq!(root.attributes[0].local_name, "model-version");
    assert!(!root
        .attributes
        .iter()
        .any(|attribute| attribute.local_name.starts_with("target-")));
}
