# Fuzz targets

Four [cargo-fuzz](https://github.com/rust-fuzz/cargo-fuzz) targets for the code
that reads bytes somebody else chose: two for the SDK (a server's response),
two for the adapter in `crates/adapter` (a model's reply, a caller's
questions):

| Target | Input | Property |
| --- | --- | --- |
| `decode_response` | a response body | the depth guard, the System One decoder into `Answers` and into a derived answer set, the models decoder and the error-body reader behind `ApiError` each return `Ok` or `Err`; rendering what they return does not panic |
| `retry_after` | a `retry-after-ms` and/or `Retry-After` value (the first byte picks which, see the target's docs) | the parser returns `None` or a `Duration`, and the same answer twice |
| `adapter_decode` | a question set, a line feed, a model's reply (see [The adapter targets](#the-adapter-targets)) | the adapter's reply decoder returns a count of answers or a message, in both answer modes |
| `adapter_schema` | a question set | the adapter's schema writer returns a schema or an error, in both answer modes, and a schema it returns parses as one JSON object |

A panic, an abort (a stack overflow included), an input that runs past
`-timeout` or a process that passes `-rss_limit_mb` is a finding.

## Running

This directory is a cargo workspace of its own and is excluded from the
repository's. `cargo check --manifest-path fuzz/Cargo.toml` works on the
stable toolchain (it builds libFuzzer's C++ runtime, so a C++ compiler is
needed). CI runs it on Linux (the "Fuzz targets compile" step, and again with
the `sonic` feature, see [The sonic backend](#the-sonic-backend)), so that a
change to the hidden `__internals` seam of the SDK or of the adapter cannot
break the targets unnoticed, and checks the directory's dependency policy against the
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
`retry_after` reads no JSON, so the feature changes nothing for it. The two
adapter targets read JSON through serde_json in either build (the adapter
does not use the SDK's backend for a model's reply or for a schema), so the
feature changes nothing for what they run either.

## The adapter targets

Both call the adapter through its hidden `__internals` module (feature
`internals`, no semver promise), and the adapter is built without its default
features: no provider and no transport is compiled for two pure parsers.

A question set is the JSON object the SDK's `PreparedQuestions::as_json`
writes, one member per question, on one line. `adapter_schema` takes the whole
input as that object. `adapter_decode` splits its input at the first line
feed: the text before it is the question set, the text after it is the reply,
which may hold further line feeds (inside a string, or around a Markdown
fence); an input without a line feed is a question set with an empty reply.
Each target runs its input as probabilities and as discrete answers, and
skips an input that is not UTF-8, since both entry points take text.

The reply decoder finds a member of the reply by a linear scan of the
question names, so its cost is the product of the two counts: 1,000 questions
and 1,000,000 unknown members (11.9 MB) took 5.9 s in an unoptimized build.
No target generates a question set, so both counts come out of one input, and
the input's length bounds them. libFuzzer limits an input to 4,096 bytes
while no seed is longer (the largest seed of `corpus/adapter_decode` is 940
bytes), which is the limit of a run in this README's form and of the
workflow's. `adapter_decode` also skips an input longer than 16,384 bytes, so
a run with a larger `-max_len` stays bounded: a question takes 20 bytes or
more and a reply member 6 or more, so the largest product an input of that
length can hold is about 410 questions against 1,365 members.

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

`target` is `decode_response` (the default), `retry_after`, `adapter_decode`
or `adapter_schema`, `backend` is
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
(`crates/sdk/tests/fixtures/*.json`, the upstream `RESULT` fixture among them),
the live API's 403 body, the other error-body shapes the message reader knows
(a `detail` list, a nested `error.message`, a string, `null`, non-JSON text), a
small flood of empty score answers, answers whose `type` comes last, escaped
names, brackets inside strings, and nesting at depth 16 (accepted), 17 (the
first depth refused), 2,000 (unclosed) and 1,000 inside an error body.

The deep case is not a 100,000-deep file: the depth guard counts brackets
before the codec sees a byte, so anything past depth 16 is refused at the
same point whatever its depth, and the fuzzer's inputs (4 KiB by default) can
already nest 4,000 levels. The 100,000-deep document of the original finding
is generated at test time by the codec's unit tests
(`crates/sdk/src/codec_tests.rs`), which assert that the guard refuses it
without parsing it.

The inputs of the one finding so far - a byte that is not UTF-8 inside a
string the decoders keep as raw text, which made the codec panic - are not
in `corpus/`: every tracked file must be UTF-8 text (`text-hygiene.py`), and
these are not by construction. Their exact bytes are the literals of the
regression tests in `crates/sdk/src/codec_tests.rs` (`NOT_UTF8`,
`fuzzer_find`) and `crates/sdk/tests/malformed_body.rs`; pass a directory
holding them as a second corpus to start a run from them.

`corpus/retry_after` holds counts, floats, exponents, negatives, values past
`u64`, `inf` and `NaN`, the three HTTP date formats (future and past against
the target's fixed clock), padding, and the millisecond header alone and
beside `Retry-After`.

`corpus/adapter_decode` and `corpus/adapter_schema` are derived from the
adapter's recorded fixtures, which are upstream material
(`crates/adapter/LICENSE-THIRD-PARTY`): the 24 vendor cassettes under
`crates/adapter/tests/fixtures/cassettes` (2 recorded tests x 2 answer modes
x native and prompted output x 3 vendors) and the two question sets those
tests ask (`crates/adapter/tests/support/expected.rs`). A seed whose bytes
equal an earlier one is left out, and a seed is named for the first cassette
that gave it: `shape` for the review question set (a noul, a score, a choice),
`probe` for the two-choice one, then the answer mode, the output mode and the
vendor.

`corpus/adapter_decode` holds 17 seeds: a question set as `as_json` writes
it, a line feed, and the reply text of a cassette, byte for byte. Each native
reply and each prompted Gemini and OpenAI reply is accepted in the mode it
was recorded in and refused in the other; the four prompted Anthropic replies
are inside a Markdown fence, which the decoder does not strip (the adapter
strips it before the decoder), so they are refused in both and seed that
path.

`corpus/adapter_schema` holds 8 seeds. `questions-shape` and
`questions-probe` are the two question sets, which the writer accepts in both
modes. The six `schema-` seeds are the schemas the recorded requests carry
(the JSON of a native request's schema member written without white space,
and the schema line of a prompted request's system message): each is a JSON
object that is not a question set, so the writer refuses it, and they give
the fuzzer the words a schema is made of. Four of them are, byte for byte,
what the writer returns for one of the two question sets in one mode.
