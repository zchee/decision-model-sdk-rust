//! The provider seam: the trait a model behind the adapter implements, the
//! call it receives and the result it returns, and the built-in providers.
//!
//! Ported from `providers/base.py` of system-one-adapter-python.

#[cfg(feature = "anthropic")]
pub(crate) mod anthropic;
pub(crate) mod factory;
#[cfg(feature = "gemini")]
pub(crate) mod gemini;
#[cfg(any(feature = "openai", feature = "anthropic", feature = "gemini"))]
pub(crate) mod http;
#[cfg(feature = "openai")]
pub(crate) mod openai;
