//! Declares the questions as a struct with `#[derive(QuestionSet)]` and reads
//! the answers from its fields.
//!
//! The questions are serialized at compile time and the response decodes
//! straight into the struct. It needs the `macros` feature, which is on by
//! default. Run it with
//!
//! ```sh
//! TYPESAFE_API_KEY=... cargo run --example typed_answers
//! ```
//!
//! Every call is billed by the API.

use std::process::ExitCode;

use typesafe_sdk::{ChoiceAnswer, Client, Error, NoulAnswer, QuestionSet, ScoreAnswer};

/// One question per field; the field's type says the question's kind.
#[derive(QuestionSet)]
struct Ticket {
    #[noul(instructions = "Is this message about billing?", yes = "payments or invoices")]
    billing: NoulAnswer,
    #[choice(
        instructions = "What is the tone?",
        options("calm" = "neutral or polite", "annoyed", "angry")
    )]
    tone: ChoiceAnswer,
    #[score(instructions = "How urgent is it?", levels("can wait", "this week", "today"))]
    urgency: ScoreAnswer,
}

#[tokio::main]
async fn main() -> ExitCode {
    match run().await {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("error: {error}");
            ExitCode::FAILURE
        }
    }
}

async fn run() -> Result<(), Error> {
    let client = Client::from_env()?;
    let state = "I was charged twice for the same order. Please fix it before Friday.";

    let response = client.ask::<Ticket>(state).send().await?;
    let ticket = response.answers();

    println!("billing: {:.3}", ticket.billing.noul());
    println!("tone: {} (confidence {:.3})", ticket.tone.choice(), ticket.tone.confidence());
    for (level, probability) in ticket.urgency.probabilities() {
        // A level's description is the text the question gave it.
        let label = ticket.urgency.description(level).and_then(|text| text.as_text());
        println!("urgency level {level} ({}): {probability:.3}", label.unwrap_or("?"));
    }
    Ok(())
}
