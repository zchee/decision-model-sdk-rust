//! How the model is asked to answer: through its native structured-output
//! mode or a prompted JSON reply, and as probabilities or discrete answers.
//!
//! Ported from `_client.py` of system-one-adapter-python.

/// How the model is made to answer in the shape of the output schema.
///
/// Upstream's `structured_outputs=True` is [`Native`](Self::Native) and
/// `structured_outputs=False` is [`Prompted`](Self::Prompted); the two words
/// are upstream's own test ids.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum StructuredOutputs {
    /// The provider's own structured-output mode: the schema travels in the
    /// request and the vendor constrains the reply to it.
    Native,
    /// A plain text request: the schema is written into the system prompt,
    /// and the reply is read as JSON, with a Markdown code fence around it
    /// removed.
    Prompted,
}

/// What the model is asked to answer each question with.
///
/// Upstream's `llm_answer_mode`. A word other than these two is a
/// `ValueError` upstream; here it cannot be written.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AnswerMode {
    /// A probability per outcome: one number for a noul, one per label of a
    /// choice and one per level of a score.
    Probabilities,
    /// One outcome per question: `true` or `false` for a noul, a label for a
    /// choice and a level index for a score.
    Discrete,
}
