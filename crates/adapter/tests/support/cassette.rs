//! The reader of the recorded upstream exchanges under
//! `tests/fixtures/cassettes`.
//!
//! Each file is one exchange that upstream's VCR recorder wrote
//! (`U:tests/conftest.py:45-88`): `version` 1 and an `interactions` array
//! holding one request and its response, both bodies stored as JSON objects
//! instead of the text that crossed the wire. `Cassette::read` takes one name
//! of `CASSETTES` and returns what a replay needs: the status and body to
//! serve, and the recorded request to hold the client's request against.
//!
//! `Cassette::matches` is upstream's matcher (`U:tests/conftest.py:129`):
//! method, path, query and body, the body compared as parsed JSON, so neither
//! member order nor number spelling (`1` against `1.0`) counts, as with
//! Python's `==`. Scheme, host and port are not compared: the test points the
//! provider's base URL at a local server, which decides all three. A body
//! mismatch names the first differing JSON pointer (RFC 6901).
//!
//! Every item here is used by each test binary that includes this file, so no
//! lint is suppressed. `expected.rs` builds on `first_difference` and needs
//! this file declared as `mod cassette` beside it.

use std::fmt;

use bytes::Bytes;
use http::{Method, StatusCode, Uri};
use serde_json::{Map, Value};

/// The file stems under `tests/fixtures/cassettes`, each the name of the
/// upstream test case that recorded it (`request.node.name`), in the order of
/// `U:tests/test_client_with_live_apis.py:146-246`. The provider tests replay
/// the 24 vendor cases, 8 per provider.
pub(crate) const CASSETTES: [&str; 25] = [
    "test_live_responses_match_reference_shape[probabilities-prompted-openai]",
    "test_live_responses_match_reference_shape[probabilities-prompted-anthropic]",
    "test_live_responses_match_reference_shape[probabilities-prompted-gemini]",
    "test_live_responses_match_reference_shape[probabilities-native-openai]",
    "test_live_responses_match_reference_shape[probabilities-native-anthropic]",
    "test_live_responses_match_reference_shape[probabilities-native-gemini]",
    "test_live_responses_match_reference_shape[discrete-prompted-openai]",
    "test_live_responses_match_reference_shape[discrete-prompted-anthropic]",
    "test_live_responses_match_reference_shape[discrete-prompted-gemini]",
    "test_live_responses_match_reference_shape[discrete-native-openai]",
    "test_live_responses_match_reference_shape[discrete-native-anthropic]",
    "test_live_responses_match_reference_shape[discrete-native-gemini]",
    "test_live_models_follow_question_instructions_and_criteria[probabilities-prompted-openai]",
    "test_live_models_follow_question_instructions_and_criteria[probabilities-prompted-anthropic]",
    "test_live_models_follow_question_instructions_and_criteria[probabilities-prompted-gemini]",
    "test_live_models_follow_question_instructions_and_criteria[probabilities-native-openai]",
    "test_live_models_follow_question_instructions_and_criteria[probabilities-native-anthropic]",
    "test_live_models_follow_question_instructions_and_criteria[probabilities-native-gemini]",
    "test_live_models_follow_question_instructions_and_criteria[discrete-prompted-openai]",
    "test_live_models_follow_question_instructions_and_criteria[discrete-prompted-anthropic]",
    "test_live_models_follow_question_instructions_and_criteria[discrete-prompted-gemini]",
    "test_live_models_follow_question_instructions_and_criteria[discrete-native-openai]",
    "test_live_models_follow_question_instructions_and_criteria[discrete-native-anthropic]",
    "test_live_models_follow_question_instructions_and_criteria[discrete-native-gemini]",
    // Deliberately unreplayed: it records the TypeSafe API, not a vendor, and
    // maps to the SDK's live test `live_questions` (plan section 7.2). It is
    // listed so that every file of the directory is named.
    "test_live_typesafe_response_matches_reference_shape",
];

const DIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/cassettes");

/// One recorded exchange.
#[derive(Debug)]
pub(crate) struct Cassette {
    file: String,
    /// What the recorded client sent.
    pub(crate) request: Request,
    /// What the vendor answered.
    pub(crate) response: Response,
}

/// The recorded request.
#[derive(Debug)]
pub(crate) struct Request {
    method: Method,
    uri: Uri,
    /// The JSON body.
    pub(crate) body: Value,
}

/// The recorded response. Its one header is `content-type:
/// application/json`, which the reader checks and a replay sends again.
#[derive(Debug)]
pub(crate) struct Response {
    /// The status code.
    pub(crate) status: StatusCode,
    /// The body as compact JSON text, the form upstream's serializer hands
    /// back to VCR (`U:tests/conftest.py:59-62`), except that a float may be
    /// spelled differently (`1e-5` where Python writes `1e-05`).
    pub(crate) body: Bytes,
}

/// A cassette the reader cannot take, with the file and the JSON pointer of
/// the member at fault.
#[derive(Debug)]
pub(crate) struct CassetteError {
    /// The file name, `<stem>.json`.
    pub(crate) file: String,
    /// The JSON pointer of the member; empty for the whole file.
    pub(crate) member: String,
    reason: String,
}

impl fmt::Display for CassetteError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "cassette `{}`, member `{}`: {}", self.file, self.member, self.reason)
    }
}

/// A request that does not match its cassette, with the part that differs.
#[derive(Debug)]
pub(crate) struct Mismatch {
    file: String,
    part: &'static str,
    detail: String,
}

impl fmt::Display for Mismatch {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "the request does not match cassette `{}`: {} {}",
            self.file, self.part, self.detail
        )
    }
}

impl Cassette {
    /// Reads the cassette `name`, which must be one of `CASSETTES`.
    pub(crate) fn read(name: &str) -> Result<Self, CassetteError> {
        let file = format!("{name}.json");
        let refuse =
            |reason: String| CassetteError { file: file.clone(), member: String::new(), reason };
        if !CASSETTES.contains(&name) {
            return Err(refuse("not one of the listed cassettes".to_owned()));
        }
        let text = std::fs::read_to_string(format!("{DIR}/{file}"))
            .map_err(|error| refuse(format!("unreadable: {error}")))?;
        Self::parse(&file, &text)
    }

    /// Reads the cassette text `text`; `file` names it in the errors.
    pub(crate) fn parse(file: &str, text: &str) -> Result<Self, CassetteError> {
        let root: Value = serde_json::from_str(text).map_err(|error| CassetteError {
            file: file.to_owned(),
            member: String::new(),
            reason: format!("not JSON: {error}"),
        })?;
        let root = Node { file, pointer: String::new(), value: &root };

        let interaction = root.get("interactions")?.first()?;

        let request = interaction.get("request")?;
        let method = request.get("method")?;
        let method =
            Method::from_bytes(method.str()?.as_bytes()).map_err(|error| method.error(error))?;
        let uri = request.get("uri")?;
        let uri = uri.str()?.parse::<Uri>().map_err(|error| uri.error(error))?;
        let body = request.get("body")?.value.clone();

        let response = interaction.get("response")?;
        let code = response.get("status")?.get("code")?;
        let status = code
            .value
            .as_u64()
            .and_then(|code| u16::try_from(code).ok())
            .and_then(|code| StatusCode::from_u16(code).ok())
            .ok_or_else(|| code.error("not an HTTP status code"))?;
        let content_type = response.get("headers")?.get("content-type")?;
        if !matches!(content_type.array()?, [only] if only == "application/json") {
            return Err(content_type.error("not the one value `application/json`"));
        }
        let response_body = response.get("body")?.get("string")?;
        let response_body = serde_json::to_vec(response_body.value).expect("a JSON value encodes");

        Ok(Self {
            file: file.to_owned(),
            request: Request { method, uri, body },
            response: Response { status, body: Bytes::from(response_body) },
        })
    }

    /// Holds a request a client sent against the recorded one: method, path,
    /// query, and the body as parsed JSON.
    ///
    /// The query is compared as its `name=value` pairs in sorted order, as
    /// VCR does, but without percent-decoding them: no cassette has a query.
    pub(crate) fn matches(&self, method: &Method, uri: &Uri, body: &[u8]) -> Result<(), Mismatch> {
        let mismatch = |part, detail| Err(Mismatch { file: self.file.clone(), part, detail });
        let recorded = &self.request;
        if *method != recorded.method {
            return mismatch("method", format!("`{method}`, recorded `{}`", recorded.method));
        }
        if uri.path() != recorded.uri.path() {
            return mismatch(
                "path",
                format!("`{}`, recorded `{}`", uri.path(), recorded.uri.path()),
            );
        }
        if query_pairs(uri.query()) != query_pairs(recorded.uri.query()) {
            let query = |uri: &Uri| uri.query().unwrap_or_default().to_owned();
            return mismatch(
                "query",
                format!("`{}`, recorded `{}`", query(uri), query(&recorded.uri)),
            );
        }
        let sent: Value = match serde_json::from_slice(body) {
            Ok(sent) => sent,
            Err(error) => return mismatch("body", format!("is not JSON: {error}")),
        };
        match first_difference(&recorded.body, &sent) {
            Some(difference) => mismatch("body", format!("differs {difference}")),
            None => Ok(()),
        }
    }
}

fn query_pairs(query: Option<&str>) -> Vec<&str> {
    let mut pairs: Vec<&str> =
        query.unwrap_or_default().split('&').filter(|pair| !pair.is_empty()).collect();
    pairs.sort_unstable();
    pairs
}

/// One member of a cassette being read: where it is, for the errors.
struct Node<'a> {
    file: &'a str,
    pointer: String,
    value: &'a Value,
}

impl<'a> Node<'a> {
    fn error(&self, reason: impl fmt::Display) -> CassetteError {
        CassetteError {
            file: self.file.to_owned(),
            member: self.pointer.clone(),
            reason: reason.to_string(),
        }
    }

    fn child(&self, key: &str, value: &'a Value) -> Self {
        let mut pointer = self.pointer.clone();
        push_token(&mut pointer, key);
        Node { file: self.file, pointer, value }
    }

    fn object(&self) -> Result<&'a Map<String, Value>, CassetteError> {
        self.value
            .as_object()
            .ok_or_else(|| self.error(format_args!("{} is not a JSON object", kind(self.value))))
    }

    fn array(&self) -> Result<&'a [Value], CassetteError> {
        match self.value {
            Value::Array(items) => Ok(items),
            other => Err(self.error(format_args!("{} is not a JSON array", kind(other)))),
        }
    }

    fn str(&self) -> Result<&'a str, CassetteError> {
        self.value
            .as_str()
            .ok_or_else(|| self.error(format_args!("{} is not a JSON string", kind(self.value))))
    }

    fn get(&self, key: &str) -> Result<Self, CassetteError> {
        match self.object()?.get(key) {
            Some(value) => Ok(self.child(key, value)),
            None => Err(self.child(key, &Value::Null).error("missing")),
        }
    }

    fn first(&self) -> Result<Self, CassetteError> {
        match self.array()? {
            [first, ..] => Ok(self.child("0", first)),
            [] => Err(self.error("holds no interaction")),
        }
    }
}

fn kind(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "a boolean",
        Value::Number(_) => "a number",
        Value::String(_) => "a string",
        Value::Array(_) => "an array",
        Value::Object(_) => "an object",
    }
}

/// Appends `/token` to a JSON pointer, escaping `~` and `/` (RFC 6901
/// section 3).
fn push_token(pointer: &mut String, token: &str) {
    pointer.push('/');
    for c in token.chars() {
        match c {
            '~' => pointer.push_str("~0"),
            '/' => pointer.push_str("~1"),
            c => pointer.push(c),
        }
    }
}

/// Where two JSON values first differ.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Difference {
    /// The JSON pointer of the differing member; empty for the whole value.
    pub(crate) pointer: String,
    /// The two values found there, shortened.
    pub(crate) detail: String,
}

impl fmt::Display for Difference {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "at `{}`: {}", self.pointer, self.detail)
    }
}

/// The first place where `found` differs from `expected`, walking object
/// members in sorted name order and array items in order, or `None` when the
/// two are equal. Numbers compare by value, so `1` equals `1.0`.
pub(crate) fn first_difference(expected: &Value, found: &Value) -> Option<Difference> {
    let mut pointer = String::new();
    walk(Some(expected), Some(found), &mut pointer)
}

fn walk(
    expected: Option<&Value>,
    found: Option<&Value>,
    pointer: &mut String,
) -> Option<Difference> {
    let (Some(expected), Some(found)) = (expected, found) else {
        return Some(differs(expected, found, pointer));
    };
    match (expected, found) {
        (Value::Object(expected), Value::Object(found)) => {
            let mut keys: Vec<&String> = expected.keys().chain(found.keys()).collect();
            keys.sort_unstable();
            keys.dedup();
            keys.into_iter().find_map(|key| {
                let len = pointer.len();
                push_token(pointer, key);
                let difference = walk(expected.get(key), found.get(key), pointer);
                pointer.truncate(len);
                difference
            })
        }
        (Value::Array(expected), Value::Array(found)) => (0..expected.len().max(found.len()))
            .find_map(|index| {
                let len = pointer.len();
                push_token(pointer, &index.to_string());
                let difference = walk(expected.get(index), found.get(index), pointer);
                pointer.truncate(len);
                difference
            }),
        (Value::Number(a), Value::Number(b)) if same_number(a, b) => None,
        (a, b) if a == b => None,
        (a, b) => Some(differs(Some(a), Some(b), pointer)),
    }
}

fn same_number(a: &serde_json::Number, b: &serde_json::Number) -> bool {
    if let (Some(a), Some(b)) = (a.as_i64(), b.as_i64()) {
        return a == b;
    }
    if let (Some(a), Some(b)) = (a.as_u64(), b.as_u64()) {
        return a == b;
    }
    a.as_f64() == b.as_f64()
}

fn differs(expected: Option<&Value>, found: Option<&Value>, pointer: &str) -> Difference {
    Difference {
        pointer: pointer.to_owned(),
        detail: format!("expected {}, found {}", render(expected), render(found)),
    }
}

/// A value as JSON text, cut after 80 characters: the fixtures hold whole
/// documents and schemas.
fn render(value: Option<&Value>) -> String {
    let Some(value) = value else {
        return "nothing".to_owned();
    };
    let text = value.to_string();
    match text.char_indices().nth(80) {
        Some((end, _)) => format!("{}...", &text[..end]),
        None => text,
    }
}
