// `Client::ask` takes a question set; `Answers` decodes answers but declares
// no questions.
use typesafe_sdk::{Answers, Client};

fn main() {
    let client = Client::builder().api_key("key").build().expect("a client");
    let _request = client.ask::<Answers>("state");
}
