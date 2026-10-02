//! The environment lookup the providers read their keys and base URLs
//! through, replaceable so a test sets variables without touching the
//! process environment.
//!
//! Edition 2024 makes `std::env::set_var` unsafe, and this workspace forbids
//! unsafe code, so a test cannot set `OPENAI_API_KEY` for itself. It calls
//! [`replace`] instead: while the returned [`Replaced`] lives, every lookup a
//! provider makes is answered from the variables set on it, and the process
//! environment is not read at all. A test so sees neither the machine's own
//! keys nor another test's.
//!
//! The replacement is process-wide, because a provider the client builds for
//! itself reads its variables wherever the call happens to run. Tests that
//! replace the environment therefore run one after another: [`replace`]
//! waits until the previous [`Replaced`] is dropped. A test that builds a
//! provider from the environment without calling [`replace`] may see another
//! test's variables when both run as threads of one process.

#![cfg(any(feature = "openai", feature = "anthropic", feature = "gemini"))]

use std::{
    collections::BTreeMap,
    ffi::OsString,
    sync::{Condvar, Mutex, MutexGuard, PoisonError},
};

/// The variables of the replacement in force, or `None` while the process
/// environment is read.
static REPLACEMENT: Mutex<Option<BTreeMap<String, OsString>>> = Mutex::new(None);

/// Signalled when a replacement ends, for a [`replace`] that waits.
static ENDED: Condvar = Condvar::new();

/// The state, whether or not a test panicked while it held the lock: the map
/// is replaced or edited in one step, so it is never left half-written.
fn replacement() -> MutexGuard<'static, Option<BTreeMap<String, OsString>>> {
    REPLACEMENT.lock().unwrap_or_else(PoisonError::into_inner)
}

/// A replaced environment, in force until it is dropped.
///
/// It starts with no variable set.
#[derive(Debug)]
pub struct Replaced(());

/// Replaces the environment the providers read with an empty one, until the
/// returned value is dropped.
///
/// Waits while another replacement is in force, so a second call on the same
/// thread without dropping the first never returns.
#[must_use = "the environment is replaced only while this value lives"]
pub fn replace() -> Replaced {
    let mut state = replacement();
    while state.is_some() {
        state = ENDED.wait(state).unwrap_or_else(PoisonError::into_inner);
    }
    *state = Some(BTreeMap::new());
    Replaced(())
}

impl Replaced {
    /// Sets the variable `name` to `value`.
    pub fn set(&self, name: &str, value: impl Into<OsString>) {
        if let Some(variables) = replacement().as_mut() {
            variables.insert(name.to_owned(), value.into());
        }
    }

    /// Unsets the variable `name`.
    pub fn remove(&self, name: &str) {
        if let Some(variables) = replacement().as_mut() {
            variables.remove(name);
        }
    }
}

impl Drop for Replaced {
    fn drop(&mut self) {
        *replacement() = None;
        ENDED.notify_one();
    }
}

/// The variable `name` in the replacement in force: `Some(None)` when it is
/// unset there, and `None` when no replacement is in force and the process
/// environment is to be read.
pub(crate) fn lookup(name: &str) -> Option<Option<OsString>> {
    replacement().as_ref().map(|variables| variables.get(name).cloned())
}
