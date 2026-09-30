//! `#[derive(QuestionSet)]` from the caller's side: the questions it compiles
//! are the runtime builder's bytes, a derived set is asked with
//! `Client::ask` and decodes the way the `AnswerSet` contract says, the
//! expansion compiles wherever a caller puts it, and misuse is refused at
//! compile time with a message that says what to write.

use std::{
    env,
    path::{Path, PathBuf},
    process::Command,
    sync::atomic::{AtomicUsize, Ordering},
};

use http::{HeaderValue, StatusCode};
use test_support::{Protocol, RecordedRequest, TestServer, json_response};
use typesafe_sdk::{
    Answers, ApiErrorKind, Choice, ChoiceAnswer, Client, Error, ErrorKind, Noul, NoulAnswer,
    PreparedQuestions, QuestionSet, Questions, RetryPolicy, Score, ScoreAnswer, SystemOneResponse,
};

// ------------------------------------------------------------- sets

/// The example of the SDK's documentation.
#[expect(dead_code, reason = "only its questions are compared; no response is decoded into it")]
#[derive(Debug, QuestionSet)]
struct Ticket {
    #[noul(instructions = "Is this about billing?", yes = "payments or invoices")]
    billing: NoulAnswer,
    #[choice(instructions = "What is the tone?", options("calm" = "neutral or polite", "angry"))]
    tone: ChoiceAnswer,
    #[score(instructions = "How urgent?", levels("can wait", "this week", "today"))]
    urgency: ScoreAnswer,
}

/// The questions the upstream `RESULT` fixture answers.
#[derive(Debug, PartialEq, QuestionSet)]
struct Review {
    #[noul(instructions = "Spam?")]
    spam: NoulAnswer,
    #[choice(instructions = "Tone?", options("friendly", "hostile"))]
    tone: ChoiceAnswer,
    #[score(instructions = "Quality?", levels("bad", "ok", "great"))]
    quality: ScoreAnswer,
}

/// Every corner of the grammar that changes the bytes: nothing set, one
/// outcome only, escapes and characters outside the BMP, a name that is not an
/// identifier, a raw identifier, an empty choice, a DEL and the last code
/// point.
#[expect(dead_code, reason = "only its questions are compared; no response is decoded into it")]
#[derive(Debug, QuestionSet)]
struct Everything {
    #[noul]
    bare: NoulAnswer,
    #[noul(no = "not \"spam\"")]
    only_no: NoulAnswer,
    #[question(name = "is spam?\n\u{1F30D}")]
    #[noul(instructions = "tab\there, NUL \u{0} and \\ backslash", yes = "\u{1F}\u{7F}")]
    renamed: NoulAnswer,
    #[choice(options())]
    empty: ChoiceAnswer,
    #[choice(options("\u{7F}" = "DEL", "\u{10FFFF}"))]
    odd_options: ChoiceAnswer,
    #[score(levels("\u{2028}", "</script>"))]
    r#type: ScoreAnswer,
}

// ------------------------------------------------------------- the bytes

/// AC-F7(b): the compiled set equals the runtime set, bytes, names and
/// name boundaries, and prints the JSON the runtime pins.
#[test]
fn the_derived_ticket_is_the_runtime_questions_byte_for_byte() {
    let runtime = Questions::new()
        .noul(
            "billing",
            Noul::new().instructions("Is this about billing?").yes("payments or invoices"),
        )
        .choice(
            "tone",
            Choice::new(["calm", "angry"])
                .option("calm", "neutral or polite")
                .instructions("What is the tone?"),
        )
        .score(
            "urgency",
            Score::new(["can wait", "this week", "today"]).instructions("How urgent?"),
        )
        .prepare()
        .expect("the runtime set is valid");

    assert_eq!(Ticket::prepared(), &runtime);
    assert_eq!(
        format!("{:?}", Ticket::prepared()),
        concat!(
            r#"PreparedQuestions { json: "{\"billing\":{\"type\":\"noul\",\"instructions\":\"Is this about billing?\",\"criteria\":{\"true\":\"payments or invoices\"}},"#,
            r#"\"tone\":{\"type\":\"choice\",\"instructions\":\"What is the tone?\",\"criteria\":{\"calm\":\"neutral or polite\",\"angry\":null}},"#,
            r#"\"urgency\":{\"type\":\"score\",\"instructions\":\"How urgent?\",\"criteria\":[\"can wait\",\"this week\",\"today\"]}}" }"#,
        )
    );
    assert_eq!(Ticket::prepared().names().collect::<Vec<_>>(), ["billing", "tone", "urgency"]);
    assert_eq!(Ticket::prepared().len(), 3);
    assert!(!Ticket::prepared().is_empty());
}

#[test]
fn every_byte_changing_corner_matches_the_runtime() {
    let runtime = Questions::new()
        .noul("bare", Noul::new())
        .noul("only_no", Noul::new().no("not \"spam\""))
        .noul(
            "is spam?\n\u{1F30D}",
            Noul::new().instructions("tab\there, NUL \u{0} and \\ backslash").yes("\u{1F}\u{7F}"),
        )
        .choice("empty", Choice::new(Vec::<&str>::new()))
        .choice("odd_options", Choice::new(["\u{7F}", "\u{10FFFF}"]).option("\u{7F}", "DEL"))
        .score("type", Score::new(["\u{2028}", "</script>"]))
        .prepare()
        .expect("the runtime set is valid");

    assert_eq!(Everything::prepared(), &runtime);
    assert_eq!(format!("{:?}", Everything::prepared()), format!("{runtime:?}"));
    assert_eq!(
        Everything::prepared().names().collect::<Vec<_>>(),
        ["bare", "only_no", "is spam?\n\u{1F30D}", "empty", "odd_options", "type"]
    );
}

/// `prepared()` is one `static`: the same set on every call, from every
/// thread.
#[test]
fn prepared_is_one_static_set() {
    let first: *const PreparedQuestions = Ticket::prepared();
    let from_thread =
        std::thread::spawn(|| Ticket::prepared() as *const PreparedQuestions as usize)
            .join()
            .expect("the thread does not panic");
    assert!(std::ptr::eq(first, Ticket::prepared()));
    assert_eq!(first as usize, from_thread);
    // A clone shares the compiled bytes too.
    assert_eq!(Ticket::prepared().clone(), *Ticket::prepared());
}

/// A set is usable through the trait alone.
#[test]
fn a_question_set_is_a_bound_like_any_other() {
    fn names<Q: QuestionSet>() -> Vec<&'static str> {
        Q::prepared().names().collect()
    }
    assert_eq!(names::<Review>(), ["spam", "tone", "quality"]);
}

// ------------------------------------------------------------- asking

/// `RESULT` of `tests/test_clients.py:42-56`.
const RESULT: &[u8] = include_bytes!("fixtures/result.json");

/// The body sent when a `Review` is asked about "I was charged twice.".
const REVIEW_BODY: &str = concat!(
    r#"{"state":"I was charged twice.","model":"jev-latest","questions":"#,
    r#"{"spam":{"type":"noul","instructions":"Spam?"},"#,
    r#""tone":{"type":"choice","instructions":"Tone?","criteria":{"friendly":null,"hostile":null}},"#,
    r#""quality":{"type":"score","instructions":"Quality?","criteria":["bad","ok","great"]}}}"#,
);

#[path = "support/answering.rs"]
mod answering;

use answering::answering;

fn client_for(server: &TestServer) -> Client {
    Client::builder()
        .api_key("test-key")
        .base_url(server.base_url())
        .default_model("jev-latest")
        .build()
        .expect("the client builds")
}

/// Asks `Q` of a server that answers `body`.
async fn ask<Q: QuestionSet>(body: impl AsRef<[u8]>) -> Result<SystemOneResponse<Q>, Error> {
    let server = answering(Protocol::Http1, StatusCode::OK, body).await;
    client_for(&server).ask::<Q>("I was charged twice.").send().await
}

/// The response-validation failure of a decode that must fail: its field
/// path, its message, and the decoder's own account of the failure.
fn validation(error: &Error) -> (&str, &str, String) {
    let ErrorKind::ResponseValidation(failure) = error.kind() else {
        panic!("expected a response-validation error, got {error:?}");
    };
    (failure.field_path(), failure.message(), failure.decode_error().to_string())
}

/// `ask` sends the compiled questions in the body `system_one` would send,
/// and decodes into the struct exactly what the lookup finds by name.
#[tokio::test]
async fn ask_sends_the_compiled_questions_and_decodes_into_the_struct() {
    let server = answering(Protocol::Http1, StatusCode::OK, RESULT).await;
    let client = client_for(&server);

    let typed = client
        .ask::<Review>("I was charged twice.")
        .header("x-team", "billing")
        .send()
        .await
        .expect("the fixture decodes as a Review");
    let looked_up = client
        .system_one("I was charged twice.", Review::prepared())
        .header("x-team", "billing")
        .send()
        .await
        .expect("the fixture decodes as Answers");

    let requests = server.requests();
    assert_eq!(requests.len(), 2);
    for request in &requests {
        assert_eq!(std::str::from_utf8(&request.body).expect("UTF-8"), REVIEW_BODY);
        assert_eq!(request.headers["x-team"], "billing");
    }

    let answers: &Answers = looked_up.answers();
    assert_eq!(Some(&typed.answers().spam), answers.noul("spam"));
    assert_eq!(Some(&typed.answers().tone), answers.choice("tone"));
    assert_eq!(Some(&typed.answers().quality), answers.score("quality"));
    assert_eq!(typed.answers().spam.noul(), 0.98);
    assert_eq!(typed.answers().tone.choice(), "friendly");
    assert_eq!(typed.model(), "jev-latest");
    assert_eq!(typed.meta().raw_body().as_ref(), RESULT);
}

/// The `X-TypeSafe-Retry-Count` values of each request.
fn retry_counts(requests: &[RecordedRequest]) -> Vec<Vec<&str>> {
    requests.iter().map(|request| request.header_values("x-typesafe-retry-count")).collect()
}

/// The error of a call whose last attempt the server answered with 503
/// `down`: an API error, whole.
#[track_caller]
fn assert_unavailable(error: &Error, endpoint: &str) {
    let ErrorKind::Api(api) = error.kind() else {
        panic!("expected an API error, got {error:?}");
    };
    assert_eq!(api.kind(), ApiErrorKind::InternalServer);
    assert_eq!(api.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(api.message(), "down");
    assert_eq!(api.body(), br#"{"message": "down"}"#);
    assert_eq!(api.request_id(), Some("req-error"));
    assert_eq!(error.to_string(), format!("{endpoint}: 503 down (request_id=req-error)"));
}

/// A derived set is retried like any other call: `ask::<T>().retry(..)`
/// replaces the client's policy for that call, and without it the client's
/// default applies. Every 503 asks for no wait (`retry-after-ms: 0`), which
/// the default policy respects, so its 0.5 s and 1 s backoff is never slept.
#[tokio::test]
async fn an_asked_request_is_retried_by_the_policy_that_applies_to_it() {
    let served = AtomicUsize::new(0);
    let server = TestServer::start(Protocol::Http1, move |_| {
        // One failure for the call that may not retry, three for the call
        // that retries twice, then the fixture.
        let response = if served.fetch_add(1, Ordering::SeqCst) < 4 {
            let mut response =
                json_response(StatusCode::SERVICE_UNAVAILABLE, r#"{"message": "down"}"#);
            response.headers_mut().insert("retry-after-ms", HeaderValue::from_static("0"));
            response
                .headers_mut()
                .insert("x-typesafe-request-id", HeaderValue::from_static("req-error"));
            response
        } else {
            json_response(StatusCode::OK, RESULT)
        };
        async move { response }
    })
    .await
    .expect("the test server starts");
    let client = client_for(&server);
    let endpoint = format!("POST {}/v1/systemone", server.base_url());

    let error = client
        .ask::<Review>("I was charged twice.")
        .retry(RetryPolicy::default().max_retries(0))
        .send()
        .await
        .expect_err("the one attempt fails");
    assert_unavailable(&error, &endpoint);
    assert_eq!(server.request_count(), 1, "a call that may not retry is sent once");

    let error =
        client.ask::<Review>("I was charged twice.").send().await.expect_err("every attempt fails");
    assert_unavailable(&error, &endpoint);
    let requests = server.requests();
    assert_eq!(requests.len(), 4, "the client's default retries twice");
    assert_eq!(retry_counts(&requests[1..]), [vec![], vec!["1"], vec!["2"]]);

    let response = client
        .ask::<Review>("I was charged twice.")
        .send()
        .await
        .expect("the fixture decodes as a Review");
    let requests = server.requests();
    assert_eq!(requests.len(), 5, "a success is not retried");
    assert_eq!(retry_counts(&requests[4..]), [Vec::<&str>::new()]);
    assert_eq!(response.answers().spam.noul(), 0.98);
    assert_eq!(response.answers().tone.choice(), "friendly");
    assert_eq!(response.answers().quality.score(), 1.7);
    assert_eq!(response.meta().raw_body().as_ref(), RESULT);

    // Every attempt of every call sent the same body, the compiled questions
    // included.
    for (index, request) in requests.iter().enumerate() {
        assert_eq!(std::str::from_utf8(&request.body).expect("UTF-8"), REVIEW_BODY, "{index}");
    }
}

const ENVELOPE_START: &str =
    r#"{"model":"jev-latest","usage":{"input_tokens":1,"output_tokens":1},"answers":"#;
const SPAM: &str = r#""spam":{"type":"noul","noul":0.25}"#;
const TONE: &str = r#""tone":{"type":"choice","choice":"hostile","confidence":0.5,"probabilities":{"friendly":0.5,"hostile":0.5}}"#;
const QUALITY: &str = r#""quality":{"type":"score","score":2.0,"confidence":1.0,"legend":{"0":"bad","2":"great"},"probabilities":{"0":0.0,"2":1.0}}"#;

fn body(answers: &str) -> String {
    format!("{ENVELOPE_START}{answers}}}")
}

/// The contract's order rule: answers in any order, and `type` after the
/// members it governs, decode alike.
#[tokio::test]
async fn answers_decode_in_any_order() {
    let reordered =
        body(&format!("{{{QUALITY},{TONE},{}}}", r#""spam":{"noul":0.25,"type":"noul"}"#));
    let response = ask::<Review>(reordered).await.expect("the body decodes");
    assert_eq!(response.answers().spam.noul(), 0.25);
    assert_eq!(response.answers().tone.choice(), "hostile");
    assert_eq!(response.answers().quality.score(), 2.0);
}

/// The contract's extra-answers and repeated-answer rules: an answer the
/// struct has no field for is skipped whatever its kind or shape, and the
/// first answer of a name is kept while a later one is skipped unread, even
/// when it is misshaped.
#[tokio::test]
async fn extra_answers_are_skipped_and_the_first_answer_of_a_name_wins() {
    let answers = format!(
        "{{{SPAM},{},{TONE},{},{QUALITY},{}}}",
        r#""future":{"type":"future","nested":[{"deep":[1,2,3]}]}"#,
        r#""spam":{"type":"noul","noul":"not a number"}"#,
        r#""extra":{"type":"noul","noul":0.75}"#,
    );
    let response = ask::<Review>(body(&answers)).await.expect("the body decodes");
    assert_eq!(response.answers().spam.noul(), 0.25);
    assert!(
        std::str::from_utf8(response.meta().raw_body()).expect("UTF-8").contains("future"),
        "a skipped answer stays in the raw body"
    );
}

/// A key written with an escape is matched by what it spells, as the field
/// identifier compares decoded text.
#[tokio::test]
async fn a_key_written_with_an_escape_is_matched() {
    let backslash = char::from(0x5C_u8);
    let escaped_spam = format!(r#""sp{backslash}u0061m":{{"type":"noul","noul":0.5}}"#);
    let response = ask::<Review>(body(&format!("{{{escaped_spam},{TONE},{QUALITY}}}")))
        .await
        .expect("the escaped key decodes");
    assert_eq!(response.answers().spam.noul(), 0.5);
}

/// A renamed question is sent and answered under its wire name.
#[tokio::test]
async fn a_renamed_question_is_answered_under_its_wire_name() {
    #[derive(Debug, QuestionSet)]
    struct Renamed {
        #[question(name = "is-spam")]
        #[noul]
        spam: NoulAnswer,
    }

    assert_eq!(Renamed::prepared().names().collect::<Vec<_>>(), ["is-spam"]);
    let response = ask::<Renamed>(body(r#"{"is-spam":{"type":"noul","noul":0.5}}"#))
        .await
        .expect("the wire name decodes");
    assert_eq!(response.answers().spam.noul(), 0.5);

    let error = ask::<Renamed>(body(&format!("{{{SPAM}}}")))
        .await
        .expect_err("the field name is not the wire name");
    assert_eq!(
        validation(&error),
        (
            "answers.is-spam",
            "Invalid response data at 'answers.is-spam'.",
            String::from("unexpected JSON value at `answers.is-spam`, line 1 column 113"),
        )
    );
}

/// The contract's missing-answer, wrong-kind and input rules: the field
/// path, the message and the decoder's account, each whole. A missing
/// answer is reported under the question's name, which is what the generated
/// `missing_field` names.
#[tokio::test]
async fn a_missing_or_wrong_answer_fails_at_its_field() {
    let wrong_kind = r#""quality":{"type":"noul","noul":0.5}"#;
    let no_answers = r#"{"model":"jev-latest","usage":{"input_tokens":1,"output_tokens":1}}"#;
    #[cfg(feature = "sonic")]
    let not_object_column = 79;
    #[cfg(not(feature = "sonic"))]
    let not_object_column = 77;
    let cases = [
        ("missing answer", body(&format!("{{{SPAM},{TONE}}}")), "answers.quality", 221),
        (
            "wrong kind",
            body(&format!("{{{SPAM},{TONE},{wrong_kind}}}")),
            "answers.quality.type",
            245,
        ),
        ("not an object", body("[]"), "answers", not_object_column),
        ("no answers member", no_answers.to_owned(), "answers", 67),
    ];
    for (case, response, path, column) in cases {
        let error = ask::<Review>(response).await.expect_err(case);
        assert_eq!(
            validation(&error),
            (
                path,
                format!("Invalid response data at '{path}'.").as_str(),
                format!("unexpected JSON value at `{path}`, line 1 column {column}"),
            ),
            "case {case}"
        );
    }
}

// ------------------------------------------------------------- hygiene

/// `#![no_implicit_prelude]`: the expansion names nothing it did not bring.
mod bare {
    #![no_implicit_prelude]

    use ::typesafe_sdk::{NoulAnswer, QuestionSet};

    #[derive(QuestionSet)]
    pub(crate) struct Bare {
        #[noul(instructions = "Spam?")]
        pub(crate) spam: NoulAnswer,
    }
}

/// Every name the expansion uses, defined differently in the caller's module,
/// and fields named like the expansion's locals.
mod shadowed {
    #![expect(dead_code, reason = "the items exist only to shadow the names they have")]
    #![expect(non_camel_case_types, reason = "the primitive names are shadowed on purpose")]
    #![expect(non_snake_case, reason = "a field is named like the expansion's static")]

    use typesafe_sdk::{ChoiceAnswer, NoulAnswer, QuestionSet, ScoreAnswer};

    pub(crate) struct Result;
    pub(crate) struct Option;
    pub(crate) struct Ok;
    pub(crate) struct Err;
    pub(crate) struct Some;
    pub(crate) struct None;
    pub(crate) struct str;
    pub(crate) struct u8;
    pub(crate) struct Formatter;
    pub(crate) struct Deserialize;
    pub(crate) struct Deserializer;
    pub(crate) struct Visitor;
    pub(crate) struct MapAccess;
    pub(crate) struct IgnoredAny;
    pub(crate) struct Error;
    pub(crate) struct PreparedQuestions;
    pub(crate) mod fmt {}
    pub(crate) mod serde {}
    pub(crate) mod core {}
    pub(crate) mod __private {}

    #[derive(QuestionSet)]
    pub(crate) struct Locals {
        #[noul]
        pub(crate) __f0: NoulAnswer,
        #[choice(options("a"))]
        pub(crate) __map: ChoiceAnswer,
        #[score(levels("low"))]
        pub(crate) __key: ScoreAnswer,
        #[noul]
        pub(crate) __answer: NoulAnswer,
        #[noul]
        pub(crate) __deserializer: NoulAnswer,
        #[noul]
        pub(crate) __formatter: NoulAnswer,
        #[noul]
        pub(crate) __value: NoulAnswer,
        #[noul]
        pub(crate) PREPARED: NoulAnswer,
    }
}

/// The crate path reached through a re-export, and through `extern crate`
/// under another name.
mod reexport {
    pub(crate) use typesafe_sdk as sdk;
}

extern crate typesafe_sdk as renamed_sdk;

mod through_other_paths {
    use typesafe_sdk::NoulAnswer;

    #[derive(renamed_sdk::QuestionSet)]
    #[question_set(crate = crate::reexport::sdk)]
    pub(crate) struct ViaReexport {
        #[noul]
        pub(crate) spam: NoulAnswer,
    }

    #[derive(renamed_sdk::QuestionSet)]
    #[question_set(crate = ::renamed_sdk)]
    pub(crate) struct ViaExternCrate {
        #[noul]
        pub(crate) spam: renamed_sdk::NoulAnswer,
    }
}

/// A caller that denies every lint a careful crate would: the expansion
/// triggers none of them.
mod strict {
    #![deny(
        warnings,
        missing_docs,
        unreachable_pub,
        unused_qualifications,
        unused_results,
        clippy::all,
        clippy::pedantic,
        clippy::nursery
    )]

    use typesafe_sdk::{NoulAnswer, QuestionSet};

    use super::{SPAM, ask, body};

    /// A documented set.
    #[derive(Debug, QuestionSet)]
    struct Strict {
        /// Its one answer.
        #[noul]
        spam: NoulAnswer,
    }

    /// The set decodes like any other. The test lives in this module so that
    /// the set needs no visibility beyond it.
    #[tokio::test]
    async fn a_set_derived_under_every_lint_decodes() {
        let response = ask::<Strict>(body(&format!("{{{SPAM}}}"))).await.expect("decodes");
        assert!((response.answers().spam.noul() - 0.25).abs() < f64::EPSILON);
    }
}

/// A set declared by a caller's `macro_rules!`: a `$ty:ty` reaches the derive
/// wrapped in an invisible group, and the answer type is still recognized.
macro_rules! one_noul_set {
    ($set:ident, $field:ident: $answer:ty) => {
        #[derive(Debug, QuestionSet)]
        struct $set {
            #[noul(instructions = "Spam?")]
            $field: $answer,
        }
    };
}

one_noul_set!(FromMacro, spam: NoulAnswer);

/// Each hygiene module's set compiles and decodes like any other.
#[tokio::test]
async fn the_expansion_works_wherever_it_lands() {
    let spam_only = body(&format!("{{{SPAM}}}"));
    assert_eq!(ask::<FromMacro>(&spam_only).await.expect("decodes").answers().spam.noul(), 0.25);
    assert_eq!(ask::<bare::Bare>(&spam_only).await.expect("decodes").answers().spam.noul(), 0.25);
    assert_eq!(
        ask::<through_other_paths::ViaReexport>(&spam_only)
            .await
            .expect("decodes")
            .answers()
            .spam
            .noul(),
        0.25
    );
    assert_eq!(
        ask::<through_other_paths::ViaExternCrate>(&spam_only)
            .await
            .expect("decodes")
            .answers()
            .spam
            .noul(),
        0.25
    );

    let locals = body(concat!(
        r#"{"__f0":{"type":"noul","noul":0.0},"#,
        r#""__map":{"type":"choice","choice":"a","confidence":1.0,"probabilities":{"a":1.0}},"#,
        r#""__key":{"type":"score","score":0.0,"confidence":1.0,"legend":{"0":"low"},"probabilities":{"0":1.0}},"#,
        r#""__answer":{"type":"noul","noul":0.1},"__deserializer":{"type":"noul","noul":0.2},"#,
        r#""__formatter":{"type":"noul","noul":0.3},"__value":{"type":"noul","noul":0.4},"#,
        r#""PREPARED":{"type":"noul","noul":0.5}}"#,
    ));
    let response = ask::<shadowed::Locals>(locals).await.expect("decodes");
    let answers = response.answers();
    assert_eq!(
        [
            answers.__f0.noul(),
            answers.__answer.noul(),
            answers.__deserializer.noul(),
            answers.__formatter.noul(),
            answers.__value.noul(),
            answers.PREPARED.noul(),
        ],
        [0.0, 0.1, 0.2, 0.3, 0.4, 0.5]
    );
    assert_eq!(answers.__map.choice(), "a");
    assert_eq!(answers.__key.score(), 0.0);
}

// ------------------------------------------------------------- misuse

/// Set in the environment of the re-run [`misuse_is_refused_at_compile_time`]
/// starts, so that the re-run never starts another.
const RERUN_MARKER: &str = "TYPESAFE_SDK_TRYBUILD_RERUN";

/// The target directory this test binary was built in, as cargo wrote it.
/// Cargo gives every integration test `CARGO_TARGET_TMPDIR`, `<target>/tmp`,
/// at compile time, wherever `--config`, the environment or a config file put
/// `<target>`.
fn target_dir_of_this_binary() -> &'static Path {
    Path::new(env!("CARGO_TARGET_TMPDIR")).parent().expect("CARGO_TARGET_TMPDIR is <target>/tmp")
}

/// The target directory trybuild will build in: it runs `cargo metadata` from
/// the package root with this process's environment, and so does this.
fn target_dir_of_trybuild() -> PathBuf {
    let cargo = env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    let output = Command::new(cargo)
        .args(["metadata", "--no-deps", "--format-version=1"])
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .output()
        .expect("cargo metadata runs");
    let metadata: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("cargo metadata prints JSON");
    let target = metadata["target_directory"].as_str().expect("metadata names the target dir");
    PathBuf::from(target)
}

/// A path in the one spelling two paths are compared in. Only for comparing:
/// on Windows it is a verbatim `\\?\C:\...` path, which is not the directory as
/// cargo wrote it and not what a child process should be handed.
fn canonical(path: &Path) -> PathBuf {
    path.canonicalize().unwrap_or_else(|_| path.to_path_buf())
}

/// AC-F7(c): each misuse is refused at compile time, at the tokens that cause
/// it, with a message that says what to write. The `.stderr` files are the
/// output of rustc 1.98.1, the toolchain `rust-toolchain.toml` pins and CI
/// uses; another rustc may word or place its own messages differently.
///
/// trybuild runs a `cargo` of its own, which reads the target directory from
/// the environment and from config files but never from the `--config` flags
/// the outer `cargo` was given. Under `cargo --config build.target-dir=...`
/// alone it would build in the workspace's `target/`, which a caller who
/// redirects builds elsewhere does not want. The environment of this process
/// cannot be changed (`std::env::set_var` is `unsafe` in edition 2024), so
/// the test runs itself again as a child process whose `CARGO_TARGET_DIR` is
/// the directory this binary was built in, and passes when the child does.
/// `--exact <name>` selects this one test under libtest and nextest alike.
#[test]
fn misuse_is_refused_at_compile_time() {
    let built_in = target_dir_of_this_binary();
    let trybuild_in = target_dir_of_trybuild();
    if canonical(&trybuild_in) != canonical(built_in) {
        assert!(
            env::var_os(RERUN_MARKER).is_none(),
            "trybuild would build in {}, not in {} where this test was built, although this is \
             the re-run whose CARGO_TARGET_DIR names that directory",
            trybuild_in.display(),
            built_in.display(),
        );
        let output = Command::new(env::current_exe().expect("the test binary has a path"))
            .args(["--exact", "misuse_is_refused_at_compile_time", "--nocapture"])
            .env("CARGO_TARGET_DIR", built_in)
            .env(RERUN_MARKER, "1")
            .output()
            .expect("the test binary runs again");
        print!("{}", String::from_utf8_lossy(&output.stdout));
        eprint!("{}", String::from_utf8_lossy(&output.stderr));
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(
            output.status.success() && stdout.contains("1 passed"),
            "the re-run with CARGO_TARGET_DIR={} failed ({}); its output is above",
            built_in.display(),
            output.status,
        );
        return;
    }
    trybuild::TestCases::new().compile_fail("tests/ui/*.rs");
}
