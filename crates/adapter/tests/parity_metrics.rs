//! The confidence and normalization values compared with the Python
//! adapter's.
//!
//! Each of the 12 recorded vendor responses holds, per score and per choice,
//! the probabilities upstream reported and the `score` and `confidence` it
//! computed from them (`_client.py:141-145,162`). This test recomputes those
//! values from the file's own probabilities and requires the same bits.
//!
//! On these files a plain left-to-right sum gives the same bits as the
//! compensated one, so the test proves the float parity only together with
//! the `to_bits` unit cases of `src/metrics_tests.rs`; the cases that
//! separate the two sums are repeated at the end.

use std::{fmt, fs, path::Path};

use serde::{
    Deserialize, Deserializer,
    de::{MapAccess, Visitor},
};
use system_one_adapter::__internals::metrics::{
    choice_confidence, expected_score, score_confidence,
};

/// The file-name prefix of the expected responses the adapter produced from a
/// vendor's reply; the directory also holds the TypeSafe API's own response.
const VENDOR_PREFIX: &str = "test_live_responses_match_reference_shape[";

#[derive(Deserialize)]
struct Expected {
    answers: Pairs<Answer>,
}

/// One answer, as a plain struct and not as an enum tagged by `type`: serde
/// buffers the content of an internally tagged enum, and a buffered number is
/// a map when serde_json's `arbitrary_precision` feature is on. Here every
/// `f64` is read from the JSON text by `deserialize_f64`, which is the same
/// code with and without that feature.
#[derive(Deserialize)]
struct Answer {
    #[serde(rename = "type")]
    kind: Kind,
    score: Option<f64>,
    confidence: Option<f64>,
    probabilities: Option<Pairs<f64>>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "lowercase")]
enum Kind {
    Noul,
    Score,
    Choice,
}

impl Answer {
    /// The value of a member that an answer of this kind always has.
    #[track_caller]
    fn required<'a, T>(&self, member: &'a Option<T>, what: &str, context: &str) -> &'a T {
        member
            .as_ref()
            .unwrap_or_else(|| panic!("{context}: a {:?} answer has no `{what}`", self.kind))
    }
}

/// The members of a JSON object in document order, which is the order
/// upstream wrote them in: level order for a score, criteria order for a
/// choice.
struct Pairs<T>(Vec<(String, T)>);

impl<'de, T: Deserialize<'de>> Deserialize<'de> for Pairs<T> {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct PairsVisitor<T>(std::marker::PhantomData<T>);

        impl<'de, T: Deserialize<'de>> Visitor<'de> for PairsVisitor<T> {
            type Value = Pairs<T>;

            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("a JSON object")
            }

            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Pairs<T>, A::Error> {
                let mut pairs = Vec::new();
                while let Some(pair) = map.next_entry::<String, T>()? {
                    pairs.push(pair);
                }
                Ok(Pairs(pairs))
            }
        }

        deserializer.deserialize_map(PairsVisitor(std::marker::PhantomData))
    }
}

impl Pairs<f64> {
    fn values(&self) -> Vec<f64> {
        self.0.iter().map(|(_, value)| *value).collect()
    }
}

#[track_caller]
fn assert_same_bits(recomputed: f64, recorded: f64, what: &str) {
    assert_eq!(
        recomputed.to_bits(),
        recorded.to_bits(),
        "{what}: recomputed {recomputed:?} ({:#018X}), recorded {recorded:?} ({:#018X})",
        recomputed.to_bits(),
        recorded.to_bits()
    );
}

#[test]
fn parity_metrics_expected_responses() {
    let directory = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/expected_responses");
    let mut files = fs::read_dir(&directory)
        .expect("the expected responses are in the repository")
        .map(|entry| entry.expect("a directory entry").path())
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with(VENDOR_PREFIX) && name.ends_with(".json"))
        })
        .collect::<Vec<_>>();
    files.sort();
    assert_eq!(files.len(), 12, "the vendor expected responses: {files:?}");

    let (mut scores, mut choices) = (0, 0);
    for file in &files {
        let name = file.display();
        let text = fs::read_to_string(file).expect("an expected response is UTF-8 text");
        let expected: Expected = serde_json::from_str(&text)
            .unwrap_or_else(|error| panic!("{name}: not an expected response: {error}"));
        for (id, answer) in &expected.answers.0 {
            let context = format!("{name}: {id}");
            match answer.kind {
                Kind::Noul => {}
                Kind::Score => {
                    let score = answer.required(&answer.score, "score", &context);
                    let confidence = answer.required(&answer.confidence, "confidence", &context);
                    let probabilities =
                        answer.required(&answer.probabilities, "probabilities", &context);
                    let levels = probabilities
                        .0
                        .iter()
                        .map(|(level, _)| level.parse::<usize>().ok())
                        .collect::<Vec<_>>();
                    let in_order = (0..levels.len()).map(Some).collect::<Vec<_>>();
                    assert_eq!(levels, in_order, "{name}: {id}: the levels are 0, 1, ...");
                    let probabilities = probabilities.values();
                    assert_same_bits(
                        expected_score(&probabilities),
                        *score,
                        &format!("{name}: {id}: score"),
                    );
                    assert_same_bits(
                        score_confidence(&probabilities),
                        *confidence,
                        &format!("{name}: {id}: score confidence"),
                    );
                    scores += 1;
                }
                Kind::Choice => {
                    let confidence = answer.required(&answer.confidence, "confidence", &context);
                    let probabilities =
                        answer.required(&answer.probabilities, "probabilities", &context);
                    assert_same_bits(
                        choice_confidence(&probabilities.values()),
                        *confidence,
                        &format!("{name}: {id}: choice confidence"),
                    );
                    choices += 1;
                }
            }
        }
    }
    // One score and one choice per file: 12 scores and 24 confidences.
    assert_eq!((scores, choices), (12, 12), "the answers that were compared");

    // The cases a plain sum gets wrong, computed with upstream's functions on
    // CPython 3.14 (the plain results are in `src/metrics_tests.rs`).
    let separating: [(&str, f64, u64); 4] = [
        ("score of ten tenths", expected_score(&[0.1; 10]), 0x4012_0000_0000_0000),
        ("score of the mixed case", expected_score(&[0.3, 0.3, 0.3, 0.1]), 0x3FF3_3333_3333_3333),
        (
            "choice confidence of the mixed case",
            choice_confidence(&[0.3, 0.3, 0.3, 0.1]),
            0x3FB1_1111_1111_1110,
        ),
        (
            "score confidence around a mode",
            score_confidence(&[0.6, 0.1, 0.1, 0.1, 0.1]),
            0x3FC5_5555_5555_5554,
        ),
    ];
    for (what, recomputed, recorded) in separating {
        assert_same_bits(recomputed, f64::from_bits(recorded), what);
    }
}
