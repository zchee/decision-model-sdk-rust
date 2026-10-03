//! The retry of failed provider calls under the SDK's retry policy.
//!
//! Ported from `_utils/error_handling.py` of system-one-adapter-python.

use std::sync::{Mutex, PoisonError};

use decision_model_sdk::{Error as SdkError, RetryPolicy};
use http::{Method, Uri};

use crate::response::{RetryCategory, RetryReason};

/// Calls `attempt` under `policy` until it succeeds or the policy stops
/// retrying, and returns its success with the number of retries made, or its
/// last error unchanged (upstream's `run_with_retries_async`).
///
/// Before each retry, the error the previous attempt failed with is appended
/// to `reasons` as a `provider_error` reason, which is what upstream's
/// `before_sleep` hook records. An attempt that must not be retried, such as a
/// reply the vendor declared unfinished, travels inside `R` as a success of
/// the attempt: it ends the loop after one call and adds no reason.
///
/// `uri` names the request in the line the SDK logs before each retry, as
/// `POST` to it; it is printed, never used to send anything.
pub(crate) async fn run_with_retries<R, F, Fut>(
    policy: &RetryPolicy,
    uri: &Uri,
    reasons: &mut Vec<RetryReason>,
    mut attempt: F,
) -> Result<(R, u32), SdkError>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Result<R, SdkError>>,
{
    // The policy calls the closure with the retry number only, so the text of
    // a failure waits here until the closure is called again. A `Mutex` and
    // not a `RefCell`, which would make this future not `Send`; it is never
    // held across an await.
    let failure: Mutex<Option<String>> = Mutex::new(None);
    let mut retries = 0;
    let result = policy
        .run(&Method::POST, uri, |retry| {
            if retry > 0 {
                retries = retry;
                let message = failure
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .take()
                    .unwrap_or_default();
                reasons.push(RetryReason::new(RetryCategory::ProviderError, message));
            }
            let call = attempt();
            let failure = &failure;
            async move {
                let result = call.await;
                if let Err(error) = &result {
                    *failure.lock().unwrap_or_else(PoisonError::into_inner) =
                        Some(error.to_string());
                }
                result
            }
        })
        .await?;
    Ok((result, retries))
}

#[cfg(test)]
#[path = "retry_tests.rs"]
mod tests;
