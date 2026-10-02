//! Tests for the confidence values and the probability normalization.
//!
//! The expected bit patterns were computed with upstream's own functions on
//! CPython 3.14, whose built-in `sum` is compensated; "plain" below is a
//! left-to-right fold, which is what the port must not be.

use proptest::prelude::*;

use super::*;
use crate::model::QuestionModel;

/// `pytest.approx` with its default tolerances: relative 1e-6, absolute
/// 1e-12, whichever is larger.
#[track_caller]
fn assert_approx(actual: f64, expected: f64, case: &str) {
    let tolerance = (1e-6 * expected.abs()).max(1e-12);
    assert!(
        (actual - expected).abs() <= tolerance,
        "{case}: got {actual:?}, expected {expected:?} within {tolerance:e}"
    );
}

/// A float as the hexadecimal text of its bits, so a failure shows the last
/// place.
#[track_caller]
fn assert_bits(actual: f64, expected: u64, case: &str) {
    assert_eq!(
        actual.to_bits(),
        expected,
        "{case}: got {actual:?} ({:#018X}), expected {:?} ({expected:#018X})",
        actual.to_bits(),
        f64::from_bits(expected)
    );
}

fn plain_sum(values: impl IntoIterator<Item = f64>) -> f64 {
    values.into_iter().fold(0.0, |total, value| total + value)
}

fn bits(values: &[f64]) -> Vec<u64> {
    values.iter().map(|value| value.to_bits()).collect()
}

/// One noul, one score of two levels and one choice of two labels, the
/// questions of upstream's normalization test.
fn questions() -> QuestionModel {
    QuestionModel::from_json(
        r#"{
            "positive": {"type": "noul"},
            "stars": {"type": "score", "criteria": ["low", "high"]},
            "genre": {"type": "choice", "criteria": {"fiction": null, "nonfiction": null}}
        }"#,
    )
    .expect("the questions are valid")
}

#[test]
// Upstream: tests/utils/test_confidence_metrics.py::test_confidence_metrics
fn metrics_upstream_confidence() {
    type Metric = fn(&[f64]) -> f64;
    let cases: [(&str, Metric, &[f64], f64); 8] = [
        ("score, uniform", score_confidence, &[0.2; 5], 0.0),
        ("score, uniform and not summing to 1", score_confidence, &[0.04; 5], 0.0),
        ("score, skewed", score_confidence, &[0.01, 0.02, 0.07, 0.3, 0.6], 0.55),
        ("choice, uniform", choice_confidence, &[0.5, 0.5], 0.0),
        ("choice, uniform and not summing to 1", choice_confidence, &[0.2, 0.2], 0.0),
        ("choice, skewed", choice_confidence, &[0.82, 0.18], 0.64),
        ("score, one level", score_confidence, &[1.0], 1.0),
        ("choice, one label", choice_confidence, &[1.0], 1.0),
    ];
    for (case, metric, probabilities, expected) in cases {
        assert_approx(metric(probabilities), expected, case);
    }
}

#[test]
// Upstream: tests/utils/test_probability_normalization.py::test_probability_normalization_and_debug_data
fn metrics_upstream_normalization() {
    struct Case {
        name: &'static str,
        enabled: bool,
        raw: f64,
        expected: f64,
        expected_originals: Vec<(&'static str, Vec<(&'static str, f64)>)>,
        expected_max_error: f64,
        expected_errors: Vec<(&'static str, f64)>,
    }
    let cases = [
        Case {
            name: "normalization off",
            enabled: false,
            raw: 0.2,
            expected: 0.2,
            expected_originals: vec![],
            expected_max_error: 0.6,
            expected_errors: vec![("stars", 0.6), ("genre", 0.6)],
        },
        Case {
            name: "normalization on",
            enabled: true,
            raw: 0.2,
            expected: 0.5,
            expected_originals: vec![
                ("stars", vec![("0", 0.2), ("1", 0.2)]),
                ("genre", vec![("fiction", 0.2), ("nonfiction", 0.2)]),
            ],
            expected_max_error: 0.6,
            expected_errors: vec![("stars", 0.6), ("genre", 0.6)],
        },
        Case {
            name: "within the tolerance",
            enabled: false,
            raw: 0.500_000_25,
            expected: 0.500_000_25,
            expected_originals: vec![],
            expected_max_error: 5e-7,
            expected_errors: vec![],
        },
    ];
    let model = questions();
    for case in cases {
        let name = case.name;
        let normalizations = [
            None,
            Some(normalize(vec![case.raw, case.raw], case.enabled)),
            Some(normalize(vec![case.raw, case.raw], case.enabled)),
        ];
        for normalization in normalizations.iter().flatten() {
            assert_eq!(normalization.probabilities.len(), 2, "{name}");
            for probability in &normalization.probabilities {
                assert_approx(*probability, case.expected, name);
            }
        }

        let debug = probability_debug(
            model.questions.iter().zip(normalizations.iter().map(Option::as_ref)),
        );
        assert_approx(debug.max_error, case.expected_max_error, name);
        assert_eq!(debug.invalid_probs, case.expected_errors.len(), "{name}");
        assert_eq!(
            debug.probability_errors.iter().map(|(id, _)| id.as_str()).collect::<Vec<_>>(),
            case.expected_errors.iter().map(|(id, _)| *id).collect::<Vec<_>>(),
            "{name}"
        );
        for ((_, error), (_, expected)) in
            debug.probability_errors.iter().zip(&case.expected_errors)
        {
            assert_approx(*error, *expected, name);
        }
        let originals = debug
            .original_probabilities
            .iter()
            .map(|(id, labels)| {
                (
                    id.as_str(),
                    labels
                        .iter()
                        .map(|(label, probability)| (label.as_str(), *probability))
                        .collect::<Vec<_>>(),
                )
            })
            .collect::<Vec<_>>();
        assert_eq!(originals, case.expected_originals, "{name}");
    }
}

#[test]
fn metrics_tie_mode() {
    // Levels 0 and 1 are equally likely. Upstream's mode is the first of
    // them; taking the last gives 0.09999999999999987.
    assert_bits(score_confidence(&[0.4, 0.4, 0.2]), 0.0_f64.to_bits(), "the first maximum");
    assert_eq!(first_max([0.4, 0.4, 0.2]), Some((0, 0.4)));
}

#[test]
fn metrics_to_bits_tenths() {
    let tenths = [0.1_f64; 10];

    assert_bits(sum(tenths), 0x3FF0_0000_0000_0000, "total");
    assert_bits(expected_score(&tenths), 0x4012_0000_0000_0000, "score");
    assert_bits(normalize(tenths.to_vec(), false).error, 0.0_f64.to_bits(), "error");

    // The same three values from a plain sum, which is why the case exists.
    let plain_total = plain_sum(tenths);
    assert_bits(plain_total, 0x3FEF_FFFF_FFFF_FFFF, "plain total");
    let plain_score = plain_sum(
        tenths.iter().enumerate().map(|(level, value)| level as f64 * (value / plain_total)),
    );
    assert_bits(plain_score, 0x4012_0000_0000_0001, "plain score");
    assert_bits((plain_total - 1.0).abs(), 2.0_f64.powi(-53).to_bits(), "plain error");
}

#[test]
fn metrics_to_bits_mixed() {
    let mixed = [0.3, 0.3, 0.3, 0.1];

    assert_bits(expected_score(&mixed), 0x3FF3_3333_3333_3333, "score");
    assert_bits(choice_confidence(&mixed), 0x3FB1_1111_1111_1110, "choice confidence");
    // The distance from the mode is the fifth compensated sum; a plain one
    // gives 0x3FC555555555554C here.
    assert_bits(
        score_confidence(&[0.6, 0.1, 0.1, 0.1, 0.1]),
        0x3FC5_5555_5555_5554,
        "score confidence",
    );

    // A plain total (0.9999999999999999) moves both values off upstream's.
    let plain_total = plain_sum(mixed);
    let plain_score = plain_sum(
        mixed.iter().enumerate().map(|(level, value)| level as f64 * (value / plain_total)),
    );
    assert_bits(plain_score, 0x3FF3_3333_3333_3334, "plain score");
    let plain_confidence = (0.3 / plain_total - 0.25) / (1.0 - 0.25);
    assert_bits(plain_confidence, 0x3FB1_1111_1111_1115, "plain choice confidence");
}

#[test]
fn metrics_single_label() {
    // One outcome is certain whatever number the model attached to it.
    for probability in [1.0, 0.3, 0.0] {
        assert_bits(score_confidence(&[probability]), 1.0_f64.to_bits(), "score");
        assert_bits(choice_confidence(&[probability]), 1.0_f64.to_bits(), "choice");
    }
}

#[test]
fn choice_confidence_is_not_clamped_below_zero() {
    // Upstream's value for five equal labels that do not sum to 1: the
    // rescaled 0.04 / 0.2 lands one unit below 1 / 5.
    assert_bits(
        choice_confidence(&[0.04; 5]),
        (-3.469_446_951_953_614e-17_f64).to_bits(),
        "choice",
    );
    assert_bits(score_confidence(&[0.04; 5]), 0.0_f64.to_bits(), "score");
}

#[test]
fn zero_total_rescales_to_uniform() {
    assert_eq!(rescale(&[0.0, 0.0, 0.0, 0.0]), vec![0.25; 4]);
    assert_bits(expected_score(&[0.0, 0.0, 0.0, 0.0]), 1.5_f64.to_bits(), "score");
    assert_bits(score_confidence(&[0.0, 0.0, 0.0]), 0.0_f64.to_bits(), "score confidence");
    assert_bits(choice_confidence(&[0.0, 0.0]), 0.0_f64.to_bits(), "choice confidence");
}

#[test]
fn sum_adds_the_compensation_only_when_it_is_finite_and_not_zero() {
    assert_bits(sum([]), 0.0_f64.to_bits(), "nothing");
    // Without the compensation the 1.0 is lost between the two large terms.
    assert_bits(sum([1e100, 1.0, -1e100]), 1.0_f64.to_bits(), "cancellation");
    // An overflowing total stays infinite; adding the compensation (itself
    // infinite here) would make it a NaN.
    assert_bits(sum([f64::MAX, f64::MAX]), f64::INFINITY.to_bits(), "overflow");
    assert!(sum([f64::INFINITY, f64::NEG_INFINITY]).is_nan());
}

#[test]
fn first_max_keeps_the_earliest_of_equal_values() {
    assert_eq!(first_max([]), None);
    assert_eq!(first_max([0.5, 0.5]), Some((0, 0.5)));
    assert_eq!(first_max([0.1, 0.7, 0.7, 0.2]), Some((1, 0.7)));
    assert_eq!(first_max([0.1, 0.2, 0.9]), Some((2, 0.9)));
}

#[test]
fn one_hot_marks_the_selected_outcome_only() {
    assert_eq!(
        one_hot(3, 1),
        Normalization { probabilities: vec![0.0, 1.0, 0.0], error: 0.0, original: None }
    );
}

#[test]
fn normalization_rescales_only_when_enabled_and_over_the_tolerance() {
    let off = normalize(vec![0.2, 0.6], false);
    assert_eq!(bits(&off.probabilities), bits(&[0.2, 0.6]));
    assert_eq!(off.original, None);
    assert_approx(off.error, 0.2, "off");

    let on = normalize(vec![0.2, 0.6], true);
    assert_eq!(bits(&on.probabilities), bits(&[0.2 / 0.8, 0.6 / 0.8]));
    assert_eq!(on.original, Some(vec![0.2, 0.6]));
    assert_approx(on.error, 0.2, "on");
}

#[test]
fn debug_data_takes_the_largest_error_over_every_distribution() {
    let model = questions();
    let normalizations = [None, Some(one_hot(2, 1)), Some(normalize(vec![0.5, 0.500_000_5], true))];

    let debug =
        probability_debug(model.questions.iter().zip(normalizations.iter().map(Option::as_ref)));

    // 5e-7 is within the tolerance: it is the largest error and no invalid
    // distribution, and nothing was rescaled.
    assert_approx(debug.max_error, 5e-7, "max_error");
    assert_eq!(debug.invalid_probs, 0);
    assert!(debug.probability_errors.is_empty());
    assert!(debug.original_probabilities.is_empty());

    let nouls_only = probability_debug(model.questions.iter().take(1).map(|noul| (noul, None)));
    assert_eq!(nouls_only, ProbabilityDebug::default());
}

/// One to eight probabilities, each in `[0, 1]`, with no rule on their sum.
fn any_distribution() -> impl Strategy<Value = Vec<f64>> {
    proptest::collection::vec(0.0_f64..=1.0, 1..=8)
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(1024))]

    /// The score confidence is clamped into `[0, 1]`. The choice confidence
    /// is at most 1, and at least 0 up to the rounding of the division by the
    /// total, which upstream does not clamp away (see
    /// `choice_confidence_is_not_clamped_below_zero`).
    #[test]
    fn metrics_prop_bounds(probabilities in any_distribution()) {
        let score = score_confidence(&probabilities);
        prop_assert!((0.0..=1.0).contains(&score), "score confidence {score:?}");
        let choice = choice_confidence(&probabilities);
        prop_assert!((-1e-12..=1.0).contains(&choice), "choice confidence {choice:?}");
    }

    #[test]
    fn metrics_prop_rescale(probabilities in any_distribution()) {
        let rescaled = rescale(&probabilities);
        prop_assert_eq!(rescaled.len(), probabilities.len());
        let total = sum(rescaled.iter().copied());
        prop_assert!((total - 1.0).abs() <= 1e-12, "total {total:?} of {rescaled:?}");
    }

    #[test]
    fn metrics_prop_within_tolerance(
        weights in proptest::collection::vec(0.01_f64..=1.0, 2..=8),
        drift in -9e-7_f64..=9e-7,
    ) {
        let mut probabilities = rescale(&weights);
        probabilities[0] += drift;
        let error = (sum(probabilities.iter().copied()) - 1.0).abs();
        prop_assume!(error <= PROBABILITY_TOLERANCE);

        let normalization = normalize(probabilities.clone(), true);

        prop_assert_eq!(bits(&normalization.probabilities), bits(&probabilities));
        prop_assert_eq!(normalization.error.to_bits(), error.to_bits());
        prop_assert_eq!(normalization.original, None);
    }
}
