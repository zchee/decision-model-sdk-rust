//! What a response keeps in memory when the questions asked a large score and
//! the server answers with many small ones.
//!
//! A request passes the largest level count of its score questions to the
//! decoder, which starts a score's level lists at that capacity. The count is
//! the caller's, but the answers are the server's: nothing stops a response
//! from carrying far more score answers than were asked, each with fewer
//! levels than the hint. This test asks one score of 1,000 levels and answers
//! it with a flood of score answers whose legend and probabilities are empty
//! or hold one level, and holds what the decoded response keeps to a small
//! multiple of what the SAME body keeps when the request gives no hint at all
//! (a question set without a score). Without a hint the decoder sizes every
//! list from what it reads, so that is the baseline an unbounded hint is
//! measured against.
//!
//! "Keeps" is measured as the bytes freed when the response is dropped: every
//! block the response owns is freed then, and nothing else is, because the
//! body it keeps is a reference to the transport's buffer rather than a copy.
//!
//! One profiler exists per process, so all of it runs in a single test.

use std::{
    convert::Infallible,
    fmt::Write as _,
    future::{Ready, ready},
    pin::pin,
    task::{Context, Poll},
};

use bytes::Bytes;
use http::{Request, Response};
use tower_service::Service;
use typesafe_sdk::{Body, ClientBuilder, Noul, PreparedQuestions, Questions, Score};

// A plain wrapper type, so declaring it as the global allocator stays safe
// code even though the crate under test forbids `unsafe`.
#[global_allocator]
static ALLOC: dhat::Alloc = dhat::Alloc;

/// How many answers of each shape the flood carries.
const ANSWERS_PER_SHAPE: usize = 500;

/// The level count of the one score the hinted request asks.
const ASKED_LEVELS: usize = 1_000;

/// How much more the hinted request may keep than the unhinted one. A bounded
/// hint gives a one-level list a few more slots than it would grow to on its
/// own, which costs 1.4 times here; an unbounded one gave every list, empty
/// ones included, room for all the levels asked, which cost 188 times.
const MAX_RATIO: f64 = 2.0;

/// A transport that answers every request with the same body, shared rather
/// than copied, so that a response owns none of the body's bytes.
#[derive(Debug, Clone)]
struct Flood(Bytes);

impl Service<Request<Body>> for Flood {
    type Response = Response<Body>;
    type Error = Infallible;
    type Future = Ready<Result<Response<Body>, Infallible>>;

    fn poll_ready(&mut self, _: &mut Context<'_>) -> Poll<Result<(), Infallible>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, request: Request<Body>) -> Self::Future {
        drop(request);
        ready(Ok(Response::new(Body::from(self.0.clone()))))
    }
}

/// A response whose answers are [`ANSWERS_PER_SHAPE`] scores with an empty
/// legend and empty probabilities, then as many with one level. `type` comes
/// first in each, so every list is read as it arrives.
fn flood_body() -> Bytes {
    let mut body = String::from(
        r#"{"model":"jev-latest","usage":{"input_tokens":1,"output_tokens":1},"answers":{"#,
    );
    for index in 0..ANSWERS_PER_SHAPE {
        write!(
            body,
            r#""empty{index}":{{"type":"score","score":0,"confidence":0,"legend":{{}},"probabilities":{{}}}},"#
        )
        .expect("a String takes any write");
    }
    for index in 0..ANSWERS_PER_SHAPE {
        write!(
            body,
            r#""one{index}":{{"type":"score","score":0,"confidence":1,"legend":{{"0":"only"}},"probabilities":{{"0":1}}}},"#
        )
        .expect("a String takes any write");
    }
    body.pop();
    body.push_str("}}");
    Bytes::from(body)
}

/// One question and no score, so the request passes no level hint.
fn unhinted() -> PreparedQuestions {
    Questions::new().noul("spam", Noul::new()).prepare().expect("the questions prepare")
}

/// One score of [`ASKED_LEVELS`] levels.
fn hinted() -> PreparedQuestions {
    let levels = (0..ASKED_LEVELS).map(|level| format!("level {level}"));
    Questions::new().score("quality", Score::new(levels)).prepare().expect("the questions prepare")
}

#[test]
fn a_large_score_asked_does_not_multiply_what_small_answers_keep() {
    let _profiler = dhat::Profiler::builder().testing().build();

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()
        .expect("the runtime builds");
    let body = flood_body();
    let client = ClientBuilder::new()
        .api_key("test-key")
        .base_url("http://127.0.0.1:9")
        .default_model("jev-latest")
        .build_with_service(Flood(body.clone()))
        .expect("the client builds");

    // What the response to `questions` keeps: the bytes its drop frees, and
    // what the whole call allocated, for the printout.
    let kept = |label: &str, questions: &PreparedQuestions| {
        // A warm-up, so that the encode scratch and the runtime's lazily
        // built state are in place before anything is counted.
        drop(runtime.block_on(pin!(client.system_one("x", questions).send())));
        let before_call = dhat::HeapStats::get();
        let response = runtime
            .block_on(pin!(client.system_one("x", questions).send()))
            .expect("the call succeeds");
        let after_call = dhat::HeapStats::get();
        assert_eq!(
            response.answers().len(),
            2 * ANSWERS_PER_SHAPE,
            "{label}: every answer is kept"
        );
        drop(response);
        let after_drop = dhat::HeapStats::get();
        let kept = after_call.curr_bytes - after_drop.curr_bytes;
        let allocated = after_call.total_bytes - before_call.total_bytes;
        println!("{label:<32} kept={kept:>9} B allocated={allocated:>9} B");
        kept
    };

    println!("body {} B, {} answers", body.len(), 2 * ANSWERS_PER_SHAPE);
    let baseline = kept("no level hint", &unhinted());
    let asked = kept("one score of 1,000 levels", &hinted());

    #[expect(clippy::cast_precision_loss, reason = "a ratio of two byte counts, for a bound")]
    let ratio = asked as f64 / baseline as f64;
    println!("ratio {ratio:.3} (bound {MAX_RATIO})");
    assert!(
        ratio <= MAX_RATIO,
        "asking one score of {ASKED_LEVELS} levels made the same {} B response keep {asked} B, \
         {ratio:.1} times the {baseline} B it keeps without a level hint; the bound is {MAX_RATIO}",
        body.len(),
    );
}
