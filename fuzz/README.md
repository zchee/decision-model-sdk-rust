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
the libFuzzer C++ runtime it bundles; no other crate may. CI runs a target
only when someone starts it by hand (see
[Fuzzing on x86_64 in CI](#fuzzing-on-x86_64-in-ci)). A run needs the nightly
toolchain (for the sanitizer flags cargo-fuzz passes) and
`cargo install cargo-fuzz --locked`:

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

## Fuzzing on x86_64 in CI

sonic-rs chooses its SIMD code when it is compiled, from the target features
the build turns on; nothing is chosen at run time. A run on Apple silicon or
another arm64 machine therefore compiles its NEON code and never its x86_64
code. The workflow `.github/workflows/fuzz.yaml` fuzzes one target on GitHub's
x86_64 runner (`ubuntu-26.04`) for that code. Its `cpu` input is named for the
flags the build gets, and chooses what the three sonic crates compile:

| `cpu` | Flags | sonic-simd | sonic-number | sonic-rs's parser code |
| --- | --- | --- | --- | --- |
| `baseline` (default) | none | `sse2.rs`, with 256-bit vectors built from pairs of 128-bit ones (`v256.rs`) | portable (`fallback.rs`) | portable (`fallback.rs`) |
| `x86-64-v3` | `-C target-cpu=x86-64-v3` | `avx2.rs` | x86_64 (AVX2) | portable (`fallback.rs`) |
| `x86-64-v3+pclmulqdq` | `-C target-cpu=x86-64-v3 -C target-feature=+pclmulqdq` | `avx2.rs` | x86_64 (AVX2) | x86_64 (PCLMULQDQ and AVX2) |

sonic-rs's own parser code (the mask of what is inside a string, and the skip
over whitespace) is compiled for x86_64 only when `pclmulqdq` is on beside
`avx2` and `sse2`, and the x86-64-v3 level does not include PCLMULQDQ. So
`x86-64-v3` fuzzes the build the SDK's README names to its users, and
`x86-64-v3+pclmulqdq` the sonic modules that `-C target-cpu=native` compiles on
a CPU that has both (a native build turns on more than that, such as `aes`).
AVX-512 is not reachable: sonic-rs compiles its AVX-512 code only under its
own `avx512` feature, which the SDK does not turn on. With `backend` set to
`default`, no sonic-rs code is compiled and the run is the serde_json control.

Before it fuzzes, a run refuses a CPU that lacks one of the target features
the flags turn on (the binary would die with SIGILL, which reads like a
finding), prints the `target_feature` lines rustc gives for the flags, checks
that exactly those flags reach the rustc command lines of the SDK and of the
sonic crates, and puts a table of the modules each sonic crate compiles into
the run's summary. It builds against `Cargo.lock`: `cargo fetch --locked`
first, then with no network.

The workflow never runs on its own: no push, pull request or schedule starts
it. Start it from the Actions tab or with the GitHub CLI:

```sh
gh workflow run fuzz.yaml -f target=decode_response -f backend=sonic -f cpu=x86-64-v3+pclmulqdq -f seconds=900
```

`target` is `decode_response` (the default) or `retry_after`, `backend` is
`sonic` (the default) or `default`, and `seconds` is the fuzzing time, a whole
number from 60 to 3300 (900 by default); the run starts one job per core of
the runner, each for that long. The run's summary names the CPU model and the
nightly, and gives each job's last statistics line, the executions of all jobs
together, and the highest `cov` and `ft` of any job.

A run fails when the fuzzer exits with anything but 0, leaves a reproducer, or
ran on a build that changed `Cargo.lock`. The fuzzer's logs are uploaded as
the artifact `fuzz-logs-<target>-<backend>-<cpu>`, and a reproducer (what a
local run leaves in `artifacts/<target>/`) as
`fuzz-reproducers-<target>-<backend>-<cpu>`; `gh run download <run-id>`
fetches both. Report a reproducer rather than committing it, as for a local
finding.

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
