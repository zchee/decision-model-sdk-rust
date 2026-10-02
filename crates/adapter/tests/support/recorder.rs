//! The event recorder the logging tests share: a hand-written `tracing`
//! subscriber that keeps every event as one line of text.

use std::{
    fmt::{self, Write as _},
    sync::{Arc, Mutex},
};

use tracing::{
    Event, Level, Metadata, Subscriber,
    field::{Field, Visit},
    span,
};

/// The target of the adapter's own events.
const TARGET: &str = "system_one_adapter";

/// Every event recorded while it is the default subscriber, as lines of
/// text: the target, then each field as ` name=value`.
#[derive(Clone, Default)]
pub(crate) struct Recorder(Arc<Mutex<Vec<(Level, String)>>>);

impl Recorder {
    /// The adapter's own events at `level`, without the target; the SDK's
    /// and hyper's are left out.
    pub(crate) fn at(&self, level: Level) -> Vec<String> {
        let events = self.0.lock().expect("not poisoned");
        events
            .iter()
            .filter(|(at, _)| *at == level)
            .filter_map(|(_, line)| line.strip_prefix(TARGET))
            .map(str::to_owned)
            .collect()
    }

    /// Every recorded event, of every target and every level, in the order
    /// recorded: the adapter's, the SDK's, and those of any other crate that
    /// logs through `tracing`, such as `h2` and `hyper_util`. Each line
    /// starts with its target.
    #[allow(
        dead_code,
        reason = "this file is a module of several test targets, and not each of them reads \
                  the events of other crates"
    )]
    pub(crate) fn all(&self) -> Vec<String> {
        let events = self.0.lock().expect("not poisoned");
        events.iter().map(|(_, line)| line.clone()).collect()
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
/// The subscriber is scoped to the thread, never process-wide, so a test
/// passes under nextest (a process per test) and under libtest (a thread per
/// test) alike.
///
/// tracing-core caches a callsite's interest when the callsite is first
/// reached. While a single dispatcher is registered in the process, that
/// cache asks only the default of the thread that reached the callsite. A
/// callsite first reached by another test's thread, which has no subscriber,
/// would be cached as `never`, and this recorder would see none of its
/// events. A second registered dispatcher, held here, makes the cache ask
/// every live dispatcher instead, and each event then goes to whichever
/// subscriber its own thread has.
pub(crate) struct Installed {
    _default: tracing::subscriber::DefaultGuard,
    _second: tracing::Dispatch,
}

pub(crate) fn install(recorder: &Recorder) -> Installed {
    let second = tracing::Dispatch::new(tracing::subscriber::NoSubscriber::default());
    Installed { _default: tracing::subscriber::set_default(recorder.clone()), _second: second }
}
