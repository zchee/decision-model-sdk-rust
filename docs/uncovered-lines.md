# Line coverage

Measured at commit `873f431` on 2026-09-24 with one merged report over both JSON
backends; every line number below is a line of that commit. The measurement ran
on macOS arm64 with rustc 1.98.1 and cargo-llvm-cov:

```sh
export CARGO_TARGET_DIR="$HOME/.cache/rust/target-main"
export CARGO_LLVM_COV_TARGET_DIR="$CARGO_TARGET_DIR/llvm-cov"
env -u RUSTFLAGS -u TYPESAFE_API_KEY cargo --config ~/.config/rust/config.dev.toml llvm-cov clean --workspace
env -u RUSTFLAGS -u TYPESAFE_API_KEY cargo --config ~/.config/rust/config.dev.toml llvm-cov nextest -p typesafe-sdk-rust --all-features --no-report
env -u RUSTFLAGS -u TYPESAFE_API_KEY cargo --config ~/.config/rust/config.dev.toml llvm-cov nextest -p typesafe-sdk-rust --features internals --no-report
env -u RUSTFLAGS -u TYPESAFE_API_KEY cargo --config ~/.config/rust/config.dev.toml llvm-cov report -p typesafe-sdk-rust --fail-under-lines 85 --show-missing-lines
```

The files are named by their present paths (at `873f431` they are `src/...` at
the repository root), and the `report` command is shown with
`-p typesafe-sdk-rust`, which the virtual root manifest needs and the run at
`873f431` did not pass.

`--all-features` selects sonic-rs; `--features internals` selects the default
serde_json backend and keeps the allocation-test seam. Each instrumented run
passes 449 tests of the published SDK package. The separate arbitrary-precision
test run is not part of this coverage report.

The published crate `typesafe-sdk-rust` is held to 85% line coverage by CI's
`coverage` job. CI uses the same clean/two-runs/report sequence without
`--show-missing-lines` or the local target-directory configuration.

## Total

| Lines | Missed | Line coverage | Regions | Missed | Functions | Missed |
| ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| 4,500 | 176 | **96.09%** | 7,079 | 395 | 772 | 33 |

A line counts as covered here when any test executes it in any instantiation.
The summary reports 176 missed lines; `--show-missing-lines` lists 139 distinct
source lines that no instantiation reaches. The two counts use different
aggregation over instantiated code. `crates/sdk/src/codec/backend.rs` is fully covered in
the merged report (14 lines); neither backend is represented only by the other.

## Uncovered lines

"Cheap to cover" marks a line a short test would reach.

### `crates/sdk/src/__internals.rs`

| Lines | Why |
| --- | --- |
| 50-52, 124-130, 135-141, 146-151, 161-163 | Thin wrappers used by benchmarks and fuzz targets (`check_depth`, `decode_list_models`, `api_error`, `parse_retry_after`, `backoff_seconds`). Those targets do not run under this coverage command; the underlying code is covered by unit tests. |

### `crates/sdk/src/client.rs`

| Lines | Why |
| --- | --- |
| 107-109 | `Client::from_env`, the wrapper around `builder().build()`. Environment resolution is tested through the injected lookup; mutating the process environment is unsafe in edition 2024 and is forbidden in this crate. |

### `crates/sdk/src/codec.rs`

| Lines | Why |
| --- | --- |
| 473 | `Detail::Opaque`: the path-tracking pass accepted what the first pass refused. Both passes read the same text with the same type; no input is known to reach this defensive disagreement case. |
| 532 | `Segment::Unknown`, a map key the path tracker could not capture. No response type here has such a key. |
| 743 | Reusing one `Transcoder` value after its deserializer was consumed. Ordinary JSON serializers ask it to write once. Defensive. |
| 760-762, 993-995, 1016-1018, 1243-1245, 1382-1384 | `Visitor::expecting` messages not requested by the current fixtures. **Cheap to cover** for the string/key visitors with a foreign adapter of the wrong shape. |
| 772-774, 780-782, 796-798, 800-802, 804-805, 807-809, 811-812 | Transcoder compatibility arms for `i128`, `u128`, `Option` and newtype values. The selected JSON parser's `deserialize_any` does not produce these shapes. |
| 834, 839, 842 | Number-token transcoding: an extra token-map entry, or token text that fits `u64`/`i64`. The synthetic tests cover a fraction, a wide integer and refusals, but not these arms. **Cheap to cover** by extending the number-token cases. |
| 859 | Transcoding an empty object through the serde_json source. **Cheap to cover** with an empty `RawJson` object written through another serializer. |
| 1037-1040 | Owned bare-string raw-text input. The real serde_json owned capture uses `TextSeed::visit_string`, which is covered; this compatibility arm needs a foreign string adapter. **Cheap to cover**. |
| 1053-1055, 1057-1059, 1061-1063, 1069-1071, 1077-1079, 1081-1083, 1085-1087, 1089-1090 | Raw text requested from a foreign deserializer that directly yields `bool`, `i64`, `i128`, `u128`, `null` or `Option`. The direct `u64` and `f64` cases are covered. **Cheap to cover** for ordinary scalar adapters; 128-bit and option cases need adapters that produce them. |
| 1123 | A second entry after the raw-value token. Real serde_json raw captures contain one entry; a foreign token-keyed map can reach this guard. **Cheap to cover**. |
| 1252-1254, 1256-1258, 1264-1266, 1282-1285, 1287-1289, 1291-1292, 1294-1296, 1298-1299 | `Render` compatibility arms for negative integers, `i128`/`u128`, `Option` and newtypes. **Cheap to cover** for a negative integer; the other shapes need foreign adapters because JSON `deserialize_any` does not yield them. |

### `crates/sdk/src/config.rs`

| Lines | Why |
| --- | --- |
| 252 | The resolved configuration's `Debug` field for `log_endpoint_host(false)`. The builder's field and the event output are tested, not this `Debug`. **Cheap to cover**. |

### `crates/sdk/src/de.rs`

| Lines | Why |
| --- | --- |
| 893, 906 | Level-keyed probabilities on a choice, or option-keyed probabilities on a score. The answer type chooses the shape before reading it, or the members are held raw until it is known; these mixed states keep the match exhaustive. |

### `crates/sdk/src/models.rs`

| Lines | Why |
| --- | --- |
| 56-58 | `Debug` for the `Models` resource handle. **Cheap to cover** with `format!("{:?}", client.models())`. |

### `crates/sdk/src/name.rs`

| Lines | Why |
| --- | --- |
| 101-103 | An owned `String` supplied to `visit_string`. The JSON parsers supply borrowed text or `&str` on this path; a foreign adapter can supply an owned string. |

### `crates/sdk/src/retry.rs`

| Lines | Why |
| --- | --- |
| 339 | A response-validation error carrying a retry delay and selected by a caller's predicate. **Cheap to cover** with a predicate test. |

### `crates/sdk/src/telemetry.rs`

| Lines | Why |
| --- | --- |
| 278 | An event's elapsed time without a start instant (`-`). Every recorded timed event in the tests has a start. |
| 340 | Bytes that are not UTF-8 in a logged body, rendered as U+FFFD. **Cheap to cover** with a trace-level event fixture. |

### `crates/sdk/src/transport/hyper.rs`

| Lines | Why |
| --- | --- |
| 182-184 | `Debug` for the default transport's response future. **Cheap to cover**. |
| 247-249 | A connect timeout mapped to `ErrorKind::Timeout`. Loopback connections are accepted or refused immediately, not left pending; the timeout detection is unit-tested separately. |

## The System One adapter (`crates/adapter`)

Measured at commit `6c00e6f` on 2026-10-03 (every line number below is a line
of that commit; `main` at `0789254` has the same `crates/adapter/src`), on macOS
arm64 with rustc 1.98.1 and cargo-llvm-cov 0.9.1, in CI's form (no target
directory configuration):

```sh
env -u RUSTFLAGS cargo llvm-cov clean --workspace
env -u RUSTFLAGS cargo llvm-cov nextest -p typesafe-sdk-rust-adapter --all-features --no-report
env -u RUSTFLAGS cargo llvm-cov report -p typesafe-sdk-rust-adapter --fail-under-lines 85 --show-missing-lines
```

The run passes 550 tests of the adapter package. It uses the default serde_json
backend; the test run under serde_json's `arbitrary_precision` feature is not
part of this report. CI's `coverage` job holds the adapter to 85% line coverage.

### Total

| Lines | Missed | Line coverage | Regions | Missed | Functions | Missed |
| ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| 3,156 | 58 | **98.16%** | 5,233 | 175 | 486 | 11 |

The summary reports 58 missed lines; `--show-missing-lines` lists 49 distinct
source lines, below, that no instantiation reaches. The files under
`crates/adapter/src/__internals/`, `metrics.rs`, `retry.rs`, `run.rs`,
`schema.rs`, `provider/factory.rs` and the three provider modules are fully
covered.

### `crates/adapter/src/client.rs`

| Lines | Why |
| --- | --- |
| 445-449, 473-475 | The answers of `Client::ask` that do not fit the question set's type. A derived question set always fits the answers of its own prepared questions; only a hand-written `AnswerSet` that disagrees with its own questions reaches these lines. |

### `crates/adapter/src/convert.rs`

| Lines | Why |
| --- | --- |
| 149-150 | The SDK refusing a score criterion's JSON. The criterion was read as one JSON value when the questions were checked, so the SDK's `Content` accepts it; the error is defensive. |

### `crates/adapter/src/decode.rs`

| Lines | Why |
| --- | --- |
| 354-356, 394-396, 413-415, 462-464 | `Visitor::expecting` of the four visitors. serde calls it to write a type error, and the decoder never hands a visitor a value of the wrong JSON type: it checks the type first (`json_type`, lines 177, 224, 258, 286) and reports a wrong type as its own problem. |

### `crates/adapter/src/error.rs`

| Lines | Why |
| --- | --- |
| 99-102 | The kind names `ResponseValidation`, `InvalidRequest`, `Config` and the catch-all for an SDK error a provider fails with. The built-in providers fail only with `Api`, `Connection`, `Timeout` and `ResponseTooLarge`; a provider of the caller's own could return the others. **Cheap to cover** with a scripted provider. |

### `crates/adapter/src/model.rs`

| Lines | Why |
| --- | --- |
| 184 | Equality of a text criterion and a JSON criterion, which no test compares. **Cheap to cover**. |
| 305 | An unknown question type in the second match. The type was already checked to be `noul`, `choice` or `score` at line 265, so this arm is unreachable; it exists because the match is over a string. |

### `crates/adapter/src/prompt.rs`

| Lines | Why |
| --- | --- |
| 127 | A number held as text with a fraction or an exponent, written as Python writes the float. `serde_json` holds numbers as text only under its `arbitrary_precision` feature, whose test run is not part of this report. |
| 189 | The name `an object` of the JSON type found where a question expects another type. The tests find the other five types there. **Cheap to cover**. |

### `crates/adapter/src/provider/http.rs`

| Lines | Why |
| --- | --- |
| 361 | A base URL whose authority has an empty host. The URLs the tests give without a host are refused earlier, by `http::Uri`'s parser or by the scheme and authority check, so this check is defensive. |
| 907 | A piece written after a capped text is already full. No test writes another piece once the cap is reached. **Cheap to cover** with a long error chain. |
| 933, 935-936 | Three ranges of format characters (word joiners and invisible operators, interlinear annotation marks, tag characters) that a message escapes. The tests use characters of the other ranges. **Cheap to cover**. |
| 1082-1084, 1111-1113 | `Debug` of `TransportFuture` and `TransportBody`. **Cheap to cover**. |
| 1128-1130 | `TransportBody::is_end_stream`. `http-body-util`'s `Limited` and `collect` read frames until the body ends and never ask. |

### `crates/adapter/src/provider/mod.rs`

| Lines | Why |
| --- | --- |
| 529-530, 532-533 | The message of an unknown provider name with no provider or with one provider compiled in. This run has all three providers; a unit test checks the text with none in the `--no-default-features` test run, which is not part of this report. |

### `crates/adapter/src/response.rs`

| Lines | Why |
| --- | --- |
| 287 | The error path of serializing `original_probabilities`. Serializing into a `String` with `serde_json` does not fail; the `?` is what a generic serializer needs. |
