//! Asks three questions - a yes/no, a choice and a score - about one
//! support message and prints each answer.
//!
//! The client is configured by the environment: `TYPESAFE_API_KEY`, and
//! optionally `TYPESAFE_BASE_URL` and `TYPESAFE_DEFAULT_MODEL`. Run it with
//!
//! ```sh
//! TYPESAFE_API_KEY=... cargo run --example quickstart
//! ```
//!
//! Every call is billed by the API.

use std::process::ExitCode;

use typesafe_sdk::{Choice, Client, Error, Noul, Questions, Score};

#[tokio::main]
async fn main() -> ExitCode {
    match run().await {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            // `Display` is the sentence meant for a person; server text in it
            // is already escaped and cut.
            eprintln!("error: {error}");
            ExitCode::FAILURE
        }
    }
}

async fn run() -> Result<(), Error> {
    let client = Client::from_env()?;

    // Validated and serialized once; a long-running program keeps this and
    // reuses it for every call that asks the same questions.
    let questions = Questions::new()
        .noul("billing", Noul::new().instructions("Is this message about billing?"))
        .choice("tone", Choice::new(["calm", "annoyed", "angry"]).instructions("What is the tone?"))
        .score(
            "urgency",
            Score::new(["can wait", "this week", "today"]).instructions("How urgent is it?"),
        )
        .prepare()?;

    let state = "I was charged twice for the same order. Please fix it before Friday.";
    let response = client.system_one(state, &questions).send().await?;

    println!("model: {}", response.model());
    for (name, answer) in response.answers().nouls() {
        println!("{name}: {:.3}", answer.noul());
    }
    for (name, answer) in response.answers().choices() {
        println!("{name}: {} (confidence {:.3})", answer.choice(), answer.confidence());
    }
    for (name, answer) in response.answers().scores() {
        println!("{name}: {:.3} (confidence {:.3})", answer.score(), answer.confidence());
    }
    if let Some(id) = response.meta().request_id() {
        println!("request id: {id}");
    }
    Ok(())
}
