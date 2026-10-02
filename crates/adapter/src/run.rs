//! One evaluation: the corrective loop over malformed replies, the usage
//! totals, the trace, and the response or error it ends with.
//!
//! Ported from `_client.py` of system-one-adapter-python.

use std::{
    error::Error as StdError,
    fmt,
    sync::{Mutex, PoisonError},
    time::Instant,
};

use http::Uri;
use typesafe_sdk::{Answers, Error as SdkError, RetryPolicy};

use crate::{
    convert::{Converted, convert},
    decode::decode,
    error::{Error, ErrorKind, provider_kind_name},
    model::QuestionModel,
    options::{AnswerMode, StructuredOutputs},
    prompt::{self, Problem},
    provider::{AttemptTrace, Message, NonAnswer, Provider, ProviderCall, ProviderResult, Role},
    response::{Attempt, Response, RetryCategory, RetryReason, Trace, Usage},
    retry::run_with_retries,
    schema,
};

/// The target of the adapter's own events.
#[cfg(feature = "tracing")]
const TARGET: &str = "system_one_adapter";

/// What one provider call ends with: a reply, a reply that is not an answer,
/// or a failure of the exchange.
type Exchange = Result<Result<ProviderResult, NonAnswer>, SdkError>;

/// Everything one evaluation is made from, after the provider is resolved,
/// the state is written and the questions are re-validated.
pub(crate) struct Evaluation<'a> {
    pub(crate) provider: &'a dyn Provider,
    pub(crate) questions: &'a QuestionModel,
    pub(crate) structured_outputs: StructuredOutputs,
    pub(crate) answer_mode: AnswerMode,
    pub(crate) normalize_probabilities: bool,
    /// The most corrective retries after a reply that does not match the
    /// schema.
    pub(crate) n_retry_malformed_structure: u32,
    pub(crate) retry: &'a RetryPolicy,
    /// The document, as [`prompt::user_message`] wrote it.
    pub(crate) user_message: String,
    /// When the latency clock started.
    pub(crate) started: Instant,
}

/// Asks the provider until a reply matches the schema, and builds the
/// response from it (upstream's `_EvaluationRun.run_async` and `response`).
///
/// Each corrective turn runs the retry policy once. The log URI of the
/// provider is read here, inside the future that holds the provider, and goes
/// to the retry policy only.
///
/// # Errors
///
/// Returns the provider's failure once the retry policy gives up, a
/// non-answer after the one call that produced it, or a malformed-structure
/// error when the reply still does not match after the last corrective retry.
/// Each carries the trace of the attempts made.
pub(crate) async fn evaluate(evaluation: Evaluation<'_>) -> Result<Response<Answers>, Error> {
    let Evaluation {
        provider,
        questions,
        structured_outputs,
        answer_mode,
        normalize_probabilities,
        n_retry_malformed_structure,
        retry,
        user_message,
        started,
    } = evaluation;

    let schema = schema::write(questions, answer_mode);
    let structured = matches!(structured_outputs, StructuredOutputs::Native);
    let system = match answer_mode {
        AnswerMode::Probabilities => prompt::PROBABILITY_SYSTEM_PROMPT,
        AnswerMode::Discrete => prompt::DISCRETE_SYSTEM_PROMPT,
    };
    let system = if structured {
        system.to_owned()
    } else {
        prompt::with_schema_instruction(system, schema.as_str())
    };
    let mut messages =
        vec![Message::new(Role::System, system), Message::new(Role::User, user_message)];

    let root = Uri::from_static("/");
    let uri = provider.log_uri().unwrap_or(&root);
    let attempts = Mutex::new(Vec::new());
    let mut retry_reasons = Vec::new();
    let mut usage = Usage {
        input_tokens: None,
        output_tokens: None,
        input_tokens_total: Some(0),
        output_tokens_total: Some(0),
        n_retries: 0,
        n_retries_malformed_structure: 0,
        latency: started.elapsed(),
    };

    let outcome: Result<Converted, Error> = async {
        loop {
            let (reply, retries) = run_with_retries(retry, uri, &mut retry_reasons, || {
                attempt(provider, &messages, &schema, structured, &attempts)
            })
            .await
            .map_err(Error::provider)?;
            let reply = reply.map_err(Error::non_answer)?;

            usage.n_retries = usage.n_retries.saturating_add(retries);
            usage.input_tokens = reply.input_tokens();
            usage.output_tokens = reply.output_tokens();
            usage.input_tokens_total = add(usage.input_tokens_total, reply.input_tokens());
            usage.output_tokens_total = add(usage.output_tokens_total, reply.output_tokens());

            match decode(questions, answer_mode, prompt::extract_json(reply.text())) {
                Ok(decoded) => break convert(questions, decoded, normalize_probabilities),
                Err(problems) => {
                    let message = prompt::validation_message(&problems);
                    if usage.n_retries_malformed_structure == n_retry_malformed_structure {
                        break Err(Error::malformed_structure(message)
                            .with_source(Box::new(InvalidReply(problems))));
                    }
                    usage.n_retries_malformed_structure += 1;
                    // The model sees its own reply as it wrote it, fence
                    // included, and then what was wrong with it.
                    messages.push(Message::new(Role::Assistant, reply.text()));
                    messages.push(Message::new(Role::User, prompt::correction_prompt(&message)));
                    retry_reasons
                        .push(RetryReason::new(RetryCategory::MalformedStructure, message));
                }
            }
        }
    }
    .await;

    let attempts = attempts.into_inner().unwrap_or_else(PoisonError::into_inner);
    match outcome {
        Ok(Converted { answers, probabilities }) => {
            usage.latency = started.elapsed();
            Ok(Response {
                model: provider.model_name().to_owned(),
                usage,
                answers,
                n_answers: questions.questions.len(),
                debug: Trace { attempts, retry_reasons, probabilities: Some(probabilities) },
            })
        }
        Err(error) => Err(error.with_trace(Trace { attempts, retry_reasons, probabilities: None })),
    }
}

/// One provider call, recorded: the finished [`Attempt`] is handed to
/// `attempts` before this returns, whether the call answered or failed
/// (upstream's `capture_attempt`).
///
/// The future owns the [`AttemptTrace`] the provider writes into, because the
/// retry policy's closure cannot lend one to the futures it returns.
async fn attempt(
    provider: &dyn Provider,
    messages: &[Message],
    schema: &crate::provider::Schema,
    structured: bool,
    attempts: &Mutex<Vec<Attempt>>,
) -> Exchange {
    let started = Instant::now();
    let mut trace = AttemptTrace::default();
    let exchange =
        provider.request(ProviderCall::new(messages, schema, structured, &mut trace)).await;

    let mut attempt = Attempt::new(
        messages.to_vec(),
        schema.clone(),
        structured,
        provider.model_name().to_owned(),
        provider.type_name().to_owned(),
    );
    attempt.record(trace);
    let failure = match &exchange {
        Ok(Ok(reply)) => {
            attempt.record_result(reply);
            None
        }
        Ok(Err(non_answer)) => {
            // The adapter's own kind name, where a failed exchange has the SDK's.
            let kind = ErrorKind::NonAnswer(non_answer.clone()).name();
            attempt.record_error(non_answer.to_string(), kind);
            Some(kind)
        }
        Err(error) => {
            let kind = provider_kind_name(error);
            attempt.record_error(error.to_string(), kind);
            Some(kind)
        }
    };
    let number = {
        let mut attempts = attempts.lock().unwrap_or_else(PoisonError::into_inner);
        attempts.push(attempt);
        attempts.len()
    };
    attempt_event(
        number,
        started,
        exchange.as_ref().ok().and_then(|reply| reply.as_ref().ok()),
        failure,
    );
    exchange
}

/// One event per finished attempt: its number within the call, how long it
/// took, the token counts of a reply, and the failure's kind name. Nothing of
/// the request or the reply, and no endpoint: the transport's own event names
/// that.
#[cfg(feature = "tracing")]
fn attempt_event(
    number: usize,
    started: Instant,
    reply: Option<&ProviderResult>,
    failure: Option<&'static str>,
) {
    tracing::debug!(
        target: TARGET,
        attempt = number,
        elapsed_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
        input_tokens = reply.and_then(ProviderResult::input_tokens),
        output_tokens = reply.and_then(ProviderResult::output_tokens),
        error = failure,
    );
}

/// One event per finished attempt: nothing, without the `tracing` feature.
#[cfg(not(feature = "tracing"))]
fn attempt_event(_: usize, _: Instant, _: Option<&ProviderResult>, _: Option<&'static str>) {}

/// A running total with one more count: unknown once either is, as upstream
/// keeps `None`, and unknown rather than wrong if it would overflow.
fn add(total: Option<u64>, count: Option<u64>) -> Option<u64> {
    total?.checked_add(count?)
}

/// The decode failure under a malformed-structure error: the problems of the
/// last reply. `Display` is the validation message; `Debug` is their number,
/// because the problems name the caller's questions and labels.
struct InvalidReply(Vec<Problem>);

impl fmt::Display for InvalidReply {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&prompt::validation_message(&self.0))
    }
}

impl fmt::Debug for InvalidReply {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.debug_struct("InvalidReply").field("problems", &self.0.len()).finish()
    }
}

impl StdError for InvalidReply {}

#[cfg(test)]
#[path = "run_tests.rs"]
mod tests;
