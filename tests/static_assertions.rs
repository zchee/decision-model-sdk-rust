//! Properties of the public types that are checked by the compiler: if one
//! of them stops holding, this file stops compiling.
//!
//! `Error` stays one pointer wide, the client and a retry policy can be shared
//! across tasks, and the future of every call - with a retry policy of its
//! own or the client's - can be sent to another thread, so a call can be
//! awaited inside `tokio::spawn`.

use std::{
    convert::Infallible,
    future::{Ready, ready},
    mem::{size_of, size_of_val},
    sync::LazyLock,
    task::{Context, Poll},
};

use http::{Request, Response};
use tower_service::Service;
use typesafe_sdk::{
    ApiError, Body, Client, ClientBuilder, Error, ErrorKind, Noul, PreparedQuestions, QuestionSet,
    Questions, ResponseValidationError, RetryPolicy, StatusSet, SystemOneResponse,
    de::{AnswerContext, AnswerSet},
    response::{Answer, Answers, ChoiceAnswer, NoulAnswer},
};

#[cfg(feature = "hyper")]
use typesafe_sdk::{HttpService, HyperResponseFuture, HyperTransport, ResponseBody};

// `Result<T, Error>` costs a pointer beside `T`.
const _: () = assert!(size_of::<Error>() == size_of::<usize>());
// A niche in the box makes the success case of a `Result` free as well,
// which is what keeps the hot path from paying for the error path.
const _: () = assert!(size_of::<Result<(), Error>>() == size_of::<usize>());

const fn error_is_shareable<T: Send + Sync + 'static>() {}
const _: () = error_is_shareable::<Error>();
const _: () = error_is_shareable::<ErrorKind>();
const _: () = error_is_shareable::<ApiError>();
const _: () = error_is_shareable::<ResponseValidationError>();

const fn client_is_shareable<T: Clone + Send + Sync>() {}
#[cfg(feature = "hyper")]
const _: () = client_is_shareable::<Client>();
#[cfg(feature = "hyper")]
const _: () = client_is_shareable::<Client<HyperTransport>>();
const _: () = client_is_shareable::<Client<Echo>>();

// A policy, predicate included, can be built once and handed to every task.
const fn policy_is_shareable<T: Clone + Send + Sync + 'static>() {}
const _: () = policy_is_shareable::<RetryPolicy>();
const fn is_copy<T: Copy + Send + Sync + 'static>() {}
const _: () = is_copy::<StatusSet>();

// The default transport answers with this crate's own body type, not hyper's,
// and both it and the future that yields it can move to another thread.
#[cfg(feature = "hyper")]
const fn crosses_threads<T: Send + 'static>() {}
#[cfg(feature = "hyper")]
const _: () = crosses_threads::<ResponseBody>();
#[cfg(feature = "hyper")]
const _: () = crosses_threads::<HyperResponseFuture>();
#[cfg(feature = "hyper")]
const _: fn(<HyperTransport as HttpService>::ResponseBody) -> ResponseBody = |body| body;

// The default transport's future is also `Sync` and `Unpin`, as hyper-util's
// own response future is: whichever path a request takes, the public type
// keeps the auto traits it has always had.
#[cfg(feature = "hyper")]
const fn is_send_sync_unpin<T: Send + Sync + Unpin>() {}
#[cfg(feature = "hyper")]
const _: () = is_send_sync_unpin::<HyperResponseFuture>();

// The response types keep their sizes whatever holds their names: a name is
// as wide as the `String` it replaced, and `None` still costs nothing extra.
// Sizes on a 64-bit target.
#[cfg(target_pointer_width = "64")]
const _: () = {
    assert!(size_of::<SystemOneResponse<Answers>>() == 216);
    assert!(size_of::<Answers>() == 24);
    assert!(size_of::<Answer>() == 64);
    assert!(size_of::<Option<Answer>>() == 64);
    assert!(size_of::<ChoiceAnswer>() == 56);
    assert!(size_of::<Option<ChoiceAnswer>>() == 56);
};

/// A transport of the caller's own, for the bounds over a custom `S`.
#[derive(Debug, Clone, Copy)]
struct Echo;

impl Service<Request<Body>> for Echo {
    type Response = Response<Body>;
    type Error = Infallible;
    type Future = Ready<Result<Response<Body>, Infallible>>;

    fn poll_ready(&mut self, _: &mut Context<'_>) -> Poll<Result<(), Infallible>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, request: Request<Body>) -> Self::Future {
        ready(Ok(Response::new(request.into_body())))
    }
}

/// A question set written by hand, as one is without the `macros` feature
/// (this file is also built without it). `ask::<Ticket>()` sends its questions
/// and decodes into it; that call is
/// `system_one(state, Ticket::prepared()).typed::<Ticket>()`.
struct Ticket {
    spam: NoulAnswer,
}

impl AnswerSet for Ticket {
    fn deserialize_answers<'de, D>(deserializer: D, _: AnswerContext) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(serde::Deserialize)]
        struct Fields {
            spam: NoulAnswer,
        }
        let Fields { spam } = serde::Deserialize::deserialize(deserializer)?;
        Ok(Self { spam })
    }
}

impl QuestionSet for Ticket {
    fn prepared() -> &'static PreparedQuestions {
        static PREPARED: LazyLock<PreparedQuestions> = LazyLock::new(|| {
            Questions::new()
                .noul("spam", Noul::new().instructions("?"))
                .prepare()
                .expect("prepares")
        });
        &PREPARED
    }
}

fn is_send<T: Send>(_: &T) {}

/// Every call's future is `Send`, over the default transport and a custom
/// one; the futures are built and dropped, never polled.
#[test]
fn the_future_of_every_call_is_send() {
    let questions =
        Questions::new().noul("spam", Noul::new().instructions("?")).prepare().expect("prepares");
    let state = String::from("hello");

    let policy = RetryPolicy::default().predicate(|_| true);
    #[cfg(feature = "hyper")]
    {
        let client = Client::builder()
            .api_key("test-key")
            .base_url("http://127.0.0.1:9")
            .build()
            .expect("the client builds");
        is_send(&client.system_one(state.as_str(), &questions).send());
        is_send(&client.system_one(&state, &questions).typed::<Ticket>().send());
        is_send(&client.ask::<Ticket>(&state).send());
        is_send(&client.models().list().send());
        is_send(&client.warm_up());
        is_send(&client.system_one(state.as_str(), &questions).retry(policy.clone()).send());
        is_send(&client.models().list().retry(policy.clone()).send());
        is_send(&client.ask::<Ticket>(&state).retry(policy.clone()).send());
    }

    let custom = ClientBuilder::new()
        .api_key("test-key")
        .base_url("http://127.0.0.1:9")
        .build_with_service(Echo)
        .expect("the client builds");
    is_send(&custom.system_one(state.as_str(), &questions).send());
    is_send(&custom.system_one(&state, &questions).typed::<Ticket>().send());
    is_send(&custom.ask::<Ticket>(&state).send());
    is_send(&custom.models().list().send());
    is_send(&custom.warm_up());
    is_send(&custom.system_one(&state, &questions).typed::<Ticket>().retry(policy.clone()).send());
    is_send(&custom.ask::<Ticket>(&state).retry(policy.clone()).send());
    is_send(&custom.models().list().retry(policy).send());

    // The typed call answers with the struct, read by field.
    let spam: fn(&SystemOneResponse<Ticket>) -> f64 = |response| response.answers().spam.noul();
    let _ = spam;
}

/// A guard on the size of every call's future, unpolled, over the default
/// transport and a custom one. Tokio boxes a future larger than its
/// `BOX_FUTURE_THRESHOLD` (16384 bytes in a release build, 2048 in a debug
/// one) when it is spawned or blocked on, so a change that grows a call's
/// future is meant to be seen here, not found in a profile.
///
/// Over a custom transport the bound IS that threshold, on every platform: a
/// call spawned in a debug build is not boxed, and a change that makes it
/// boxed fails here rather than passing under a looser number. Over the
/// default transport the futures are over the threshold anyway (hyper's
/// response future is the larger one), so what is guarded there is growth:
/// the bounds are the sizes measured on macOS arm64 and Linux x86_64 -
/// identical on both, in both profiles, 24 bytes less each without the
/// default features - plus 32 bytes. The transport's wait for the first
/// connection of a client took 16 of those bytes: a reference count held by
/// the transport and one held by its connector, both inside the transport
/// the call clones. Targets other than macOS and Linux (Windows among them)
/// have not been measured, so their bounds are looser. Raising a bound is a
/// decision to state, not a number to bump.
#[test]
fn the_future_of_every_call_stays_small() {
    // Tokio 1.53.1 `runtime/mod.rs`: the debug build's `BOX_FUTURE_THRESHOLD`.
    const TOKIO_DEBUG_BOX: usize = 2048;
    // Measured over the default transport: 2360, 2344 and 2048 bytes (2344,
    // 2328 and 2032 before the wait for the first connection), `ask` the same
    // as `typed`; over a custom transport 2040, 2024 and 1728.
    #[cfg(all(feature = "hyper", any(target_os = "macos", target_os = "linux")))]
    const DEFAULT_TRANSPORT: [usize; 4] = [2376, 2360, 2360, 2064];
    #[cfg(all(feature = "hyper", not(any(target_os = "macos", target_os = "linux"))))]
    const DEFAULT_TRANSPORT: [usize; 4] = [2816, 2816, 2816, 2560];
    #[cfg(feature = "hyper")]
    const SYSTEM_ONE: usize = DEFAULT_TRANSPORT[0];
    #[cfg(feature = "hyper")]
    const TYPED: usize = DEFAULT_TRANSPORT[1];
    #[cfg(feature = "hyper")]
    const ASK: usize = DEFAULT_TRANSPORT[2];
    #[cfg(feature = "hyper")]
    const MODELS: usize = DEFAULT_TRANSPORT[3];

    let questions =
        Questions::new().noul("spam", Noul::new().instructions("?")).prepare().expect("prepares");
    let state = String::from("hello");
    #[cfg(feature = "hyper")]
    let client = Client::builder()
        .api_key("test-key")
        .base_url("http://127.0.0.1:9")
        .default_model("jev-latest")
        .build()
        .expect("the client builds");
    let custom = ClientBuilder::new()
        .api_key("test-key")
        .base_url("http://127.0.0.1:9")
        .default_model("jev-latest")
        .build_with_service(Echo)
        .expect("the client builds");

    let sizes = [
        #[cfg(feature = "hyper")]
        (
            "system_one",
            size_of_val(&client.system_one(state.as_str(), &questions).send()),
            SYSTEM_ONE,
        ),
        #[cfg(feature = "hyper")]
        (
            "typed",
            size_of_val(&client.system_one(&state, &questions).typed::<Ticket>().send()),
            TYPED,
        ),
        #[cfg(feature = "hyper")]
        ("ask", size_of_val(&client.ask::<Ticket>(&state).send()), ASK),
        #[cfg(feature = "hyper")]
        ("models", size_of_val(&client.models().list().send()), MODELS),
        (
            "system_one, custom",
            size_of_val(&custom.system_one(state.as_str(), &questions).send()),
            TOKIO_DEBUG_BOX,
        ),
        (
            "typed, custom",
            size_of_val(&custom.system_one(&state, &questions).typed::<Ticket>().send()),
            TOKIO_DEBUG_BOX,
        ),
        ("ask, custom", size_of_val(&custom.ask::<Ticket>(&state).send()), TOKIO_DEBUG_BOX),
        ("models, custom", size_of_val(&custom.models().list().send()), TOKIO_DEBUG_BOX),
    ];
    for (call, size, bound) in sizes {
        println!("{call:<20} {size:>6} bytes (bound {bound})");
    }
    for (call, size, bound) in sizes {
        assert!(size <= bound, "the {call} future is {size} bytes, over its bound of {bound}");
    }
}
