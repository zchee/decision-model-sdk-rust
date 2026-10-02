//! The environment lookup the providers read their keys and base URLs
//! through, in a build with the `internals` feature: a test sets variables
//! here without touching the process environment.
//!
//! Edition 2024 makes `std::env::set_var` unsafe, and this workspace forbids
//! unsafe code, so a test cannot set `OPENAI_API_KEY` for itself. It calls
//! [`replace`] instead: while the returned [`Replaced`] lives, every lookup a
//! provider makes is answered from the variables set on it.
//!
//! A build with the `internals` feature never reads the process environment
//! for a provider, whether or not a replacement is in force: while none is,
//! every variable is unset. A test that forgets [`replace`] therefore sees
//! every variable unset, and so never the key of the machine it runs on,
//! with which it could send a billed request. The same holds for any
//! program built with the feature: its providers take their keys and base
//! URLs from their builders or from a replacement, never from the process
//! environment.
//!
//! The replacement is process-wide, because a provider the client builds for
//! itself reads its variables wherever the call happens to run. Tests that
//! replace the environment therefore run one after another: [`replace`]
//! waits until the previous [`Replaced`] is dropped. A test that builds a
//! provider from the environment without calling [`replace`] sees no
//! variable when no replacement is in force, and another test's variables
//! when that test's replacement is in force in the same process.

#![cfg(any(feature = "openai", feature = "anthropic", feature = "gemini"))]

use std::{
    collections::BTreeMap,
    ffi::OsString,
    sync::{Condvar, Mutex, MutexGuard, PoisonError},
};

/// The variables of the replacement in force, or `None` while none is.
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
/// returned value is dropped. After that every variable is unset again: the
/// process environment is read neither while the value lives nor afterwards.
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

/// The variable `name` in the replacement in force, or `None` when it is
/// unset there or no replacement is in force. The process environment is
/// never read.
pub(crate) fn lookup(name: &str) -> Option<OsString> {
    replacement().as_ref().and_then(|variables| variables.get(name).cloned())
}
