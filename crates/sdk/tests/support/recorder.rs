//! The event recorder the logging tests of the client and of the retry
//! policy share.

use std::{
    fmt::{self, Write as _},
    sync::{Arc, Mutex},
};

use tracing::{
    Event, Level, Metadata, Subscriber,
    field::{Field, Visit},
    span,
};

/// Every event recorded while it is the default subscriber, as lines of
/// text: level, target, then each field.
#[derive(Clone, Default)]
pub(crate) struct Recorder(pub(crate) Arc<Mutex<Vec<(Level, String)>>>);

impl Recorder {
    /// This crate's events at `level`, without the target; hyper's own are
    /// left out.
    pub(crate) fn at(&self, level: Level) -> Vec<String> {
        let events = self.0.lock().expect("not poisoned");
        events
            .iter()
            .filter(|(at, _)| *at == level)
            .filter_map(|(_, line)| line.strip_prefix("typesafe_sdk "))
            .map(str::to_owned)
            .collect()
    }
}

struct Line(String);

impl Visit for Line {
    fn record_debug(&mut self, field: &Field, value: &dyn fmt::Debug) {
        write!(self.0, " {}={value:?}", field.name()).expect("a String takes any write");
    }
}

impl Subscriber for Recorder {
    fn enabled(&self, _: &Metadata<'_>) -> bool {
        true
    }

    fn new_span(&self, _: &span::Attributes<'_>) -> span::Id {
        span::Id::from_u64(1)
    }

    fn record(&self, _: &span::Id, _: &span::Record<'_>) {}

    fn record_follows_from(&self, _: &span::Id, _: &span::Id) {}

    fn event(&self, event: &Event<'_>) {
        let mut line = Line(event.metadata().target().to_owned());
        event.record(&mut line);
        self.0.lock().expect("not poisoned").push((*event.metadata().level(), line.0));
    }

    fn enter(&self, _: &span::Id) {}

    fn exit(&self, _: &span::Id) {}
}

/// `recorder` installed as this thread's subscriber, until dropped.
///
/// tracing-core caches a callsite's interest when the callsite is first
/// reached. While a single dispatcher is registered in the process, that
/// cache asks only the default of the thread that reached the callsite
/// (`Rebuilder::JustOne` in tracing-core 0.1.36 `callsite.rs`). libtest
/// runs the tests of a binary as threads of one process, so a callsite
/// first reached by another test's thread, which has no subscriber, was
/// cached as `never`, and this recorder saw none of its events. A second
/// registered dispatcher, held here, makes the cache ask every live
/// dispatcher instead: this recorder wants the event and the other does
/// not, which caches `sometimes`, and each event then goes to whichever
/// subscriber its own thread has.
pub(crate) struct Installed {
    _default: tracing::subscriber::DefaultGuard,
    _second: tracing::Dispatch,
}

pub(crate) fn install(recorder: &Recorder) -> Installed {
    let second = tracing::Dispatch::new(tracing::subscriber::NoSubscriber::default());
    Installed { _default: tracing::subscriber::set_default(recorder.clone()), _second: second }
}

/// `<prefix><digits>ms<suffix>`, and nothing else.
#[track_caller]
pub(crate) fn assert_timed(line: &str, prefix: &str, suffix: &str) {
    let millis = line
        .strip_prefix(prefix)
        .and_then(|rest| rest.strip_suffix(suffix))
        .and_then(|rest| rest.strip_suffix("ms"))
        .unwrap_or_else(|| panic!("{line:?} is not {prefix:?}<n>ms{suffix:?}"));
    assert!(millis.bytes().all(|byte| byte.is_ascii_digit()) && !millis.is_empty(), "{line}");
}
