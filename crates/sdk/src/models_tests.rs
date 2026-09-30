use http::HeaderValue;
use serde_json::json;

use super::*;
use crate::{ErrorKind, ResponseValidationError};

/// `{"models": [CARD]}` of `test_models_shape`, `tests/test_clients.py`.
const MODELS: &[u8] = include_bytes!("../tests/fixtures/models.json");
/// The card of `test_models_ignore_unknown_fields`, `tests/test_clients.py`.
const MODELS_EXTRA_FIELDS: &[u8] = include_bytes!("../tests/fixtures/models-extra-fields.json");

fn decode(body: &[u8], endpoint: bool) -> Result<ListModelsResponse, Error> {
    let mut headers = HeaderMap::new();
    headers.insert("x-typesafe-request-id", HeaderValue::from_static("req_123"));
    let uri = Uri::from_static("https://api.typesafe.ai/v1/models");
    decode_list_models(
        Bytes::copy_from_slice(body),
        StatusCode::OK,
        headers,
        endpoint.then_some((&Method::GET, &uri)),
    )
}

fn rejection(body: &[u8], endpoint: bool) -> ResponseValidationError {
    match decode(body, endpoint) {
        Ok(response) => panic!(
            "{} decoded where it should have failed: {response:?}",
            String::from_utf8_lossy(body)
        ),
        Err(error) => match error.kind() {
            ErrorKind::ResponseValidation(failure) => failure.clone(),
            other => {
                panic!("{} failed with the wrong kind: {other:?}", String::from_utf8_lossy(body))
            }
        },
    }
}

/// `test_models_shape`.
#[test]
fn a_models_response_decodes_to_its_cards() {
    let response = decode(MODELS, true).expect("the fixture decodes");

    let [card] = response.models() else {
        panic!("one card expected, got {:?}", response.models());
    };
    assert_eq!(card.name(), "jev-latest");
    assert_eq!(card.description(), "Fast model");
    assert_eq!(card.release_date(), "2026-08-01");
    assert_eq!(response.meta().status(), StatusCode::OK);
    assert_eq!(response.meta().request_id(), Some("req_123"));
    assert_eq!(&response.meta().raw_body()[..], MODELS);
    assert_eq!(response.clone().into_models(), response.models());
}

/// `test_models_ignore_unknown_fields`.
#[test]
fn members_a_card_does_not_model_are_dropped_but_stay_in_the_raw_body() {
    let response = decode(MODELS_EXTRA_FIELDS, true).expect("the fixture decodes");

    assert_eq!(response.models(), decode(MODELS, true).expect("the fixture decodes").models());
    let with_top_level_extra = br#"{"object":"list","models":[{"name":"jev-latest","description":"Fast model","release_date":"2026-08-01"}],"has_more":false}"#;
    assert_eq!(
        decode(with_top_level_extra, true)
            .expect("an unknown top-level member is ignored")
            .models(),
        response.models()
    );
    let raw: serde_json::Value =
        serde_json::from_slice(response.meta().raw_body()).expect("the raw body parses");
    assert_eq!(raw["models"][0]["context_window"], 128_000);
}

/// `test_invalid_models_response`, every row, plus a card sent as an array.
#[test]
fn a_models_body_of_the_wrong_shape_is_a_validation_error_at_the_offending_field() {
    // The Python SDK reports the document root as an empty path; the codec
    // names it `.`, as it does for every type.
    let rows: [(&[u8], &str); 7] = [
        (b"null", "."),
        (b"{}", "models"),
        (br#"{"models":"bad"}"#, "models"),
        (br#"{"models":[{"name":"x"}]}"#, "models[0].description"),
        (br#"{"models":[["jev-latest","Fast model","2026-08-01"]]}"#, "models[0]"),
        (br#"[[{"name":"a","description":"b","release_date":"c"}]]"#, "."),
        (br#"{"models":[{"name":1,"description":"b","release_date":"c"}]}"#, "models[0].name"),
    ];

    for (body, path) in rows {
        let failure = rejection(body, true);
        let text = String::from_utf8_lossy(body);
        assert_eq!(failure.field_path(), path, "body {text}");
        assert_eq!(failure.message(), format!("Invalid response data at '{path}'."), "body {text}");
        assert_eq!(failure.body(), body, "body {text}");
        assert_eq!(
            failure.to_string(),
            format!(
                "GET https://api.typesafe.ai/v1/models: 200 Invalid response data at '{path}'. (request_id=req_123)"
            ),
            "body {text}"
        );
    }
}

/// `test_nested_missing_field_path`, all three members.
#[test]
fn a_missing_member_of_a_later_card_is_named_with_its_index() {
    for missing in ["name", "description", "release_date"] {
        let model =
            json!({"name": "test", "description": "Test model", "release_date": "2026-09-14"});
        let mut partial = model.clone();
        partial.as_object_mut().expect("a card is an object").remove(missing);
        let body =
            serde_json::to_vec(&json!({"models": [model, partial]})).expect("the body encodes");

        let failure = rejection(&body, false);

        assert_eq!(failure.field_path(), format!("models[1].{missing}"));
        assert_eq!(
            failure.to_string(),
            format!("200 Invalid response data at 'models[1].{missing}'. (request_id=req_123)")
        );
        assert_eq!(failure.endpoint(), None);
    }
}

/// `test_response_serialization_excludes_http_metadata`, the models half.
#[test]
fn serializing_a_models_response_writes_the_payload_and_not_the_http_metadata() {
    let body = json!({"models": [{"name": "test", "description": "Test model", "release_date": "2026-09-14"}]});
    let text = serde_json::to_vec(&body).expect("the body encodes");
    let response = decode(&text, true).expect("the body decodes");

    let through_serde_json = serde_json::to_value(&response).expect("the response serializes");
    assert_eq!(through_serde_json, body);

    let mut through_the_codec = Vec::new();
    codec::encode_into(&mut through_the_codec, &response).expect("the response encodes");
    let reparsed: serde_json::Value =
        serde_json::from_slice(&through_the_codec).expect("the codec writes JSON");
    assert_eq!(reparsed, body);

    let restored = decode(&through_the_codec, true).expect("the serialized form decodes again");
    assert_eq!(restored.models(), response.models());
}

#[test]
fn the_readers_say_what_they_expected() {
    assert_eq!(
        Show(|f: &mut fmt::Formatter<'_>| CardVisitor.expecting(f)).to_string(),
        "a model card"
    );
    assert_eq!(
        Show(|f: &mut fmt::Formatter<'_>| ModelListVisitor.expecting(f)).to_string(),
        "a list of models"
    );
}

/// Renders whatever the closure writes.
struct Show<F>(F);

impl<F> fmt::Display for Show<F>
where
    F: Fn(&mut fmt::Formatter<'_>) -> fmt::Result,
{
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        (self.0)(formatter)
    }
}
