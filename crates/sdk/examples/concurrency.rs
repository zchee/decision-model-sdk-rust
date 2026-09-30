//! Asks the same questions about several states at once, with at most a
//! fixed number of calls in flight.
//!
//! `warm_up()` goes first: it checks the API key once and leaves an open
//! HTTP/2 connection in the client's pool, so the calls that follow share it
//! instead of each paying a handshake. Each task gets a clone of the client,
//! which is one reference count; all of them share that one connection. Run
//! it with
//!
//! ```sh
//! TYPESAFE_API_KEY=... cargo run --example concurrency
//! ```
//!
//! Every call is billed by the API.

use std::{process::ExitCode, sync::Arc};

use tokio::{sync::Semaphore, task::JoinSet};
use typesafe_sdk::{Client, Error, Noul, PreparedQuestions, Questions};

/// The most calls in flight at once.
const MAX_IN_FLIGHT: usize = 4;

const STATES: [&str; 8] = [
    "My invoice shows the wrong VAT number.",
    "How do I change my password?",
    "I was charged twice for one order.",
    "The app crashes when I open settings.",
    "Can I get a refund for last month?",
    "Your product is great, thanks!",
    "Where can I download my receipts?",
    "The export button does nothing.",
];

#[tokio::main]
async fn main() -> ExitCode {
    match run().await {
        Ok(0) => ExitCode::SUCCESS,
        Ok(_) => ExitCode::FAILURE,
        Err(error) => {
            eprintln!("error: {error}");
            ExitCode::FAILURE
        }
    }
}

/// Runs the fan-out and returns how many calls failed.
async fn run() -> Result<usize, Error> {
    let client = Client::from_env()?;
    // Fails here, once, on a key the API refuses.
    client.warm_up().await?;

    let questions: Arc<PreparedQuestions> = Arc::new(
        Questions::new()
            .noul("billing", Noul::new().instructions("Is this about billing?"))
            .prepare()?,
    );
    let permits = Arc::new(Semaphore::new(MAX_IN_FLIGHT));

    let mut tasks = JoinSet::new();
    for (index, state) in STATES.into_iter().enumerate() {
        let client = client.clone();
        let questions = Arc::clone(&questions);
        let permits = Arc::clone(&permits);
        tasks.spawn(async move {
            // The permit is held until the call ends, which bounds the calls
            // in flight; the semaphore is never closed, so acquiring cannot
            // fail.
            let _permit = permits.acquire_owned().await.expect("invariant: never closed");
            let answer = client.system_one(state, &questions).send().await;
            (index, answer.map(|response| response.answers().noul("billing").map(|a| a.noul())))
        });
    }

    let mut failed = 0;
    while let Some(joined) = tasks.join_next().await {
        match joined {
            Ok((index, Ok(billing))) => println!("{index}: billing {billing:?}"),
            Ok((index, Err(error))) => {
                failed += 1;
                eprintln!("{index}: {error}");
            }
            Err(join_error) => {
                failed += 1;
                eprintln!("a task did not finish: {join_error}");
            }
        }
    }
    Ok(failed)
}
