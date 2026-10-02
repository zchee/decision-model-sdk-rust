//! A built-in provider from a provider name and a model.
//!
//! Ported from `providers/__init__.py` of system-one-adapter-python.

use std::sync::Arc;

#[cfg(feature = "anthropic")]
use super::anthropic::AnthropicProvider;
#[cfg(feature = "gemini")]
use super::gemini::GeminiProvider;
#[cfg(feature = "openai")]
use super::openai::OpenAiProvider;
use super::{Provider, ProviderName};
use crate::error::Error;

/// The provider `name` names, asking `model`, built as its public builder
/// builds it with nothing set but the model: the key and the base URL come
/// from the environment, read now.
///
/// Upstream's error for a provider that is not installed has no counterpart:
/// a provider whose feature is off has no [`ProviderName`].
///
/// # Errors
///
/// The [`ErrorKind::Config`](crate::ErrorKind::Config) error of the
/// provider's own `build`, unchanged: no key in the environment, a base URL
/// it refuses, a certificate verifier that cannot be built.
#[cfg_attr(
    not(any(feature = "openai", feature = "anthropic", feature = "gemini")),
    expect(unused_variables, reason = "with no provider compiled in, no name reaches a model")
)]
pub(crate) fn build(name: ProviderName, model: &str) -> Result<Arc<dyn Provider>, Error> {
    match name {
        #[cfg(feature = "openai")]
        ProviderName::OpenAi => Ok(Arc::new(OpenAiProvider::builder(model).build()?)),
        #[cfg(feature = "anthropic")]
        ProviderName::Anthropic => Ok(Arc::new(AnthropicProvider::builder(model).build()?)),
        #[cfg(feature = "gemini")]
        ProviderName::Gemini => Ok(Arc::new(GeminiProvider::builder(model).build()?)),
    }
}

#[cfg(test)]
#[path = "factory_tests.rs"]
mod tests;
