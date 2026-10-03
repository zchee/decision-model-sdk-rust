//! Unit tests of `factory.rs`: which provider a name builds, and what a
//! failed build returns.

#[cfg(any(feature = "openai", feature = "anthropic", feature = "gemini"))]
use super::build;
use crate::provider::{ParseProviderNameError, ProviderName};

#[cfg(feature = "internals")]
#[cfg(any(feature = "openai", feature = "anthropic", feature = "gemini"))]
#[test]
fn every_compiled_in_name_builds_its_own_provider() {
    let environment = crate::__internals::env::replace();
    for variable in ["OPENAI_API_KEY", "ANTHROPIC_API_KEY", "GEMINI_API_KEY"] {
        environment.set(variable, "test-key");
    }

    for name in ProviderName::all() {
        let provider = build(*name, "a-model").expect("a key is set for every provider");

        let expected = match name {
            #[cfg(feature = "openai")]
            ProviderName::OpenAi => "decision_model_adapter::OpenAiProvider",
            #[cfg(feature = "anthropic")]
            ProviderName::Anthropic => "decision_model_adapter::AnthropicProvider",
            #[cfg(feature = "gemini")]
            ProviderName::Gemini => "decision_model_adapter::GeminiProvider",
        };
        assert_eq!(provider.type_name(), expected, "{name}");
        assert_eq!(provider.model_name(), "a-model", "{name}");
    }
}

#[cfg(all(feature = "gemini", feature = "internals"))]
#[test]
// Upstream: tests/test_provider_requests.py::test_build_providers_select_gemini
fn a_gemini_name_builds_a_gemini_provider() {
    let environment = crate::__internals::env::replace();
    environment.set("GEMINI_API_KEY", "test-key");

    let provider = build(ProviderName::Gemini, "gemini-3.8-flash").expect("a key is set");

    assert_eq!(provider.type_name(), "decision_model_adapter::GeminiProvider");
    assert_eq!(provider.model_name(), "gemini-3.8-flash");
}

/// With no key anywhere each provider's own builder fails, and the factory
/// hands that error back as it is.
#[cfg(any(feature = "openai", feature = "anthropic", feature = "gemini"))]
#[test]
fn a_failed_build_is_the_providers_own_config_error() {
    // This build reads no variable without `internals`; with it, an empty
    // replacement keeps another test's variables out.
    #[cfg(feature = "internals")]
    let _environment = crate::__internals::env::replace();

    for name in ProviderName::all() {
        let error = build(*name, "a-model").map(drop).expect_err("no key is set");

        let own = match name {
            #[cfg(feature = "openai")]
            ProviderName::OpenAi => super::OpenAiProvider::builder("a-model").build().map(drop),
            #[cfg(feature = "anthropic")]
            ProviderName::Anthropic => {
                super::AnthropicProvider::builder("a-model").build().map(drop)
            }
            #[cfg(feature = "gemini")]
            ProviderName::Gemini => super::GeminiProvider::builder("a-model").build().map(drop),
        }
        .expect_err("no key is set");
        assert!(matches!(error.kind(), crate::ErrorKind::Config), "{name}: {error:?}");
        assert_eq!(error.to_string(), own.to_string(), "{name}");
        assert!(error.to_string().contains("API_KEY"), "{name}: {error}");
    }
}

#[test]
// Upstream: tests/test_provider_requests.py::test_unknown_provider_is_rejected
fn an_unknown_provider_name_is_rejected() {
    let error = "nope".parse::<ProviderName>().expect_err("no provider is called nope");

    assert_eq!(error, ParseProviderNameError);
    assert!(error.to_string().starts_with("Unknown provider."), "{error}");
    assert!(!error.to_string().contains("nope"), "the message repeats the name: {error}");
}
