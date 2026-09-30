// A response decodes into a set of answers: `Answers`, or a struct that
// derives `QuestionSet`. The note says how to get one.
use typesafe_sdk::Client;

fn main() {
    let client = Client::builder().api_key("key").build().expect("a client");
    let prepared = typesafe_sdk::Questions::new()
        .noul("spam", typesafe_sdk::Noul::new())
        .prepare()
        .expect("a valid set");
    let _request = client.system_one("state", &prepared).typed::<String>();
}
