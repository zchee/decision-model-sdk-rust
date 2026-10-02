//! Confidence values and the normalization of answer probabilities.
//!
//! Ported from `_utils/confidence_metrics.py` and
//! `_utils/probability_normalization.py` of system-one-adapter-python.
//!
//! The results are compared with upstream's bit for bit, so two rules of the
//! interpreter upstream records under (CPython 3.14) are copied here rather
//! than left to the obvious Rust form:
//!
//! - the built-in `sum` over floats is compensated (Neumaier), see [`sum`];
//! - `max` returns the first of equal elements, see [`first_max`].

use crate::{model::Question, response::ProbabilityDebug};

/// How far a distribution's total may be from 1 before it counts as invalid
/// (upstream's `PROBABILITY_TOLERANCE`).
pub(crate) const PROBABILITY_TOLERANCE: f64 = 1e-6;

/// The probabilities of one choice or score, as they are reported, and what
/// normalization did to them (upstream's `ProbabilityNormalization`).
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Normalization {
    /// One probability per label or level, in criteria order.
    pub(crate) probabilities: Vec<f64>,
    /// The distance of the model's total from 1; 0 for a discrete answer.
    pub(crate) error: f64,
    /// The model's probabilities, when normalization rescaled them.
    pub(crate) original: Option<Vec<f64>>,
}

/// The sum of `values` as CPython's built-in `sum` computes it for floats.
///
/// A plain left-to-right fold loses the low bits of every addition: ten times
/// `0.1` gives `0.9999999999999999`. CPython (3.12 and later) carries those
/// bits in a compensation term (Neumaier's variant of Kahan summation) and
/// adds the term at the end, but only when it is neither zero nor non-finite,
/// so that the sign of a zero total and an infinite total survive.
pub(crate) fn sum(values: impl IntoIterator<Item = f64>) -> f64 {
    let mut total = 0.0_f64;
    let mut compensation = 0.0_f64;
    for value in values {
        let next = total + value;
        compensation += if total.abs() >= value.abs() {
            (total - next) + value
        } else {
            (value - next) + total
        };
        total = next;
    }
    if compensation != 0.0 && compensation.is_finite() { total + compensation } else { total }
}

/// The position and the value of the largest of `values`, the first one among
/// equals, as Python's `max` chooses; `None` when there are none.
///
/// `Iterator::max_by` returns the last of equal elements, so it is not used.
pub(crate) fn first_max(values: impl IntoIterator<Item = f64>) -> Option<(usize, f64)> {
    values.into_iter().enumerate().reduce(|best, next| if next.1 > best.1 { next } else { best })
}

/// `probabilities` divided by their total, or the uniform distribution when
/// the total is zero (upstream's `rescale_probabilities`, and the private
/// `_normalize` of its confidence metrics, which is the same computation).
pub(crate) fn rescale(probabilities: &[f64]) -> Vec<f64> {
    let total = sum(probabilities.iter().copied());
    if total == 0.0 {
        return vec![1.0 / probabilities.len() as f64; probabilities.len()];
    }
    probabilities.iter().map(|probability| probability / total).collect()
}

/// How concentrated a score's probabilities are around the most likely
/// level: 1 when all of the weight is on it, 0 when the weight is spread as a
/// uniform distribution is, or wider.
pub(crate) fn score_confidence(probabilities: &[f64]) -> f64 {
    if probabilities.len() == 1 {
        return 1.0;
    }
    let normalized = rescale(probabilities);
    let mode = first_max(normalized.iter().copied()).map_or(0, |(index, _)| index);
    let distance_from_mode = sum(normalized
        .iter()
        .enumerate()
        .map(|(index, probability)| probability * index.abs_diff(mode) as f64));
    let levels = normalized.len() as f64;
    let uniform_center = (levels - 1.0) / 2.0;
    let uniform_mean_absolute_deviation =
        sum((0..normalized.len()).map(|index| (index as f64 - uniform_center).abs())) / levels;
    let confidence = 1.0 - distance_from_mode / uniform_mean_absolute_deviation;
    // Python's `max(0.0, confidence)`: its first argument unless the second
    // is strictly greater, which also turns a NaN into 0.
    if confidence > 0.0 { confidence } else { 0.0 }
}

/// How far the most likely label's probability is from the uniform one, on a
/// scale where uniform is 0 and certainty is 1.
///
/// Upstream does not clamp this value, and neither does this function: labels
/// of equal probability can give a result a few units in the last place below
/// zero (`[0.04; 5]` gives `-3.469446951953614e-17`).
pub(crate) fn choice_confidence(probabilities: &[f64]) -> f64 {
    if probabilities.len() == 1 {
        return 1.0;
    }
    let normalized = rescale(probabilities);
    let uniform_probability = 1.0 / normalized.len() as f64;
    let peak = first_max(normalized.iter().copied()).map_or(f64::NAN, |(_, value)| value);
    (peak - uniform_probability) / (1.0 - uniform_probability)
}

/// The expected level of a score: each level's index weighted by its
/// probability, over the rescaled distribution.
///
/// The distribution is rescaled here even when `probabilities` were already
/// normalized, as upstream does (`_client.py:141-142`): an expected value has
/// a meaning only over probabilities that sum to 1, and the reported
/// probabilities are left as the model gave them when normalization is off.
pub(crate) fn expected_score(probabilities: &[f64]) -> f64 {
    let distribution = rescale(probabilities);
    sum(distribution.iter().enumerate().map(|(level, probability)| level as f64 * probability))
}

/// The distribution of a discrete answer: 1 for the selected outcome and 0
/// for every other, never normalized and with no error.
pub(crate) fn one_hot(outcomes: usize, selected: usize) -> Normalization {
    Normalization {
        probabilities: (0..outcomes)
            .map(|outcome| if outcome == selected { 1.0 } else { 0.0 })
            .collect(),
        error: 0.0,
        original: None,
    }
}

/// The distribution of a probabilities answer: the model's values, rescaled
/// to sum to 1 when `enabled` and their total misses 1 by more than
/// [`PROBABILITY_TOLERANCE`] (upstream's
/// `normalize_probabilities_of_all_answers`).
pub(crate) fn normalize(probabilities: Vec<f64>, enabled: bool) -> Normalization {
    let error = (sum(probabilities.iter().copied()) - 1.0).abs();
    if !enabled || error <= PROBABILITY_TOLERANCE {
        return Normalization { probabilities, error, original: None };
    }
    Normalization { probabilities: rescale(&probabilities), error, original: Some(probabilities) }
}

/// What normalization found over the questions of one reply (upstream's
/// `probability_debug_data`).
///
/// A noul has no distribution and comes with `None`; it adds nothing. The
/// largest error is taken over every distribution, the discrete ones (error
/// 0) included, and is 0 when there is none.
pub(crate) fn probability_debug<'a>(
    questions: impl IntoIterator<Item = (&'a Question, Option<&'a Normalization>)>,
) -> ProbabilityDebug {
    let mut errors = Vec::new();
    let mut debug = ProbabilityDebug::default();
    for (question, normalization) in questions {
        let Some(normalization) = normalization else {
            continue;
        };
        errors.push(normalization.error);
        if normalization.error > PROBABILITY_TOLERANCE {
            debug.probability_errors.push((question.id.clone(), normalization.error));
        }
        if let Some(original) = &normalization.original {
            let labels = question.outcomes();
            debug.original_probabilities.push((
                question.id.clone(),
                labels
                    .iter()
                    .zip(original)
                    .map(|((label, _), probability)| (label.to_string(), *probability))
                    .collect(),
            ));
        }
    }
    debug.max_error = first_max(errors).map_or(0.0, |(_, error)| error);
    debug.invalid_probs = debug.probability_errors.len();
    debug
}

#[cfg(test)]
#[path = "metrics_tests.rs"]
mod tests;
