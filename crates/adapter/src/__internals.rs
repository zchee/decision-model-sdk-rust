//! Entry points into crate-private code, for the parity tests, the fuzz
//! targets and the tests that replace the process environment.
//!
//! This module exists only because those tests and targets must exercise the
//! same code the adapter runs, and that code is crate-private. It is hidden
//! from the documentation, it is behind the non-default `internals` feature,
//! and it carries **no semver promise**: anything here may change or disappear
//! in a patch release.

pub mod decode;
pub mod env;
pub mod metrics;
pub mod schema;
