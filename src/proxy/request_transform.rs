//! Idempotent request transforms for billable chat-completion requests.

use serde_json::Value;

use crate::usage::RequestTransformError;

pub fn include_stream_usage(body: &mut Value) -> Result<bool, RequestTransformError> {
    let object = body
        .as_object_mut()
        .ok_or(RequestTransformError::NotObject)?;
    let stream = object
        .get("stream")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    if !stream {
        return Ok(false);
    }

    let stream_options = object
        .entry("stream_options")
        .or_insert_with(|| Value::Object(serde_json::Map::new()));
    let options = stream_options
        .as_object_mut()
        .ok_or(RequestTransformError::InvalidStreamOptions)?;
    if options.get("include_usage") == Some(&Value::Bool(true)) {
        return Ok(false);
    }
    options.insert("include_usage".to_owned(), Value::Bool(true));
    Ok(true)
}
