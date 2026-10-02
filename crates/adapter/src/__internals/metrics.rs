//! The confidence and probability-normalization entry points, for the
//! metrics parity test.

use crate::metrics;

/// The score of a score answer whose reported probabilities are
/// `probabilities`, in level order: the expected level over the rescaled
/// distribution.
#[must_use]
pub fn expected_score(probabilities: &[f64]) -> f64 {
    metrics::expected_score(probabilities)
}

/// The confidence of a score answer whose reported probabilities are
/// `probabilities`, in level order.
#[must_use]
pub fn score_confidence(probabilities: &[f64]) -> f64 {
    metrics::score_confidence(probabilities)
}

/// The confidence of a choice answer whose reported probabilities are
/// `probabilities`, in criteria order.
#[must_use]
pub fn choice_confidence(probabilities: &[f64]) -> f64 {
    metrics::choice_confidence(probabilities)
}
