use axum::http::{Method, Uri};
use maul::openai::{
    CHAT_COMPLETIONS_PATH, ModelId, ModelIdError, OpenAiErrorEnvelope, classify_billable_route,
};

#[test]
fn classifies_only_post_chat_completions_requests() {
    let uri: Uri = format!("{CHAT_COMPLETIONS_PATH}?stream=true")
        .parse()
        .unwrap();
    assert!(classify_billable_route(&Method::POST, &uri).is_some());
    assert!(classify_billable_route(&Method::GET, &uri).is_none());

    let trailing_slash: Uri = format!("{CHAT_COMPLETIONS_PATH}/").parse().unwrap();
    assert!(classify_billable_route(&Method::POST, &trailing_slash).is_some());

    let models: Uri = "/v1/models".parse().unwrap();
    assert!(classify_billable_route(&Method::POST, &models).is_none());
}

#[test]
fn model_ids_reject_empty_and_whitespace() {
    assert_eq!(ModelId::try_from(String::new()), Err(ModelIdError::Empty));
    assert_eq!(
        ModelId::try_from("gpt 4o".to_owned()),
        Err(ModelIdError::Whitespace)
    );

    let model = ModelId::try_from("gpt-4o-mini".to_owned()).unwrap();
    assert_eq!(model.as_str(), "gpt-4o-mini");
}

#[test]
fn error_envelope_serializes_openai_compatible_shape() {
    let envelope = OpenAiErrorEnvelope::new(
        "maul: injected fault force_429",
        "rate_limit_error",
        "force_429",
    );
    let value = serde_json::to_value(envelope).unwrap();

    assert_eq!(value["error"]["message"], "maul: injected fault force_429");
    assert_eq!(value["error"]["type"], "rate_limit_error");
    assert_eq!(value["error"]["code"], "force_429");
}
