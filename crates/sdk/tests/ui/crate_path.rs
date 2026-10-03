// `#[question_set(...)]` takes `crate = <path>` only. (An unresolved path is
// in `crate_path_unresolved.rs`: rustc reports it only when no derive in the
// file failed.)
use decision_model_sdk::{NoulAnswer, QuestionSet};

#[derive(QuestionSet)]
#[question_set(krate = decision_model_sdk)]
struct UnknownKey {
    #[noul]
    spam: NoulAnswer,
}

#[derive(QuestionSet)]
#[question_set(crate = decision_model_sdk, crate = decision_model_sdk)]
struct Twice {
    #[noul]
    spam: NoulAnswer,
}

fn main() {}
