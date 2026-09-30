//! Tests for the System One request body and the per-call settings. The body
//! is compared byte for byte: its member order is part of what the request
//! builder promises.

use serde::{Serializer, ser};

use super::*;
use crate::{ErrorKind, Noul, Questions, RawJson, transport::HyperTransport};

fn client() -> Client<HyperTransport> {
    Client::builder()
        .api_key("test-key")
        .default_model("jev-latest")
        .build_with_env(|_: &str| None::<String>)
        .expect("the client builds")
}

fn questions() -> PreparedQuestions {
    Questions::new().noul("q", Noul::new().instructions("?")).prepare().expect("prepares")
}

/// The body `request` sends, as text.
fn body<T, A>(request: &SystemOne<'_, HyperTransport, T, A>) -> String
where
    T: Serialize + ?Sized,
    A: AnswerSet,
{
    let bytes = request.encode().unwrap_or_else(|error| panic!("the body encodes: {error}"));
    String::from_utf8(bytes.to_vec()).expect("a body is UTF-8")
}

/// The invalid-request error `request` fails to encode with, rendered.
fn refusal<T>(request: &SystemOne<'_, HyperTransport, T>) -> String
where
    T: Serialize + ?Sized,
{
    let error = request.encode().expect_err("the body must be refused");
    assert!(matches!(error.kind(), ErrorKind::InvalidRequest), "{error:?}");
    error.to_string()
}

const QUESTIONS: &str = r#"{"q":{"type":"noul","instructions":"?"}}"#;

#[test]
fn the_body_is_state_model_and_questions_in_that_order() {
    let client = client();
    let questions = questions();

    let text = client.system_one("hello", &questions);
    assert_eq!(
        body(&text),
        format!(r#"{{"state":"hello","model":"jev-latest","questions":{QUESTIONS}}}"#)
    );

    // "Hello" and the globe emoji: an object state, encoded where it lies.
    let object = serde_json::json!({"document": "Hello \u{1f30d}"});
    let request = client.system_one(&object, &questions).model("call-\"model\"");
    assert_eq!(
        body(&request),
        format!(
            "{{\"state\":{{\"document\":\"Hello \u{1f30d}\"}},\"model\":\"call-\\\"model\\\"\",\"questions\":{QUESTIONS}}}"
        )
    );

    let list = ["a", "b"];
    assert!(body(&client.system_one(&list, &questions)).starts_with(r#"{"state":["a","b"],"#));
}

/// Upstream `test_extra_body_shallow_override`: last write wins, a built-in
/// member is replaced where it stands, and `null` is sent as `null`.
#[test]
fn extra_members_merge_last_write_wins_and_replace_a_built_in_in_place() {
    let client = client();
    let questions = questions();
    let request = client
        .system_one("hi", &questions)
        .model("call-model")
        .extra_body("model", "override-model")
        .extra_body("beam_width", &4)
        .extra_body("nullable", &None::<u8>)
        .extra_body("beam_width", &5);

    assert_eq!(
        body(&request),
        format!(
            r#"{{"state":"hi","model":"override-model","questions":{QUESTIONS},"beam_width":5,"nullable":null}}"#
        )
    );

    // A replaced `questions` and `state`, and raw JSON spliced as written.
    let raw = RawJson::from_value(&serde_json::json!({"k": 1})).expect("encodes");
    let request = client
        .system_one(&7, &questions)
        .extra_body("state", "replaced")
        .extra_body("questions", &raw)
        .extra_body("odd \"name\"", &true);
    assert_eq!(
        body(&request),
        r#"{"state":"replaced","model":"jev-latest","questions":{"k":1},"odd \"name\"":true}"#,
        "a replaced state is never encoded, so a number there is no error"
    );
}

#[test]
fn a_state_that_is_not_text_an_object_or_an_array_is_refused() {
    let client = client();
    let questions = questions();
    let message = "The state must be a JSON string, object or array; \
                   it encoded as a number, a boolean or null.";
    assert_eq!(refusal(&client.system_one(&5, &questions)), message);
    assert_eq!(refusal(&client.system_one(&1.5, &questions)), message);
    assert_eq!(refusal(&client.system_one(&true, &questions)), message);
    assert_eq!(refusal(&client.system_one(&None::<String>, &questions)), message);
    assert_eq!(refusal(&client.system_one(&(), &questions)), message);
}

/// A value whose `Serialize` fails, as upstream's `object()` does.
struct Refuses;

impl Serialize for Refuses {
    fn serialize<S: Serializer>(&self, _: S) -> Result<S::Ok, S::Error> {
        Err(ser::Error::custom("this value refuses to be encoded"))
    }
}

/// Upstream `test_unserializable_request_body_raises`, for the state and for
/// an extra member.
#[test]
fn a_value_that_cannot_be_encoded_fails_before_anything_is_sent() {
    let client = client();
    let questions = questions();
    let codec_says = codec::encode_into(&mut Vec::new(), &Refuses)
        .expect_err("the value refuses")
        .message()
        .to_owned();

    assert_eq!(
        refusal(&client.system_one(&Refuses, &questions)),
        format!("The request body could not be encoded as JSON: the state: {codec_says}")
    );
    assert_eq!(
        refusal(&client.system_one("x", &questions).extra_body("bad", &Refuses)),
        format!(
            r#"The request body could not be encoded as JSON: the extra member "bad": {codec_says}"#
        )
    );
    // A later value for the same name replaces the one that failed.
    let recovered =
        client.system_one("x", &questions).extra_body("bad", &Refuses).extra_body("bad", &1);
    assert!(body(&recovered).ends_with(r#""bad":1}"#));
}

#[test]
fn a_deadline_is_the_clients_a_calls_or_none_and_never_zero() {
    let client_deadline = Some(Duration::from_secs(10));
    assert_eq!(Deadline::Client.resolve(client_deadline).ok(), Some(client_deadline));
    assert_eq!(Deadline::Client.resolve(None).ok(), Some(None));
    let call = Duration::from_millis(1250);
    assert_eq!(Deadline::After(call).resolve(client_deadline).ok(), Some(Some(call)));
    assert_eq!(Deadline::Never.resolve(client_deadline).ok(), Some(None));

    let error = Deadline::After(Duration::ZERO).resolve(client_deadline).expect_err("zero");
    assert!(matches!(error.kind(), ErrorKind::InvalidRequest), "{error:?}");
    assert_eq!(error.to_string(), "timeout must be a positive, finite number of seconds.");
}

#[test]
fn a_request_prints_its_settings_and_none_of_the_callers_data() {
    let client = client();
    let questions = questions();
    let request = client
        .system_one("my card number is 4242", &questions)
        .model("m")
        .timeout(Duration::from_secs(2))
        .header("x-api-key", "sk-live-secret")
        .extra_body("customer", "Jane Roe");

    let debug = format!("{request:?}");
    assert_eq!(
        debug,
        r#"SystemOne { questions: 1, model: Some("m"), deadline: After(2s), headers: ["x-api-key"], extra_body: ["customer"], .. }"#
    );
    for private in ["4242", "sk-live-secret", "Jane Roe"] {
        assert!(!debug.contains(private), "{private} leaked into {debug}");
    }
}
