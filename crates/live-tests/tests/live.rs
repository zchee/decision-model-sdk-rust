//! The Python SDK's `tests/test_integration.py`, against the live API, and
//! the idle-connection check.
//!
//! Run them with the vendor's API and model in the environment, for
//! TypeSafe AI's:
//!
//! ```sh
//! DECISION_MODEL_LIVE_TESTS=1 DECISION_MODEL_API_KEY=... \
//!     DECISION_MODEL_BASE_URL=https://api.typesafe.ai DECISION_MODEL_DEFAULT_MODEL=jev-latest \
//!     cargo nextest run -p decision-model-sdk-live-tests
//! ```
//!
//! Nothing else in the repository runs them.

use std::{
    collections::BTreeMap,
    time::{Duration, Instant},
};

use decision_model_sdk::{
    ChoiceAnswer, Client, Content, ErrorKind, NoulAnswer, QuestionSet, Questions, RetryPolicy,
    ScoreAnswer,
    question::{Choice, Noul, Score},
};
use live_tests::live_client;
use serde::Serialize;

/// The tone options both System One tests offer.
const TONES: [&str; 3] = ["calm", "frustrated", "angry"];

/// The urgency levels both System One tests offer, lowest first.
const URGENCY: [&str; 3] = ["can wait", "this week", "today"];

/// How far a set of probabilities may sum from one, as the Python tests allow.
const PROBABILITY_SLACK: f64 = 0.1;

/// The support ticket the Python tests ask about, as a JSON object.
fn ticket(body: &'static str) -> BTreeMap<&'static str, &'static str> {
    BTreeMap::from([("subject", "Charged twice this month"), ("body", body)])
}

/// Fails unless `probabilities` sum to one, within [`PROBABILITY_SLACK`].
fn assert_sums_to_one(what: &str, probabilities: impl Iterator<Item = f64>) {
    let sum: f64 = probabilities.sum();
    assert!(
        (sum - 1.0).abs() <= PROBABILITY_SLACK,
        "the probabilities of {what} sum to {sum}, not 1 within {PROBABILITY_SLACK}"
    );
}

/// The criteria of the billing question, in the shape the Python test sends:
/// what a yes means, with an example.
#[derive(Serialize)]
struct Criterion {
    meaning: &'static str,
    examples: [&'static str; 1],
}

#[tokio::test]
async fn live_models() {
    let client = live_client();
    let list = client.models().list().send().await.expect("the live API lists its models");
    println!("the live API lists {} models", list.models().len());
    assert!(!list.models().is_empty(), "the live API listed no models");
    for model in list.models() {
        println!("  {} ({}): {}", model.name(), model.release_date(), model.description());
        assert!(!model.name().is_empty(), "a model card without a name: {model:?}");
    }
}

#[tokio::test]
async fn live_questions() {
    let client = live_client();
    let state = ticket("I see two charges of $49. I only have one account. Please fix this ASAP.");
    let billing =
        Content::json(&Criterion { meaning: "Payments or invoices", examples: ["charged twice"] })
            .expect("a struct of strings is a JSON object");
    let questions = Questions::new()
        .noul("billing", Noul::new().instructions("Is this ticket about billing?").yes(billing))
        .choice("tone", Choice::new(TONES).instructions("What is the customer's tone?"))
        .score("urgency", Score::new(URGENCY).instructions("How urgent is this ticket?"))
        .prepare()
        .expect("the questions are valid");

    let response =
        client.system_one(&state, &questions).send().await.expect("the live API answers");
    println!("model {:?}, usage {:?}", response.model(), response.usage());
    println!("answers {:?}", response.answers());

    assert!(!response.model().is_empty(), "the answer names no model");
    // `output_tokens` is unsigned, so the Python test's `>= 0` holds by type.
    if let Some(input) = response.usage().input_tokens() {
        assert!(input > 0, "a request with a state and three questions used {input} input tokens");
    }

    let answers = response.answers();
    let billing = answers.noul("billing").expect("a noul answer named billing");
    assert!((0.0..=1.0).contains(&billing.noul()), "billing noul {}", billing.noul());

    let tone = answers.choice("tone").expect("a choice answer named tone");
    assert!(TONES.contains(&tone.choice()), "tone picked {:?}", tone.choice());
    assert_sums_to_one("tone", tone.probabilities().map(|(_, probability)| probability));

    let urgency = answers.score("urgency").expect("a score answer named urgency");
    assert!((0.0..=2.0).contains(&urgency.score()), "urgency scored {}", urgency.score());
    let legend: Vec<(u32, Option<&str>)> =
        urgency.legend().map(|(level, description)| (level, description.as_text())).collect();
    assert_eq!(legend, [(0, Some(URGENCY[0])), (1, Some(URGENCY[1])), (2, Some(URGENCY[2]))]);
    let levels: Vec<u32> = urgency.probabilities().map(|(level, _)| level).collect();
    assert_eq!(levels, [0, 1, 2], "urgency's probabilities cover other levels");
    assert_sums_to_one("urgency", urgency.probabilities().map(|(_, probability)| probability));
}

/// The typed form of `live_questions`' questions: the Python test's
/// `PydanticQuestionsResponse`.
#[derive(QuestionSet)]
struct Triage {
    #[noul(instructions = "Is this ticket about billing?")]
    billing: NoulAnswer,
    #[choice(
        instructions = "What is the customer's tone?",
        options("calm", "frustrated", "angry")
    )]
    tone: ChoiceAnswer,
    #[score(instructions = "How urgent is this ticket?", levels("can wait", "this week", "today"))]
    urgency: ScoreAnswer,
}

#[tokio::test]
async fn live_typed_response() {
    let client = live_client();
    let state = ticket("I see two charges of $49. Please fix this ASAP.");

    let response = client.ask::<Triage>(&state).send().await.expect("the live API answers");
    println!("request id {:?}", response.meta().request_id());

    let answers = response.answers();
    assert!(
        (0.0..=1.0).contains(&answers.billing.noul()),
        "billing noul {}",
        answers.billing.noul()
    );
    assert!(TONES.contains(&answers.tone.choice()), "tone picked {:?}", answers.tone.choice());
    assert!(
        (0.0..=2.0).contains(&answers.urgency.score()),
        "urgency scored {}",
        answers.urgency.score()
    );
    assert!(response.meta().request_id().is_some(), "the answer carries no request id");
}

/// How long the connection is left without a request: longer than the
/// 60-second idle timeout common on load balancers, shorter than the 90
/// seconds after which the client closes an idle pooled connection itself.
const IDLE: Duration = Duration::from_secs(75);

/// The deadline of each timed call. A connection the load balancer dropped
/// without telling the client would hold a request until its deadline, so it
/// is short enough to fail on that instead of waiting out
/// [`live_tests::LIVE_TIMEOUT`].
const TIMED_CALL_TIMEOUT: Duration = Duration::from_secs(30);

/// Lists the models once, with no retry, and returns how long the call took.
///
/// No retry, so a request written to a dead connection cannot hide behind a
/// second attempt on a new one.
///
/// # Panics
///
/// When the call fails. A call that runs into [`TIMED_CALL_TIMEOUT`] is
/// reported as what it most likely is: a request stranded on a connection
/// the far side dropped without telling the client.
async fn timed_list(client: &Client) -> Duration {
    let started = Instant::now();
    let listed = client
        .models()
        .list()
        .timeout(TIMED_CALL_TIMEOUT)
        .retry(RetryPolicy::new().max_retries(0))
        .send()
        .await;
    let elapsed = started.elapsed();
    match listed {
        Ok(_) => elapsed,
        Err(error) if matches!(error.kind(), ErrorKind::Timeout { .. }) => panic!(
            "the listing ran into its {TIMED_CALL_TIMEOUT:?} deadline after {elapsed:?}: the \
             request was stranded on a connection that was dropped without the client being \
             told ({error})"
        ),
        Err(error) => panic!("the live API did not list its models: {error}"),
    }
}

/// After 75 seconds without a request, the next call succeeds on its first
/// attempt and costs no more than a call on a new client, plus a second. The
/// cost of a call on a new client is the slower of two, each on a client of
/// its own, so that one fast sample does not set the bound.
///
/// The pause is chosen against two clocks. A load balancer commonly closes a
/// connection idle for 60 seconds, unless it counts the client's HTTP/2 PING
/// (sent every 30 seconds, idle or not) as activity; and the client closes a
/// pooled connection idle for 90 seconds on its own (`POOL_IDLE_TIMEOUT` in
/// `crates/sdk/src/transport/hyper.rs`), after which the next call always
/// opens a new connection and this test could tell nothing. At 75 seconds the
/// call after the pause either finds the connection open - the load balancer
/// counted the PINGs, and the call costs about one round trip, like a warm
/// one - or opens a new one and costs about what the first call on a new
/// client did.
///
/// Which of the two happens is the load balancer's behaviour, not a defect,
/// so the test only prints it. What it asserts is what must hold either way:
/// the call succeeds with retries off, so it was not written to a connection
/// the far side had already dropped, and it takes no longer than the cold
/// call plus one second, which a request stranded on a dead connection until
/// its deadline would exceed.
#[tokio::test]
async fn call_after_idle_pause_succeeds_within_cold_call_plus_one_second() {
    let first = live_client();
    let first_cold = timed_list(&first).await;
    drop(first);
    let client = live_client();
    let second_cold = timed_list(&client).await;
    let cold = first_cold.max(second_cold);
    println!("cold calls on two new clients {first_cold:?} and {second_cold:?}");
    let mut warm = Duration::MAX;
    for _ in 0..3 {
        warm = warm.min(timed_list(&client).await);
    }
    println!("cold call {cold:?}, fastest of three warm calls {warm:?}; pausing {IDLE:?}");

    tokio::time::sleep(IDLE).await;
    let after = timed_list(&client).await;

    let reading = if after < (cold + warm) / 2 {
        "reused: about one round trip"
    } else {
        "reopened: about a cold call"
    };
    println!(
        "call after the {IDLE:?} pause took {after:?} ({reading}; warm {warm:?}, cold {cold:?})"
    );
    let bound = cold + Duration::from_secs(1);
    assert!(
        after <= bound,
        "the call after the {IDLE:?} pause took {after:?}, more than the cold call plus one \
         second ({bound:?}): it waited on a connection before a working one was used"
    );
}
