//! The response types of the naive comparator: what the `sdk` bench decodes
//! and what the decode allocation budget's ratio to naive is measured
//! against.

use std::collections::HashMap;

use serde::Deserialize;

/// A decoded response.
#[expect(dead_code, reason = "decoded to be measured; at most the answer count is read")]
#[derive(Debug, Deserialize)]
pub(crate) struct NaiveResponse {
    model: String,
    usage: NaiveUsage,
    pub(crate) answers: HashMap<String, NaiveAnswer>,
}

/// Token counts.
#[expect(dead_code, reason = "decoded to be measured")]
#[derive(Debug, Deserialize)]
pub(crate) struct NaiveUsage {
    input_tokens: Option<u64>,
    output_tokens: Option<u64>,
}

/// One answer, tagged by its `type` member.
#[expect(dead_code, reason = "decoded to be measured")]
#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub(crate) enum NaiveAnswer {
    Noul {
        noul: f64,
    },
    Choice {
        choice: String,
        confidence: f64,
        probabilities: HashMap<String, f64>,
    },
    Score {
        score: f64,
        confidence: f64,
        legend: HashMap<String, String>,
        probabilities: HashMap<String, f64>,
    },
}
