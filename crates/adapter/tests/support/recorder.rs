//! The event recorder the logging tests share: a hand-written `tracing`
//! subscriber that keeps every event, and the fields of every span, as lines
//! of text.

use std::{
    collections::HashMap,
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

/// Everything recorded while it is the default subscriber, as lines of
/// text. An event is one line: the target, then each field as ` name=value`.
/// A span is one line when it is opened and one for each later
/// `Span::record`: the target, then the span's name, then each field that
/// was given a value there, as ` name=value`.
#[derive(Clone, Default)]
pub(crate) struct Recorder(Arc<Mutex<Recorded>>);

#[derive(Default)]
struct Recorded {
    /// In the order recorded.
    lines: Vec<(Kind, String)>,
    /// The spans opened so far, by id: a later record names its span by id
    /// only. A span's id is its number in the order opened, from 1.
    spans: HashMap<u64, &'static Metadata<'static>>,
}

/// What a line was recorded for.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    /// An event, at this level.
    Event(Level),
    /// A span that was opened, or values recorded on one later.
    Span,
}

impl Recorder {
    /// The adapter's own events at `level`, without the target; the SDK's
    /// and hyper's are left out, and so is every line of a span.
    pub(crate) fn at(&self, level: Level) -> Vec<String> {
        let recorded = self.0.lock().expect("not poisoned");
        recorded
            .lines
            .iter()
            .filter(|(kind, _)| *kind == Kind::Event(level))
            .filter_map(|(_, line)| line.strip_prefix(TARGET))
            .map(str::to_owned)
            .collect()
    }

    /// Every recorded line, of every target and every level, in the order
    /// recorded: the events and the spans of the adapter, of the SDK, and of
    /// any other crate that logs through `tracing`, such as `h2` and
    /// `hyper_util`. Each line starts with its target.
    #[allow(
        dead_code,
        reason = "this file is a module of several test targets, and not each of them reads \
                  the events of other crates"
    )]
    pub(crate) fn all(&self) -> Vec<String> {
        let recorded = self.0.lock().expect("not poisoned");
        recorded.lines.iter().map(|(_, line)| line.clone()).collect()
    }
}

/// The start of a span's line: its target, then its name.
fn span_line(metadata: &Metadata<'_>) -> Line {
    Line(format!("{} {}", metadata.target(), metadata.name()))
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

    fn new_span(&self, attributes: &span::Attributes<'_>) -> span::Id {
        let metadata = attributes.metadata();
        let mut line = span_line(metadata);
        attributes.record(&mut line);
        let mut recorded = self.0.lock().expect("not poisoned");
        recorded.lines.push((Kind::Span, line.0));
        let id = u64::try_from(recorded.spans.len()).expect("fewer spans than a u64 counts") + 1;
        recorded.spans.insert(id, metadata);
        span::Id::from_u64(id)
    }

    fn record(&self, id: &span::Id, values: &span::Record<'_>) {
        let mut recorded = self.0.lock().expect("not poisoned");
        // An id this recorder did not give out names no span it knows.
        let Some(metadata) = recorded.spans.get(&id.into_u64()).copied() else {
            return;
        };
        let mut line = span_line(metadata);
        values.record(&mut line);
        recorded.lines.push((Kind::Span, line.0));
    }

    fn record_follows_from(&self, _: &span::Id, _: &span::Id) {}

    fn event(&self, event: &Event<'_>) {
        let mut line = Line(event.metadata().target().to_owned());
        event.record(&mut line);
        let kind = Kind::Event(*event.metadata().level());
        self.0.lock().expect("not poisoned").lines.push((kind, line.0));
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
