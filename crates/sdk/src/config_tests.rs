//! Tests for configuration resolution.
//!
//! The resolution cases are ports of the upstream Python SDK's
//! `tests/test_config.py` (`test_resolution`, `test_missing_key`,
//! `test_empty_env_unset`, `test_invalid_timeout`). Upstream sets real
//! environment variables through `monkeypatch`; here every case hands
//! [`Config::resolve`] its own lookup closure, so no test touches the process
//! environment and the cases can run in parallel.

use std::{cell::RefCell, error::Error as StdError, ffi::OsString};

use http::HeaderName;

use super::*;
use crate::ErrorKind;

// ------------------------------------------------------------- fixtures

/// The message a client with no API key fails with, as upstream words it.
const MISSING_KEY: &str =
    "No API key was provided. Pass api_key or set the TYPESAFE_API_KEY environment variable.";

/// The message a key outside printable ASCII fails with, as upstream words it.
const INVALID_KEY: &str =
    "API key must contain only printable ASCII characters without whitespace.";

/// A lookup that finds nothing: a process with none of the variables set.
fn no_env(_: &str) -> Option<String> {
    None
}

/// A lookup over a fixed set of variables.
fn env<'a>(pairs: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Option<String> + 'a {
    move |name| pairs.iter().find(|(key, _)| *key == name).map(|(_, value)| (*value).to_owned())
}

/// Resolves with only an explicit API key and the given environment.
fn with_key(lookup: impl Fn(&str) -> Option<String>) -> Config {
    Config::resolve(Explicit { api_key: Some("test-key".into()), ..Explicit::default() }, lookup)
        .unwrap_or_else(|error| panic!("a configuration with a key failed to resolve: {error:?}"))
}

/// Resolves and expects a configuration error, returning its rendering.
fn config_error(explicit: Explicit, lookup: impl Fn(&str) -> Option<String>) -> String {
    match Config::resolve(explicit, lookup) {
        Ok(config) => panic!("expected a configuration error, resolved {config:?}"),
        Err(error) => {
            assert!(
                matches!(error.kind(), ErrorKind::Config),
                "expected ErrorKind::Config, got {error:?}"
            );
            assert!(StdError::source(&error).is_none(), "a config error has no cause: {error:?}");
            error.to_string()
        }
    }
}

/// The `Debug` of a configuration error with this message, as `Error` renders
/// it.
fn config_debug(message: &str) -> String {
    format!("Error {{ kind: Config, message: {message:?} }}")
}

/// The `Authorization` value as text. It is flagged sensitive, which hides it
/// from `Debug` but not from the bytes.
fn authorization(config: &Config) -> &str {
    config.authorization().to_str().expect("invariant: the bearer value is ASCII")
}

// ------------------------------------------------------------ resolution

#[test]
fn defaults_apply_when_neither_the_caller_nor_the_environment_sets_anything() {
    let config = with_key(no_env);

    assert_eq!(authorization(&config), "Bearer test-key");
    assert_eq!(config.endpoints().system_one(), "https://api.typesafe.ai/v1/systemone");
    assert_eq!(config.endpoints().models(), "https://api.typesafe.ai/v1/models");
    assert_eq!(config.default_model(), "jev-latest");
    assert_eq!(config.timeout(), Some(Duration::from_secs(10)));
    assert_eq!(config.max_response_bytes(), 16 * 1024 * 1024);
    assert!(config.default_headers().is_empty());
}

/// Upstream `test_resolution`, all three sources, in one table.
#[test]
fn each_setting_comes_from_the_caller_then_the_environment_then_the_default() {
    let environment = [
        ("TYPESAFE_API_KEY", "  env-key  "),
        ("TYPESAFE_BASE_URL", "  https://env.test///  "),
        ("TYPESAFE_DEFAULT_MODEL", "  env-model  "),
    ];
    /// One source of settings and what resolving from it must produce.
    struct Case<'a> {
        source: &'a str,
        explicit: Explicit,
        environment: &'a [(&'a str, &'a str)],
        expected: [&'a str; 3],
    }
    let cases = [
        Case {
            source: "default",
            explicit: Explicit { api_key: Some("test-key".into()), ..Explicit::default() },
            environment: &[],
            expected: ["Bearer test-key", "https://api.typesafe.ai/v1/systemone", "jev-latest"],
        },
        Case {
            source: "env",
            explicit: Explicit::default(),
            environment: &environment,
            expected: ["Bearer env-key", "https://env.test/v1/systemone", "env-model"],
        },
        Case {
            source: "constructor",
            explicit: Explicit {
                api_key: Some("code-key".into()),
                base_url: Some("https://code.test///".into()),
                default_model: Some("code-model".into()),
                ..Explicit::default()
            },
            environment: &environment,
            expected: ["Bearer code-key", "https://code.test/v1/systemone", "code-model"],
        },
    ];
    for Case { source, explicit, environment, expected: [key, url, model] } in cases {
        let config = Config::resolve(explicit, env(environment))
            .unwrap_or_else(|error| panic!("source {source}: {error:?}"));
        assert_eq!(authorization(&config), key, "source {source}");
        assert_eq!(config.endpoints().system_one(), url, "source {source}");
        assert_eq!(config.default_model(), model, "source {source}");
        assert_eq!(config.timeout(), Some(Duration::from_secs(10)), "source {source}");
    }
}

/// Upstream `test_missing_key`: unset, empty and whitespace-only all count as
/// no key.
#[test]
fn a_missing_or_blank_environment_key_is_the_upstream_error() {
    for value in [None, Some(""), Some(" \t\n ")] {
        let lookup = |name: &str| {
            assert_eq!(name, "TYPESAFE_API_KEY", "the key must be the first thing looked up");
            value.map(str::to_owned)
        };
        let error =
            Config::resolve(Explicit::default(), lookup).expect_err("a blank key must not resolve");
        assert!(matches!(error.kind(), ErrorKind::Config), "{value:?}: {error:?}");
        assert_eq!(error.to_string(), MISSING_KEY, "{value:?}");
        assert_eq!(format!("{error:?}"), config_debug(MISSING_KEY), "{value:?}");
    }
}

/// Upstream `test_empty_env_unset`. `TYPESAFE_LOG_LEVEL` is set as upstream
/// sets it, and is never even looked up: the SDK does not honour it.
#[test]
fn blank_environment_values_count_as_unset_and_the_log_level_is_never_read() {
    let looked_up = RefCell::new(Vec::new());
    let environment = [
        ("TYPESAFE_BASE_URL", " \t "),
        ("TYPESAFE_DEFAULT_MODEL", " \t "),
        ("TYPESAFE_LOG_LEVEL", " \t "),
    ];
    let lookup = |name: &str| {
        looked_up.borrow_mut().push(name.to_owned());
        env(&environment)(name)
    };

    let config = with_key(lookup);

    assert_eq!(config.endpoints().system_one(), "https://api.typesafe.ai/v1/systemone");
    assert_eq!(config.default_model(), "jev-latest");
    assert_eq!(*looked_up.borrow(), ["TYPESAFE_BASE_URL", "TYPESAFE_DEFAULT_MODEL"]);
}

#[test]
fn explicit_settings_are_used_without_consulting_the_environment() {
    let lookup = |name: &str| -> Option<String> {
        panic!("the environment was consulted for {name} although every setting was explicit")
    };
    let explicit = Explicit {
        api_key: Some(SecretString::from("code-key")),
        base_url: Some("https://code.test".into()),
        default_model: Some("code-model".into()),
        timeout: Some(Some(Duration::from_millis(1500))),
        ..Explicit::default()
    };

    let config = Config::resolve(explicit, lookup)
        .unwrap_or_else(|error| panic!("explicit settings failed to resolve: {error:?}"));

    assert_eq!(authorization(&config), "Bearer code-key");
    assert_eq!(config.endpoints().system_one(), "https://code.test/v1/systemone");
    assert_eq!(config.default_model(), "code-model");
    assert_eq!(config.timeout(), Some(Duration::from_millis(1500)));
}

/// The message an explicit blank default model fails with.
const BLANK_MODEL: &str = "The default model is empty. \
    Pass a non-empty default_model or set the TYPESAFE_DEFAULT_MODEL environment variable.";

/// Blank by Python's `str.strip()`: empty, spaces, mixed whitespace, and the
/// ASCII separators Python counts as whitespace and Rust does not.
const BLANK: [&str; 5] = ["", " ", " \t\n ", "\u{1c}", "\u{1d}\u{1f} \u{1e}"];

/// A lookup over `pairs` that fails the test when it is asked for the API
/// key: an explicit key, whatever it holds, is the key.
fn env_without_key_lookup<'a>(
    pairs: &'a [(&'a str, &'a str)],
) -> impl Fn(&str) -> Option<String> + 'a {
    move |name| {
        assert_ne!(name, "TYPESAFE_API_KEY", "the environment was read for an explicit key");
        env(pairs)(name)
    }
}

/// Upstream strips an explicit key as it strips the environment's since
/// 0.7.1, so a blank one is the missing-key error, and the environment is not
/// consulted even when it holds a usable key: the caller said which key to
/// use, and it is no key at all. The message repeats nothing of the value.
#[test]
fn an_explicit_blank_key_is_the_missing_key_error_and_the_environment_is_not_read() {
    let environment = [("TYPESAFE_API_KEY", "env-key")];
    for given in BLANK {
        let error = Config::resolve(
            Explicit { api_key: Some(given.into()), ..Explicit::default() },
            env_without_key_lookup(&environment),
        )
        .expect_err("an explicit blank key must not resolve");
        assert!(matches!(error.kind(), ErrorKind::Config), "{given:?}: {error:?}");
        assert_eq!(error.to_string(), MISSING_KEY, "{given:?}");
        assert_eq!(format!("{error:?}"), config_debug(MISSING_KEY), "{given:?}");
    }
}

/// Upstream `test_invalid_explicit_key_does_not_fall_back_to_env`: an
/// explicit key that is blank or holds a control character fails, with the
/// environment holding a usable key that is never looked up.
#[test]
fn an_invalid_explicit_key_does_not_fall_back_to_the_environment() {
    let environment = [("TYPESAFE_API_KEY", "env-key")];
    let cases = [
        ("", MISSING_KEY),
        (" \t\r\n ", MISSING_KEY),
        ("\u{0}private", INVALID_KEY),
        ("private\u{0}", INVALID_KEY),
    ];
    for (given, message) in cases {
        let error = Config::resolve(
            Explicit { api_key: Some(given.into()), ..Explicit::default() },
            env_without_key_lookup(&environment),
        )
        .expect_err("an invalid explicit key must not resolve");
        assert!(matches!(error.kind(), ErrorKind::Config), "{given:?}: {error:?}");
        assert_eq!(error.to_string(), message, "{given:?}");
        let debug = format!("{error:?}");
        assert_eq!(debug, config_debug(message), "{given:?}");
        assert!(!debug.contains("private"), "{given:?} leaked into {debug}");
        assert!(!debug.contains("env-key"), "{given:?}: the environment's key leaked into {debug}");
    }
}

/// Upstream `test_api_key_whitespace`: a key padded with whitespace is sent
/// stripped, whether it was passed or read from the environment. The
/// explicit cases run with another key in the environment, which must not
/// win. Python's `str.strip()` also strips U+00A0 and U+001C; U+200B is not
/// whitespace to it, so a key ending in one is refused.
#[test]
fn a_padded_key_is_trimmed_as_python_strips_it_from_either_source() {
    for padding in ["", "\n", "\r\n", " \t\r\n "] {
        let key = format!("{padding}test-key{padding}");

        let environment = [("TYPESAFE_API_KEY", "env-key")];
        let config = Config::resolve(
            Explicit { api_key: Some(key.as_str().into()), ..Explicit::default() },
            env_without_key_lookup(&environment),
        )
        .unwrap_or_else(|error| panic!("explicit {key:?} failed to resolve: {error:?}"));
        assert_eq!(authorization(&config).as_bytes(), b"Bearer test-key", "explicit {key:?}");

        let environment = [("TYPESAFE_API_KEY", key.as_str())];
        let config = Config::resolve(Explicit::default(), env(&environment))
            .unwrap_or_else(|error| panic!("environment {key:?} failed to resolve: {error:?}"));
        assert_eq!(authorization(&config).as_bytes(), b"Bearer test-key", "environment {key:?}");
    }

    let config = Config::resolve(
        Explicit { api_key: Some("\u{a0}key\u{1c}".into()), ..Explicit::default() },
        no_env,
    )
    .unwrap_or_else(|error| panic!("a key padded with U+00A0 and U+001C failed: {error:?}"));
    assert_eq!(authorization(&config).as_bytes(), b"Bearer key", "U+00A0 and U+001C are stripped");

    let rendered = config_error(
        Explicit { api_key: Some("key\u{200b}".into()), ..Explicit::default() },
        no_env,
    );
    assert_eq!(rendered, INVALID_KEY, "U+200B is not whitespace to Python and stays in the key");
}

/// The same rule for the default model, which upstream would send as an
/// empty or whitespace model name.
#[test]
fn an_explicit_blank_default_model_is_a_config_error_even_with_one_in_the_environment() {
    let environment = [("TYPESAFE_DEFAULT_MODEL", "env-model")];
    for given in BLANK {
        let explicit = Explicit {
            api_key: Some("test-key".into()),
            default_model: Some(given.into()),
            ..Explicit::default()
        };
        let error = Config::resolve(explicit, env(&environment))
            .expect_err("an explicit blank model must not resolve");
        assert!(matches!(error.kind(), ErrorKind::Config), "{given:?}: {error:?}");
        assert_eq!(error.to_string(), BLANK_MODEL, "{given:?}");
        assert_eq!(format!("{error:?}"), config_debug(BLANK_MODEL), "{given:?}");
    }
}

/// A padded explicit default model that is not blank is kept byte for byte,
/// as upstream sends it as given.
#[test]
fn a_padded_non_blank_explicit_model_is_kept_byte_for_byte() {
    let environment = [("TYPESAFE_DEFAULT_MODEL", "env-model")];
    for given in ["  m  ", "\tm\t", " a b "] {
        let explicit = Explicit {
            api_key: Some("test-key".into()),
            default_model: Some(given.into()),
            ..Explicit::default()
        };
        let config = Config::resolve(explicit, env(&environment))
            .unwrap_or_else(|error| panic!("explicit {given:?} failed to resolve: {error:?}"));
        assert_eq!(config.default_model().as_bytes(), given.as_bytes(), "model {given:?}");
    }
}

/// Python's `str.strip()` also strips U+001C to U+001F, which Rust's
/// `char::is_whitespace` does not; both strip U+00A0 and U+3000.
#[test]
fn environment_values_are_trimmed_as_python_trims_them() {
    let environment = [
        ("TYPESAFE_API_KEY", "\u{1c}\u{a0}env-key\u{3000}\u{1f}"),
        ("TYPESAFE_DEFAULT_MODEL", "\u{1d}\u{1e}"),
    ];

    let config = Config::resolve(Explicit::default(), env(&environment))
        .unwrap_or_else(|error| panic!("{error:?}"));

    assert_eq!(authorization(&config), "Bearer env-key");
    assert_eq!(config.default_model(), "jev-latest", "a value of separators only is blank");
}

#[test]
fn default_headers_are_kept_as_given_including_repeated_names() {
    let mut headers = HeaderMap::new();
    headers.insert("x-team", HeaderValue::from_static("billing"));
    headers.append("x-tag", HeaderValue::from_static("a"));
    headers.append("x-tag", HeaderValue::from_static("b"));

    let config = Config::resolve(
        Explicit {
            api_key: Some("test-key".into()),
            default_headers: headers.clone(),
            ..Explicit::default()
        },
        no_env,
    )
    .unwrap_or_else(|error| panic!("{error:?}"));

    assert_eq!(*config.default_headers(), headers);
    let tags: Vec<_> = config.default_headers().get_all("x-tag").iter().collect();
    assert_eq!(tags, ["a", "b"]);
}

// ------------------------------------------------------------- base URL

/// AC-F5's URL half: every trailing slash goes, and a path prefix survives.
#[test]
fn trailing_slashes_are_stripped_and_a_path_prefix_is_kept() {
    let config = Config::resolve(
        Explicit {
            api_key: Some("test-key".into()),
            base_url: Some("https://example.test/prefix///".into()),
            ..Explicit::default()
        },
        no_env,
    )
    .unwrap_or_else(|error| panic!("{error:?}"));

    assert_eq!(config.endpoints().system_one(), "https://example.test/prefix/v1/systemone");
    assert_eq!(config.endpoints().models(), "https://example.test/prefix/v1/models");
}

#[test]
fn endpoints_join_the_api_paths_onto_any_usable_base_url() {
    let cases = [
        ("https://api.typesafe.ai", "https://api.typesafe.ai/v1/systemone"),
        ("http://127.0.0.1:8080", "http://127.0.0.1:8080/v1/systemone"),
        ("http://[::1]:8080/a/b", "http://[::1]:8080/a/b/v1/systemone"),
        ("https://example.test:8443/pre%20fix", "https://example.test:8443/pre%20fix/v1/systemone"),
        ("HTTPS://Example.TEST/Prefix", "https://Example.TEST/Prefix/v1/systemone"),
    ];
    for (base, expected) in cases {
        let joined = endpoints(base).unwrap_or_else(|error| panic!("{base}: {error:?}"));
        assert_eq!(joined.system_one(), expected, "base {base}");
        let models = expected.replace("/v1/systemone", "/v1/models");
        assert_eq!(joined.models(), models.as_str(), "base {base}");
    }
}

/// Each unusable base URL fails with its own message, and no message repeats
/// the URL: the userinfo cases carry a credential that must not reach a log.
#[test]
fn an_unusable_base_url_is_a_config_error_that_does_not_repeat_it() {
    let cases = [
        ("", "The base URL is not a valid URL."),
        (
            "https:",
            "The base URL must be absolute, with a scheme and a host, such as https://api.typesafe.ai.",
        ),
        (
            "example.test",
            "The base URL must be absolute, with a scheme and a host, such as https://api.typesafe.ai.",
        ),
        (
            "/v1",
            "The base URL must be absolute, with a scheme and a host, such as https://api.typesafe.ai.",
        ),
        ("https://exa mple.test", "The base URL is not a valid URL."),
        ("ftp://example.test", "The base URL must use http or https."),
        ("https://:443", "The base URL has an empty host."),
        ("https://example.test?key=sk-in-query", "The base URL must not carry a query ('?...')."),
        ("https://example.test/?", "The base URL must not carry a query ('?...')."),
        ("https://example.test#sk-in-fragment", "The base URL must not carry a fragment ('#...')."),
        (
            "https://user:sk-in-userinfo@example.test",
            "The base URL must not carry credentials; pass the API key on its own instead.",
        ),
        (
            "https://sk-in-userinfo@example.test",
            "The base URL must not carry credentials; pass the API key on its own instead.",
        ),
    ];
    for (base, message) in cases {
        let rendered = config_error(
            Explicit {
                api_key: Some("test-key".into()),
                base_url: Some(base.into()),
                ..Explicit::default()
            },
            no_env,
        );
        assert_eq!(rendered, message, "base {base:?}");
        assert!(!rendered.contains("sk-"), "base {base:?} leaked into {rendered:?}");
    }
}

/// A base URL of slashes only is empty once they are stripped.
#[test]
fn a_base_url_of_slashes_only_is_not_a_url() {
    let rendered = config_error(
        Explicit {
            api_key: Some("test-key".into()),
            base_url: Some("///".into()),
            ..Explicit::default()
        },
        no_env,
    );
    assert_eq!(rendered, "The base URL is not a valid URL.");
}

#[test]
fn an_unusable_environment_base_url_fails_the_same_way() {
    let environment = [("TYPESAFE_BASE_URL", " https://user:sk-in-env@example.test/ ")];
    let rendered = config_error(
        Explicit { api_key: Some("test-key".into()), ..Explicit::default() },
        env(&environment),
    );
    assert_eq!(
        rendered,
        "The base URL must not carry credentials; pass the API key on its own instead."
    );
}

// -------------------------------------------------------------- timeout

/// Upstream `test_invalid_timeout` covers 0, -1, infinity and NaN. A
/// `Duration` cannot hold the last three, so zero is the case left.
#[test]
fn a_zero_timeout_is_the_upstream_error_and_any_positive_one_is_kept() {
    let message = "timeout must be a positive, finite number of seconds.";
    let rendered = config_error(
        Explicit {
            api_key: Some("test-key".into()),
            timeout: Some(Some(Duration::ZERO)),
            ..Explicit::default()
        },
        no_env,
    );
    assert_eq!(rendered, message);

    for timeout in [Duration::from_nanos(1), Duration::from_secs(7), Duration::MAX] {
        let config = Config::resolve(
            Explicit {
                api_key: Some("test-key".into()),
                timeout: Some(Some(timeout)),
                ..Explicit::default()
            },
            no_env,
        )
        .unwrap_or_else(|error| panic!("{timeout:?}: {error:?}"));
        assert_eq!(config.timeout(), Some(timeout));
    }
}

/// No deadline is asked for by name, and then no timer is armed at all; it is
/// never spelled as a very long deadline, which a clock can overflow.
#[test]
fn a_deadline_or_no_deadline_is_kept_as_set() {
    let none = Config::resolve(
        Explicit { api_key: Some("test-key".into()), timeout: Some(None), ..Explicit::default() },
        no_env,
    )
    .unwrap_or_else(|error| panic!("{error:?}"));
    assert_eq!(none.timeout(), None);

    let explicit = Explicit {
        api_key: Some("test-key".into()),
        timeout: Some(Some(Duration::from_secs(3))),
        ..Explicit::default()
    };
    let config = Config::resolve(explicit, no_env).unwrap_or_else(|error| panic!("{error:?}"));
    assert_eq!(config.timeout(), Some(Duration::from_secs(3)));
}

// -------------------------------------------------------- response limit

#[test]
fn the_response_limit_is_the_default_or_what_the_caller_set_and_never_zero() {
    let explicit = Explicit {
        api_key: Some("test-key".into()),
        max_response_bytes: Some(1),
        ..Explicit::default()
    };
    let config = Config::resolve(explicit, no_env).unwrap_or_else(|error| panic!("{error:?}"));
    assert_eq!(config.max_response_bytes(), 1);

    let rendered = config_error(
        Explicit {
            api_key: Some("test-key".into()),
            max_response_bytes: Some(0),
            ..Explicit::default()
        },
        no_env,
    );
    assert_eq!(rendered, "max_response_bytes must be at least 1: every response carries a body.");
}

// ------------------------------------------------------ not UTF-8

/// A value no `String` can hold, spelled the way the platform spells one.
fn not_unicode(prefix: &str) -> OsString {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStringExt as _;
        let mut bytes = prefix.as_bytes().to_vec();
        bytes.push(0xff);
        OsString::from_vec(bytes)
    }
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStringExt as _;
        let mut units = prefix.encode_utf16().collect::<Vec<_>>();
        // A lone surrogate: valid in a Windows string, not in UTF-8.
        units.push(0xd800);
        OsString::from_wide(&units)
    }
}

/// A variable the SDK reads whose value is not UTF-8 is refused by name. The
/// value is never repeated: for `TYPESAFE_API_KEY` it is the credential.
#[test]
fn a_variable_that_is_not_utf8_is_a_config_error_naming_the_variable() {
    for name in ["TYPESAFE_API_KEY", "TYPESAFE_BASE_URL", "TYPESAFE_DEFAULT_MODEL"] {
        let lookup = |wanted: &str| {
            if wanted == name {
                Some(not_unicode("sk-not-unicode-secret"))
            } else if wanted == "TYPESAFE_API_KEY" {
                Some(OsString::from("env-key"))
            } else {
                None
            }
        };
        let error = Config::resolve(Explicit::default(), lookup)
            .expect_err("a value that is not UTF-8 must not resolve");
        let message = format!("The {name} environment variable is not valid UTF-8.");
        assert!(matches!(error.kind(), ErrorKind::Config), "{name}: {error:?}");
        assert_eq!(error.to_string(), message, "{name}");
        assert_eq!(format!("{error:?}"), config_debug(&message), "{name}");
        assert!(StdError::source(&error).is_none(), "{name}: {error:?}");
    }

    // A variable the caller's explicit setting makes unnecessary is not read,
    // so an unreadable value there is no error.
    let lookup = |_: &str| Some(not_unicode("unread"));
    let explicit = Explicit {
        api_key: Some("code-key".into()),
        base_url: Some("https://code.test".into()),
        default_model: Some("code-model".into()),
        ..Explicit::default()
    };
    Config::resolve(explicit, lookup).unwrap_or_else(|error| panic!("{error:?}"));
}

// -------------------------------------------------------------- API key

#[test]
fn the_authorization_value_is_flagged_sensitive() {
    let config = with_key(no_env);
    assert!(config.authorization().is_sensitive());
    assert_eq!(format!("{:?}", config.authorization()), "Sensitive");
}

/// Upstream `test_invalid_api_key`: a key holding whitespace, a control, DEL
/// or a non-ASCII character once stripped fails from either source, with a
/// message that does not repeat it. The explicit cases run with a usable key
/// in the environment. The last six are keys `http` could not put in a
/// header or that hold a pasted character; a character that only pads the
/// key would be stripped, so each sits inside it.
#[test]
fn a_key_outside_printable_ascii_is_refused_without_repeating_it() {
    let mut keys: Vec<String> = ['\n', '\r', '\t', '\u{1f}', '\u{7f}', ' ', '\u{e9}', '\u{200b}']
        .into_iter()
        .map(|c| format!("ts_live_private{c}suffix"))
        .collect();
    keys.extend(
        [
            "ts_live_private\nInjected: header",
            "ts_live_private\rsuffix",
            "ts_live_private\u{0}",
            "ts_live_private\u{7f}",
            "ts_live_private\u{a0}suffix",
            "ts_live_private\u{201c}",
        ]
        .map(str::to_owned),
    );
    for key in &keys {
        let environment = [("TYPESAFE_API_KEY", "env-key")];
        let explicit = Config::resolve(
            Explicit { api_key: Some(key.as_str().into()), ..Explicit::default() },
            env_without_key_lookup(&environment),
        )
        .expect_err("an invalid explicit key must not resolve");
        let environment = [("TYPESAFE_API_KEY", key.as_str())];
        let from_environment = Config::resolve(Explicit::default(), env(&environment))
            .expect_err("an invalid environment key must not resolve");
        for (source, error) in [("explicit", explicit), ("environment", from_environment)] {
            assert!(matches!(error.kind(), ErrorKind::Config), "{source} {key:?}: {error:?}");
            let display = error.to_string();
            assert_eq!(display, INVALID_KEY, "{source} {key:?}");
            let debug = format!("{error:?}");
            assert_eq!(debug, config_debug(INVALID_KEY), "{source} {key:?}");
            for rendering in [&display, &debug] {
                assert!(!rendering.contains("ts_live_private"), "{source} {key:?} in {rendering}");
            }
        }
    }
}

/// Every printable ASCII character but the space is accepted as it is.
#[test]
fn a_key_of_printable_ascii_is_sent_byte_for_byte() {
    let key: String = (b'!'..=b'~').map(char::from).collect();
    assert_eq!(key.len(), 94, "'!' to '~' is 94 characters");
    let config = Config::resolve(
        Explicit { api_key: Some(key.as_str().into()), ..Explicit::default() },
        no_env,
    )
    .unwrap_or_else(|error| panic!("{error:?}"));
    assert_eq!(authorization(&config), format!("Bearer {key}"));
}

/// An environment key that is not a valid header value is refused the same
/// way as an explicit one.
#[test]
fn an_unsendable_environment_key_is_refused_without_repeating_it() {
    let environment = [("TYPESAFE_API_KEY", "sk-env\u{7f}secret")];
    let rendered = config_error(Explicit::default(), env(&environment));
    assert_eq!(rendered, INVALID_KEY);
}

// ---------------------------------------------------------------- Debug

#[test]
fn debug_prints_neither_the_key_nor_any_default_header_value() {
    let key = "sk-live-0123456789abcdefDO-NOT-LOG";
    let mut headers = HeaderMap::new();
    headers.insert("x-team", HeaderValue::from_static("billing-team-value"));
    headers.insert(
        HeaderName::from_static("x-client-secret"),
        HeaderValue::from_static("client-secret-value"),
    );
    headers.append("x-team", HeaderValue::from_static("second-team-value"));
    let explicit = Explicit {
        api_key: Some(key.into()),
        base_url: Some("https://example.test/prefix/".into()),
        default_model: Some("jev-latest".into()),
        default_headers: headers,
        ..Explicit::default()
    };

    let config = Config::resolve(explicit, no_env).unwrap_or_else(|error| panic!("{error:?}"));
    let debug = format!("{config:?}");

    assert_eq!(
        debug,
        concat!(
            r#"Config { endpoints: ["POST https://example.test/prefix/v1/systemone", "#,
            r#""GET https://example.test/prefix/v1/models"], default_model: "jev-latest", "#,
            r#"timeout: Some(10s), max_response_bytes: 16777216, authorization: <redacted>, "#,
            r#"default_headers: ["x-team", "x-client-secret"] }"#,
        )
    );
    for secret in
        [key, "sk-live", "Bearer", "billing-team-value", "second-team-value", "client-secret-value"]
    {
        assert!(!debug.contains(secret), "{secret:?} leaked into {debug}");
    }
}

// ------------------------------------------------------ User-Agent product

/// The SDK's own identifier, as `User-Agent` and `X-TypeSafe-SDK` spell it.
fn sdk_identifier() -> String {
    format!("typesafe-sdk-rust/{}", env!("CARGO_PKG_VERSION"))
}

/// An explicit key and the given `User-Agent` product.
fn with_product(product: &str) -> Explicit {
    Explicit {
        api_key: Some("test-key".into()),
        user_agent_product: Some(product.to_owned()),
        ..Explicit::default()
    }
}

/// What every refused product's message starts with.
const PRODUCT_RULE: &str =
    "The user_agent_product must be a product token, name/version (RFC 9110, section 10.1.5): ";

#[test]
fn without_a_product_the_user_agent_is_the_sdk_identifier_and_the_runtime_header_is_sent() {
    let config = with_key(no_env);
    assert_eq!(config.user_agent().to_str().expect("ASCII"), sdk_identifier());
    assert_eq!(*config.user_agent(), crate::constants::SDK_IDENTIFIER);
    assert!(config.send_runtime_header(), "the runtime header is sent unless switched off");

    let off = Config::resolve(
        Explicit {
            api_key: Some("test-key".into()),
            omit_runtime_header: true,
            ..Explicit::default()
        },
        no_env,
    )
    .unwrap_or_else(|error| panic!("{error:?}"));
    assert!(!off.send_runtime_header());
    assert_eq!(off.user_agent().to_str().expect("ASCII"), sdk_identifier());
}

/// Each shape the rules refuse, with the rule its message names. No message
/// repeats the value, and every rendering is free of control and hidden
/// characters, whatever the value held.
#[test]
fn a_user_agent_product_that_is_not_a_product_token_is_a_config_error_naming_the_rule() {
    let too_long = format!("{}/1234", "a".repeat(60));
    assert_eq!(too_long.len(), 65);
    let cases: [(&str, &str); 24] = [
        ("", "it is empty"),
        ("   ", "it contains whitespace"),
        ("\t", "it contains whitespace"),
        ("my app/1.0", "it contains whitespace"),
        ("app/1.0 ", "it contains whitespace"),
        (" app/1.0", "it contains whitespace"),
        ("app/1.0\r\nX-Injected: yes", "it contains whitespace"),
        ("app/1.0\n", "it contains whitespace"),
        ("app\u{0}/1.0", "it contains a control character"),
        ("app/1.0\u{1b}[31m", "it contains a control character"),
        ("app/1.0\u{7f}", "it contains a control character"),
        ("app/1.0\u{a0}", "it contains a character that is not ASCII"),
        ("app/1.0\u{202e}", "it contains a character that is not ASCII"),
        ("caf\u{e9}/1.0", "it contains a character that is not ASCII"),
        ("app", "it has no '/' between the name and the version"),
        ("app/1.0/extra", "it has more than one '/'"),
        ("//", "it has more than one '/'"),
        ("/1.0", "the name before the '/' is empty"),
        ("/", "the name before the '/' is empty"),
        ("app/", "the version after the '/' is empty"),
        ("app/(1.0)", "it contains a character a token cannot hold (RFC 9110, section 5.6.2)"),
        ("app@home/1.0", "it contains a character a token cannot hold (RFC 9110, section 5.6.2)"),
        ("\"app\"/1.0", "it contains a character a token cannot hold (RFC 9110, section 5.6.2)"),
        (&too_long, "it is longer than 64 bytes"),
    ];
    for (given, rule) in cases {
        let rendered = config_error(with_product(given), no_env);
        assert_eq!(rendered, format!("{PRODUCT_RULE}{rule}."), "product {given:?}");
        let debug = format!(
            "{:?}",
            Config::resolve(with_product(given), no_env).expect_err("refused as above")
        );
        assert_eq!(debug, config_debug(&rendered), "product {given:?}");
        crate::rendering_tests::assert_printable(&rendered);
        crate::rendering_tests::assert_printable(&debug);
        if given.len() >= 3 {
            assert!(!rendered.contains(given), "product {given:?} leaked into {rendered:?}");
        }
    }
}

/// The accepted edges: exactly 64 bytes, every `tchar` class on both sides
/// of the `/`, and a name and a version of one character each. The value
/// sent is the product, one space, then the SDK's identifier.
#[test]
fn a_product_token_goes_in_front_of_the_sdk_identifier() {
    let longest = format!("{}/1234", "a".repeat(59));
    assert_eq!(longest.len(), 64);
    let every_tchar = "AZaz09!#$%&'*+-.^_`|~/AZaz09!#$%&'*+-.^_`|~";
    for product in ["ganja-code/0.1.0", "a/1", every_tchar, longest.as_str(), "A/b"] {
        let config = Config::resolve(with_product(product), no_env)
            .unwrap_or_else(|error| panic!("{product:?} was refused: {error:?}"));
        assert_eq!(
            config.user_agent().to_str().expect("ASCII"),
            format!("{product} {}", sdk_identifier()),
            "product {product:?}"
        );
        assert!(config.send_runtime_header(), "product {product:?}");
    }
}

/// `Debug` shows a `User-Agent` other than the default and a runtime header
/// switched off, and nothing of either when they are the default.
#[test]
fn debug_prints_the_user_agent_and_the_runtime_switch_only_when_they_are_not_the_default() {
    let explicit = Explicit {
        base_url: Some("https://example.test".into()),
        omit_runtime_header: true,
        ..with_product("my-app/1.2.0")
    };
    let debug = format!("{:?}", Config::resolve(explicit, no_env).expect("it resolves"));
    assert!(
        debug.ends_with(&format!(
            r#"default_headers: [], user_agent: "my-app/1.2.0 {}", send_runtime_header: false }}"#,
            sdk_identifier()
        )),
        "{debug}"
    );
    crate::rendering_tests::assert_printable(&debug);

    let default = format!("{:?}", with_key(no_env));
    assert!(default.ends_with("default_headers: [] }"), "{default}");
}
