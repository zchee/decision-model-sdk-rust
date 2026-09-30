//! B2: the response decode.
//!
//! `answers` decodes into `SystemOneResponse<Answers>`, the lookup by name;
//! `typed` decodes the same bytes into a `#[derive(QuestionSet)]` struct.
//! Both go through the response decoder every call uses, depth pre-scan
//! included, but without a level hint: a call passes the largest score its
//! questions ask (at most 8), which starts a score's first level list at
//! that capacity, while this entry point passes none, so a list starts at 4
//! and a five-level score grows once more than it would in a call. The
//! 20-answer document cycles through a noul, a choice of four options and a
//! score of five levels.
//!
//! How this can mislead: the body is handed in as a `Bytes` that is already
//! whole, so reading it off a connection is not part of the number; B5 and
//! B6 include it. The decoded response is dropped outside the timed section.
//!
//! `codec` decodes the naive comparator's types - serde's derive, answers
//! tagged by `type` in a `HashMap` - with sonic-rs and with serde_json, from
//! the same bytes. The SDK's own visitors cannot be pointed at serde_json
//! (`codec.rs` is the only module that names a codec), so this is the like
//! for like comparison available: the same serde work through each parser.

use std::sync::LazyLock;

use bytes::Bytes;
use divan::{Bencher, black_box};
use http::{HeaderMap, StatusCode};
use typesafe_sdk::{
    __internals as sdk, Answers, ChoiceAnswer, NoulAnswer, QuestionSet, ScoreAnswer,
    SystemOneResponse,
};

use crate::support::RESULT;

/// A 20-answer response, built once and kept for the process so that it can
/// be handed out as a `Bytes` that costs nothing to create.
pub(crate) static TWENTY: LazyLock<Vec<u8>> = LazyLock::new(|| response_of(20));

/// The questions `RESULT` answers, as a struct.
#[derive(Debug, QuestionSet)]
struct Review {
    #[noul(instructions = "Spam?")]
    spam: NoulAnswer,
    #[choice(instructions = "Tone?", options("friendly", "hostile"))]
    tone: ChoiceAnswer,
    #[score(instructions = "Quality?", levels("bad", "ok", "great"))]
    quality: ScoreAnswer,
}

/// The response body with `answers` answers: the fixture for 3, the
/// generated document for 20.
fn document(answers: usize) -> &'static [u8] {
    match answers {
        3 => RESULT,
        20 => TWENTY.as_slice(),
        other => panic!("no document of {other} answers"),
    }
}

/// A response of `answers` answers, cycling through a noul, a choice of four
/// options and a score of five levels, named `q0`, `q1`, ...
fn response_of(answers: usize) -> Vec<u8> {
    const NOUL: &str = r#"{"type":"noul","noul":0.9731}"#;
    const CHOICE: &str = r#"{"type":"choice","choice":"billing","confidence":0.82,"probabilities":{"billing":0.82,"shipping":0.1,"account":0.05,"other":0.03}}"#;
    const SCORE: &str = r#"{"type":"score","score":3.4,"confidence":0.71,"legend":{"0":"none","1":"low","2":"medium","3":"high","4":"critical"},"probabilities":{"0":0.01,"1":0.04,"2":0.14,"3":0.71,"4":0.1}}"#;
    let mut json = Vec::from(
        &br#"{"model":"jev-latest","usage":{"input_tokens":812,"output_tokens":61},"answers":{"#[..],
    );
    for index in 0..answers {
        if index > 0 {
            json.push(b',');
        }
        let answer = [NOUL, CHOICE, SCORE][index % 3];
        json.extend_from_slice(format!(r#""q{index}":{answer}"#).as_bytes());
    }
    json.extend_from_slice(b"}}");
    json
}

fn decode_answers(body: &'static [u8], questions: usize) -> SystemOneResponse<Answers> {
    sdk::decode_system_one(Bytes::from_static(body), StatusCode::OK, HeaderMap::new(), questions)
        .expect("the document decodes")
}

#[divan::bench(args = [3, 20])]
fn answers(bencher: Bencher<'_, '_>, answers: usize) {
    let body = document(answers);
    assert_eq!(decode_answers(body, answers).answers().len(), answers);
    bencher.bench_local(|| decode_answers(black_box(body), answers));
}

#[divan::bench]
fn typed(bencher: Bencher<'_, '_>) {
    let decode = || -> SystemOneResponse<Review> {
        sdk::decode_system_one(
            Bytes::from_static(black_box(RESULT)),
            StatusCode::OK,
            HeaderMap::new(),
            Review::prepared().len(),
        )
        .expect("the fixture decodes as a Review")
    };
    let review = decode();
    let lookup = decode_answers(RESULT, 3);
    assert_eq!(Some(&review.answers().spam), lookup.answers().noul("spam"));
    assert_eq!(Some(&review.answers().tone), lookup.answers().choice("tone"));
    assert_eq!(Some(&review.answers().quality), lookup.answers().score("quality"));
    bencher.bench_local(decode);
}

mod codec {
    use divan::{Bencher, black_box};

    use super::document;
    use crate::naive_response::NaiveResponse;

    #[divan::bench(args = [3, 20])]
    fn sonic_rs(bencher: Bencher<'_, '_>, answers: usize) {
        let body = document(answers);
        let decoded: NaiveResponse = sonic_rs::from_slice(body).expect("the document decodes");
        assert_eq!(decoded.answers.len(), answers);
        bencher.bench_local(|| -> NaiveResponse {
            sonic_rs::from_slice(black_box(body)).expect("the document decodes")
        });
    }

    #[divan::bench(args = [3, 20])]
    fn serde_json(bencher: Bencher<'_, '_>, answers: usize) {
        let body = document(answers);
        let decoded: NaiveResponse = serde_json::from_slice(body).expect("the document decodes");
        assert_eq!(decoded.answers.len(), answers);
        bencher.bench_local(|| -> NaiveResponse {
            serde_json::from_slice(black_box(body)).expect("the document decodes")
        });
    }
}
