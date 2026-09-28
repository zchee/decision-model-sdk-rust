# Fuzz targets

Two [cargo-fuzz](https://github.com/rust-fuzz/cargo-fuzz) targets for the code
that reads bytes a server chose:

| Target | Input | Property |
| --- | --- | --- |
| `decode_response` | a response body | the depth guard, the System One decoder into `Answers` and into a derived answer set, the models decoder and the error-body reader behind `ApiError` each return `Ok` or `Err`; rendering what they return does not panic |
| `retry_after` | a `retry-after-ms` and/or `Retry-After` value (the first byte picks which, see the target's docs) | the parser returns `None` or a `Duration`, and the same answer twice |

A panic, an abort (a stack overflow included), an input that runs past
`-timeout` or a process that passes `-rss_limit_mb` is a finding.

## Running

This directory is a cargo workspace of its own and is excluded from the
repository's. `cargo check --manifest-path fuzz/Cargo.toml` works on the
stable toolchain (it builds libFuzzer's C++ runtime, so a C++ compiler is
needed). CI runs it on Linux (the "Fuzz targets compile" step, and again with
the `sonic` feature, see [The sonic backend](#the-sonic-backend)), so that a
change to the SDK's hidden `__internals` seam cannot break the targets
unnoticed, and checks the directory's dependency policy against the
repository's `deny.toml` (`cargo deny --manifest-path fuzz/Cargo.toml check`,
the "Dependency policy of the fuzz targets" step). That policy carries one
crate-scoped license exception: `libfuzzer-sys` may carry NCSA, the license of
the libFuzzer C++ runtime it bundles; no other crate may. CI never runs a
target: that needs the nightly toolchain (for the sanitizer flags cargo-fuzz
passes) and `cargo install cargo-fuzz --locked`:

```sh
cd fuzz
mkdir -p /tmp/finds/decode_response
cargo +nightly fuzz run decode_response /tmp/finds/decode_response corpus/decode_response \
  -- -max_total_time=300 -timeout=10 -rss_limit_mb=2048
```

The first corpus directory is where libFuzzer writes the inputs it finds, so
it is a scratch directory; `corpus/<target>` holds the committed seeds and is
only read. A crash reproducer lands in `artifacts/<target>/`, which is
ignored by git: keep it and report it rather than committing it.

## The sonic backend

The SDK decodes JSON with serde_json unless its `sonic` feature selects
sonic-rs, a SIMD parser that carries `unsafe`; the one finding so far was a
panic inside it. This crate's feature of the same name turns the SDK's on:

```sh
cd fuzz
mkdir -p /tmp/finds/decode_response-sonic
cargo +nightly fuzz run --features sonic decode_response /tmp/finds/decode_response-sonic \
  corpus/decode_response -- -max_total_time=300 -timeout=10 -rss_limit_mb=2048
```

Run a target against the backend through this feature: it is the spelling CI
compiles. Declaring it is what puts sonic-rs and its dependencies into
`Cargo.lock`, at the versions the repository's own lock file has, and
`cargo deny` judges each crate of that tree that a supported target compiles
(`deny.toml` turns on every feature of the crate it checks and limits the
graph to the supported targets). Without the feature,
`--features typesafe-sdk-rust/sonic` builds as well, but cargo resolves that
tree outside the lock file, at the newest versions the index has, even under
`--locked` (seen with cargo 1.98.1), and the dependency policy does not see
it. With the feature declared, both spellings build the locked versions. CI
compiles both builds under `--locked`: "Fuzz targets compile" without the
feature and "Fuzz targets compile with the sonic backend" with it.

With the feature, every decoder `decode_response` drives parses through
sonic-rs instead of serde_json: the System One answers into `Answers` and into
a derived answer set, the models list and the error-body reader, including the
raw JSON they keep and the positions and syntax classification the codec
rebuilds its errors from. A run without it exercises the same decoders over
serde_json, so it is the control. The depth guard and the UTF-8 check are the
codec's own and run before either parser, so both builds cover them alike.
`retry_after` reads no JSON, so the feature changes nothing for it.

## Seeds

`corpus/decode_response` holds the repository's response fixtures
(`tests/fixtures/*.json`, the upstream `RESULT` fixture among them), the live
API's 403 body, the other error-body shapes the message reader knows (a
`detail` list, a nested `error.message`, a string, `null`, non-JSON text), a
small flood of empty score answers, answers whose `type` comes last, escaped
names, brackets inside strings, and nesting at depth 16 (accepted), 17 (the
first depth refused), 2,000 (unclosed) and 1,000 inside an error body.

The deep case is not a 100,000-deep file: the depth guard counts brackets
before the codec sees a byte, so anything past depth 16 is refused at the
same point whatever its depth, and the fuzzer's inputs (4 KiB by default) can
already nest 4,000 levels. The 100,000-deep document of the original finding
is generated at test time by the codec's unit tests (`src/codec_tests.rs`),
which assert that the guard refuses it without parsing it.

The inputs of the one finding so far - a byte that is not UTF-8 inside a
string the decoders keep as raw text, which made the codec panic - are not
in `corpus/`: every tracked file must be UTF-8 text (`text-hygiene.py`), and
these are not by construction. Their exact bytes are the literals of the
regression tests in `src/codec_tests.rs` (`NOT_UTF8`, `fuzzer_find`) and
`tests/malformed_body.rs`; pass a directory holding them as a second corpus
to start a run from them.

`corpus/retry_after` holds counts, floats, exponents, negatives, values past
`u64`, `inf` and `NaN`, the three HTTP date formats (future and past against
the target's fixed clock), padding, and the millisecond header alone and
beside `Retry-After`.
