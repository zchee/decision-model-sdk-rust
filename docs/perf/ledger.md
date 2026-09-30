# Optimization ledger

Every number below comes from a run made on the machine described under **Environment**, with the command printed
above the table that holds it. Nothing here is estimated, and no measurement was repeated until it gave a wanted
answer. Results that contradict the plan are marked **Contradicts the plan** and carry the size of the gap.

The plan is the port's plan, which is not part of this repository; identifiers such as AC-P1, R17, T0.1 and v3.5 name
its acceptance criteria, risks, tasks and revisions.

## Environment

| Item | Value |
| --- | --- |
| Machine | Apple M3 Max, arm64 |
| OS | macOS 27.2 (build 26B5086k), Darwin 27.2.0 |
| Toolchain | `rustc 1.98.1 (48a229cea 2026-09-01)`, `cargo 1.98.1 (797e8a9bc 2026-08-05)` |
| RUSTFLAGS | **cleared** on every invocation (`env -u RUSTFLAGS`); the login shell exports nightly-only `-Z` options and `-C target-cpu=apple-m3`, neither of which may influence a measurement |
| cargo config | `--config ~/.config/rust/config.dev.toml`, which only redirects `build.target-dir`; no profile overrides |
| Date | 2026-09-19 |
| Crates measured | sonic-rs 0.5.10, serde_json 1.0.151 (`float_roundtrip`), serde_path_to_error 0.1.20, dhat 0.3.3, bytes 1.12.1, json-escape-simd 3.1.2, divan 0.1.21, hyper 1.11.1, hyper-util 0.1.20, hyper-rustls 0.27.9, rustls 0.23.45, rustls-platform-verifier 0.7.0, tokio-rustls 0.26.5, h2 0.4.19 |

Measurement rules followed: no benchmark ran in parallel with another benchmark or with a build; no `RUSTFLAGS` and no
`target-cpu`; allocation numbers are taken on the **second identical call**, after one warm-up call of the same shape;
every dhat scenario is its own process, because dhat attributes the whole process to one profiler.

Reproduce everything from a checkout of the tag `v0.1.0` (`git worktree add ../spikes-v0.1.0 v0.1.0`), which keeps the
five spike crates this ledger names (`spikes/sonic-probe`, `spikes/encode-buffer`, `spikes/transport-probe`,
`spikes/alloc-inventory`, `spikes/deps-probe`); none of them is kept on `main`:

```sh
cd spikes/sonic-probe     && env -u RUSTFLAGS cargo --config ~/.config/rust/config.dev.toml build --release
cd spikes/encode-buffer   && env -u RUSTFLAGS cargo --config ~/.config/rust/config.dev.toml build --release
cd spikes/transport-probe && env -u RUSTFLAGS cargo --config ~/.config/rust/config.dev.toml build --release
cd spikes/alloc-inventory  && env -u RUSTFLAGS cargo --config ~/.config/rust/config.dev.toml build --release
```

The binaries land in the shared target directory that `config.dev.toml` names; the commands below write it as
`$TARGET/release/<name>`.

---

## Phase 0 foundation findings

Carried into the ledger so that a reader of this file alone has the transport and dependency facts the spikes build on.
These six were established in T0.1-T0.3.

| # | Finding | Consequence |
| --- | --- | --- |
| F1 | `hyper_rustls::HttpsConnectorBuilder::with_tls_config` asserts that `ClientConfig.alpn_protocols` is empty and aborts the process otherwise; it fills ALPN itself from `enable_http1()`/`enable_http2()` | the SDK's own `ClientConfig` must leave `alpn_protocols` empty |
| F2 | rustls-platform-verifier 0.7.0 has no `tls_config()`; the entry points are `BuilderVerifierExt::with_platform_verifier()` / `ConfigVerifierExt`, and `Verifier::new_with_extra_roots(roots, provider) -> Result<Self, TlsError>` | extra roots need the explicit `Verifier`, not hyper-rustls' own helper |
| F3 | The crypto provider must be named explicitly: `builder_with_provider(Arc::new(rustls::crypto::aws_lc_rs::default_provider()))` | a second provider entering the graph cannot make the process default ambiguous |
| F4 | A cold `http2_only(true)` hyper-util legacy client sent 50 concurrent h2c requests over exactly 1 accepted connection | the TLS `Auto` case was the open question S2b answers below |
| F5 | An rcgen self-signed certificate with SAN `127.0.0.1` works directly as a rustls trust anchor; no CA/leaf pair is needed | the TestServer needs one certificate, not a chain |
| F6 | `cargo deny check` is clean (advisories, bans, licenses, sources) for the workspace and for the full runtime set in `spikes/deps-probe` (a probe crate that is not kept: CI's `cargo deny check` covers the workspace's own graph); duplicate versions are warnings only (`syn` 2.0.119 vs 3.0.6, `getrandom` 0.3.4 vs 0.4.3) | `crates/macros` takes syn 3 in Phase 4 |

---

## S1 - sonic-rs semantics

sonic-rs is the `sonic` feature since 0.2.0, see S7. These measurements remain the historical record.

Binary: `spikes/sonic-probe`. One scenario per process run.

### S1(a) `serde_path_to_error` over `sonic_rs::Deserializer`

```sh
$TARGET/release/sonic-probe a-path
```

| Case | Codec | `error.path()` | `error.inner()` |
| --- | --- | --- | --- |
| `answers.spam.noul` removed | sonic-rs | `answers.spam` | ``missing field `noul` at line 1 column 101`` |
| `answers.spam.noul` removed | serde_json | `answers.spam` | ``missing field `noul` at line 1 column 101`` |
| `models[1].name` is the integer `123` | sonic-rs | `models[1].name` | `invalid type: integer 123, expected a borrowed string at line 1 column 99` |
| `models[1].name` is the integer `123` | serde_json | `models[1].name` | `invalid type: integer 123, expected a borrowed string at line 1 column 99` |

`serde_path_to_error::deserialize(&mut sonic_rs::Deserializer::from_slice(..))` compiles and tracks paths, and reports
the same path as it does over serde_json. Without it, the bare sonic-rs error carries a line/column and a three-line
ASCII caret excerpt of the input, but no path.

**Decision (plan section 5, S1(a) rule: "works -> use it on the failure path for generic `T`"):** use
`serde_path_to_error` on the failure path for generic `T`.

**Contradicts the plan, small:** AC-F6 requires ``field_path == "answers.spam.noul"`` for a missing `noul`. Neither
codec reports that: a *missing field* is reported at the containing struct (`answers.spam`), because that is where
serde raises it; the field name appears in the inner message instead. Both halves are available, so the SDK can render
`answers.spam.noul` by appending the missing field name from the inner error, but AC-F6 cannot be satisfied by
`error.path().to_string()` alone. Either the SDK composes the path, or AC-F6 is restated as
``path == "answers.spam" && inner contains "missing field `noul`"``. This is the next worker's call to make, in
`error.rs`.

**Open question:** the bare sonic-rs error embeds a multi-line excerpt of the input in its `Display`. For a `state`
carrying PII that excerpt would reach the user's logs. `codec.rs` should decide whether to render sonic errors
verbatim or to strip them to code plus position.

### S1(b) borrowed `&'de str` fields and integer map keys

```sh
$TARGET/release/sonic-probe b-borrow
```

| Target | Input | sonic-rs | serde_json |
| --- | --- | --- | --- |
| `&'a str` | `{"s":"friendly"}` | `Ok`, points into the input buffer | `Ok`, points into the input buffer |
| `&'a str` | `{"s":"a\"b\ncé"}` | `Err invalid type: string ..., expected a borrowed string` | same error |
| `Cow<'a, str>` | `{"s":"friendly"}` | `Ok`, borrowed, points into the input | not measured |
| `Cow<'a, str>` | `{"s":"a\"b\ncé"}` | `Ok`, **owned** (the unescaped form is a fresh allocation) | not measured |
| `HashMap<u32, f64>` | `{"0":0.1,"1":0.9}` | `Ok {0: 0.1, 1: 0.9}` | not measured |
| `BTreeMap<u32, f64>` | `{"0":0.1,"1":0.9}` | `Ok {0: 0.1, 1: 0.9}` | `Ok {0: 0.1, 1: 0.9}` |
| `BTreeMap<LevelKey, f64>`, newtype whose visitor parses `&str` | `{"0":0.1,"1":0.9}` | `Ok {LevelKey(0): 0.1, LevelKey(1): 0.9}` | same |

`LevelKey`'s visitor implements both `visit_str` and `visit_u64`; both codecs took `visit_str`, so an object key
reaches the visitor as text and the newtype parses it.

**Decision:** score levels can be stored as integers without allocating, either through `u32` directly or through a
newtype visitor; both work on both codecs. Borrowing a `&'de str` out of the response body is available but only for
values with no escape sequences, so any field that may contain an escape must be `Cow<'de, str>` or owned. Zero-copy
answer names are therefore possible but not unconditional.

### S1(c) constant C for AC-P2

```sh
$TARGET/release/sonic-probe c-borrowed
$TARGET/release/sonic-probe c-ignoredany
$TARGET/release/sonic-probe c-owned
```

Fixture: the upstream `RESULT` (`tests/test_clients.py:42-56`) as compact JSON, 364 bytes. Second identical call.

| Target | blocks | bytes |
| --- | ---: | ---: |
| Allocation-free borrowed probe (`&str`, `f64`, named fields, no `String`/`Vec`/map) | **0** | **0** |
| `serde::de::IgnoredAny` | 0 | 0 |
| Owned control (`String` + `HashMap<String, sonic_rs::Value>`), not a budget, only proof the harness measures | 8 | 1765 |

**C = 0 blocks, 0 bytes.** sonic-rs allocates nothing to decode a 364-byte document into a fully borrowed target, and
its skip path allocates nothing either, so the two do not differ. The owned control shows the counters are live.

**Consequence for AC-P2:** the budget `<= 14 + C` blocks is `<= 14` blocks. Every block in a decode is one the SDK's own
representation asked for; none is the codec's.

### S1(d) nesting

Each shape runs in its own process; the shell records the exit status, because a stack overflow terminates the process
rather than returning an error.

```sh
$TARGET/release/sonic-probe nest <shape> <depth> <stack-kib>   # stack-kib 0 = the main thread (8 MiB on macOS)
```

Depth 100,000, input `[[[...]]]` (200,000 bytes), and the same nested inside `{"legend": ...}`:

| Shape | main thread (8 MiB) | 2 MiB thread |
| --- | --- | --- |
| `sonic_rs::Value` | **SIGABRT, "has overflowed its stack"** | **SIGABRT** |
| `serde::de::IgnoredAny` through sonic-rs | **SIGABRT** | **SIGABRT** |
| `sonic_rs::OwnedLazyValue` | **SIGABRT** | **SIGABRT** |
| the same three inside a `legend` field | **SIGABRT** | **SIGABRT** |
| `serde_json::Value` | `Err recursion limit exceeded at line 1 column 128` | same |
| `IgnoredAny` through serde_json | `Ok` (its skip path is iterative) | same |

Highest depth at which the process still exits normally, found by bisection:

| Shape | release, 2 MiB | release, 8 MiB | dev, 2 MiB | dev, 8 MiB |
| --- | ---: | ---: | ---: | ---: |
| `sonic_rs::Value` | 9,409 | 37,120 | 43 | 175 |
| `IgnoredAny` (sonic skip path) | 8,785 | 34,649 | **24** | 97 |
| `OwnedLazyValue` | 8,784 | 34,647 | 24 | 96 |
| `Value` inside `legend` | 7,318 | 28,870 | 40 | 162 |
| `IgnoredAny` inside `legend` | 8,784 | 34,647 | not measured | not measured |
| `OwnedLazyValue` inside `legend` | 8,782 | 34,645 | not measured | not measured |

Stack consumed per nesting level, from the same bisection at four stack sizes (`IgnoredAny` shape):

| Stack | dev max depth | dev bytes/level | release max depth | release bytes/level |
| ---: | ---: | ---: | ---: | ---: |
| 512 KiB | 6 | 87,381 | 2,232 | 234 |
| 1 MiB | 12 | 87,381 | 4,416 | 237 |
| 2 MiB | 24 | 87,381 | 8,785 | 238 |
| 4 MiB | 49 | 85,598 | not measured | |
| 8 MiB | 97 | 86,480 | 35,000 | 239 |

**Result: sonic-rs 0.5.10 has no effective recursion limit on any path the SDK uses.** Its serde `Deserializer` does
carry one (`src/serde/de.rs:23`, `MAX_ALLOWED_DEPTH: u8 = u8::MAX`), but `deserialize_ignored_any` calls
`parser.skip_one(true)` directly (`src/serde/de.rs:889`) and the `Value` and `OwnedLazyValue` paths have parsers of
their own, so none of the three passes through `with_depth_limit`. The failure mode is `SIGABRT`, not an `Err`: it
cannot be caught, and it takes the whole process, not the request.

**Decision (plan section 5, S1(d) rule: "if the skip path or `OwnedLazyValue` recurses without a limit, cap depth in
`codec.rs` and add a fuzz seed"): `codec.rs` must cap depth, and the cap must be enforced BEFORE the bytes reach
sonic-rs.** A depth counter inside a serde `Visitor` cannot help, because the overflow happens inside sonic's own
parser and the visitor is never re-entered. The workable guard is an O(n) pre-scan of the response bytes that counts
`[`/`{` against `]`/`}` outside string literals and rejects the body above the cap. The 100,000-deep array is a fuzz
seed.

**Contradicts the plan, large, and it constrains the test suite as well as the SDK:** in an unoptimized build - which
is what `cargo test`, `cargo nextest` and `cargo fuzz` produce - the skip path costs ~85 KiB of stack per level and
survives only **24 levels on a default 2 MiB tokio worker thread**. A fuzz target seeded with a 100,000-deep array, as
plan section 7 R12 prescribes, will abort immediately in a debug build; so will any test that decodes a document more
than ~24 deep on a worker thread. The depth cap has to be low enough to be enforced before sonic sees the bytes in
every profile, and the alloc/fuzz harnesses need either a release profile or an explicit larger stack.

**Cap decided: `MAX_JSON_DEPTH = 16`**, enforced by the pre-scan in `codec.rs`. 16 sits below the lowest ceiling in the
tables above - the 24 levels an unoptimized skip path survives on a 2 MiB tokio worker - so the depth the SDK accepts is
itself within reach of every profile, and the pre-scan's position ahead of sonic-rs is a second line of defence rather
than the only one. serde_json's limit of 128 was the obvious value to borrow and is rejected for that reason: it exceeds
the 24-level ceiling, so a body between 25 and 128 deep would pass the cap and still abort a dev-profile worker.

### S1(e) appending into a non-empty `Vec<u8>`

```sh
$TARGET/release/sonic-probe e-append
```

Starting from a 9-byte prefix `{"state":`, two `sonic_rs::to_writer(&mut buffer, ..)` calls and two literal splices
produced `{"state":"a \"quoted\" state","model":"jev-latest"}`, which serde_json re-parses into the expected object.
`to_writer` appends; it does not clear.

**Decision:** confirmed. The body splice of plan section 3.3 step 1 and the scratch buffer of S6 can both write through
`&mut Vec<u8>` (the blanket `impl<W: WriteExt + ?Sized> WriteExt for &mut W`, `src/writer.rs:138`).

### S1(f) float parsing

```sh
$TARGET/release/sonic-probe f-float
```

`f64::to_bits` of each parse:

| Literal | sonic-rs | `str::parse` | serde_json | agree |
| --- | --- | --- | --- | --- |
| `0.1` | `0x3fb999999999999a` | `0x3fb999999999999a` | `0x3fb999999999999a` | yes |
| `1e-7` | `0x3e7ad7f29abcaf48` | same | same | yes |
| `0.30000000000000004` | `0x3fd3333333333334` | same | same | yes |
| `5e-324` | `0x0000000000000001` | same | same | yes |
| `-0.0` | `0x0000000000000000` | `0x8000000000000000` | `0x8000000000000000` | **NO** |
| `-0` | `0x0000000000000000` | `0x8000000000000000` | `0x8000000000000000` | **NO** |
| `-0.0e5` | `0x0000000000000000` | `0x8000000000000000` | `0x8000000000000000` | **NO** |
| `-1e-400` (underflow) | `0x8000000000000000` | same | same | yes |
| `1e-400` (underflow) | `0x0000000000000000` | same | same | yes |
| `1.7976931348623157e308` | `0x7fefffffffffffff` | same | same | yes |
| 30-digit mantissa `1.234567890123456789012345678901` | `0x3ff3c0ca428c59fb` | same | same | yes |
| 30-digit integer `123456789012345678901234567890` | `0x45f8ee90ff6c373e` | same | same | yes |
| `1e309` | `Err "Float number must be finite"` | `inf` | `Err` | both codecs reject |
| `-1e309` | `Err` | `-inf` | `Err` | both codecs reject |
| `0.000000000000000000000000000001` | `0x39b4484bfeebc2a0` | same | same | yes |

The same divergence inside a document, not only as a bare scalar:

| Input | sonic-rs | serde_json |
| --- | --- | --- |
| `{"s":-0.0}` into `struct { s: f64 }` | bits `0x0000000000000000` | bits `0x8000000000000000` |
| `{"s":-0.0}` re-rendered through a `Value` | `{"s":0.0}` | `{"s":-0.0}` |
| `[-0.0]` re-rendered through a `Value` | `[0.0]` | `[-0.0]` |

**Result: sonic-rs 0.5.10 drops the sign of a literal negative zero.** `-0.0`, `-0` and `-0.0e5` all decode to `+0.0`.
The sign is preserved when the value reaches zero by *underflow* (`-1e-400` gives `-0.0` correctly), so the bug is in
the literal-zero path, not in the sign handling generally. Both codecs reject values outside f64 range rather than
returning an infinity, and agree on every other case measured, including subnormals, the 30-digit mantissa and both
underflows.

**Contradicts the plan, small but blocking for one criterion:** AC-Q3 compares decoded numbers with serde_json
`float_roundtrip` by `f64::to_bits` and names `-0.0` as one of the generated forms. That comparison fails today. The
next worker must add negative zero to AC-Q3's enumerated list of accepted divergences, or AC-Q3 cannot pass. For the
API itself the impact is nil - `noul`, `confidence`, `score` and probabilities are never negative zero in a meaningful
sense - but it must be written down rather than discovered by a failing test.

---

## S5 - how dhat accounts for re-allocation

```sh
$TARGET/release/sonic-probe s5-realloc
```

A `Vec<u8>` grown through a ladder of sizes, with the pointer compared before and after each step:

| Step | blocks | bytes | curr_bytes | pointer moved |
| --- | ---: | ---: | ---: | --- |
| `vec![0u8; 1024]` | 1 | 1,024 | +1,024 | n/a (fresh) |
| grow 1,024 -> 4,096 | 1 | 4,096 | +3,072 | yes |
| grow 4,096 -> 65,536 | 1 | 65,536 | +61,440 | yes |
| grow 65,536 -> 1,048,576 | 1 | **1,048,576** | +983,040 | **no** |
| grow 1,048,576 -> 8,388,608 | 1 | 8,388,608 | +7,340,032 | yes |
| `shrink_to(1024)` while `len == capacity == 8,388,608` (a no-op in effect) | 1 | **8,388,608** | +0 | no |
| `truncate(1024)` + `shrink_to_fit()` from capacity 8,388,608 | 1 | **1,024** | -8,387,584 | yes |

**Conclusion, in one sentence: dhat charges every allocator call, including a `realloc` that keeps the pointer and even
one that does not change the size, as one block whose `total_bytes` contribution is the FULL NEW SIZE rather than the
growth delta - so AC-P1's rule "bytes allocated during the call <= 1.25x body length + 64 KiB" does catch a
re-allocation, because a buffer that grows from n to m inside the call contributes m, not m - n.**

Two details the next worker needs: `total_bytes` of a shrink is the new size, not the old, so a shrink is cheap in the
byte bound but still costs one block; and `curr_blocks` stays at +0 across a realloc, so a budget expressed in
`curr_blocks` would not see re-allocation at all.

---

## S6 - body encode buffer

sonic-rs is the `sonic` feature since 0.2.0, see S7. The shared buffer policy is unchanged.

Binary: `spikes/encode-buffer`. Body shape
`{"state":<state>,"model":"jev-latest","questions":<297 bytes of pre-serialized JSON>}`. States: English-like text with
straight quotes, newlines and multi-byte characters (`é`, `€`, `—`) at 1 KB / 64 KB / 1 MB, and an object
`{"subject": <64 B>, "body": <1 MB>}`.

### Variants

| Name | Strategy |
| --- | --- |
| `i` | one fresh `Vec<u8>` per call, pre-sized from the previous call's post-encode `capacity()` |
| `ii` | exact-size two-pass: measure the escaped length, allocate exactly that, write once with a hand-written scalar escaper. Object states fall through to `iii` |
| `ii-simd` | `ii` with `json_escape_simd::escape_into` in place of the scalar escaper |
| `iii` | thread-local retained scratch, taken out of the thread-local (never borrowed across the caller's `Serialize`), decayed hint `hint = max(len, hint - hint/16)`, shrink to the `8 * hint` ceiling, then an exact copy into a right-sized body buffer |
| `iii-1shot` | `iii`, but the shrink goes in ONE step down to `hint` instead of down to the ceiling |
| `iv` | `ii` for string states, `iii` for object states |
| `iv-1shot` | `ii` for string states, `iii-1shot` for object states |

### Correctness first

```sh
$TARGET/release/encode-buffer verify
```

All seven variants produce byte-identical bodies for all four states. The scalar escaper's output equals
`sonic_rs::to_string` for the English-like filler and for `"`, `\`, `/`, every control character in
U+0000-U+001F, U+007F (DEL), U+00A0 (no-break space), U+2028 (line separator), U+2029 (paragraph separator),
the astral character U+1F600 and the non-ASCII sample `é€—` (U+00E9, U+20AC, U+2014); `escaped_len` predicts the
exact written length in every case. `json_escape_simd` also matches sonic-rs byte for byte.

Those characters are named in notation rather than written out: the corpus itself lives in
`spikes/encode-buffer/src/main.rs`, and putting raw control bytes in a Markdown file turns it into something
`file` calls data, git diffs as binary and grep skips.

### `json-escape-simd`: can it write into a caller-provided buffer?

**Yes, but not into an exactly-sized one, which is what variant (ii) needs.** `escape_into(value, dst)` appends to a
caller `Vec<u8>`, but its first statement is `dst.reserve(value.len() * 6 + 32 + 3)`, because its SIMD kernels perform
speculative full-register stores past the logical end (escape-simd 0.1.0 `src/json.rs:18-25`, with that reasoning in
its own comment). That is the same `len * 6 + 35` reservation the variant exists to avoid, so the crate cannot serve as
an exact-size escaper. It is measured below to show that, not because it can work.

### Variant x state, second identical call

```sh
for v in i ii ii-simd iii iii-1shot iv iv-1shot; do
  for s in s1k s64k s1m obj1m; do $TARGET/release/encode-buffer variant "$v" "$s"; done
done
```

| Variant | State | body bytes | blocks | bytes allocated | bytes / body | retained capacity | hint | retained / hint |
| --- | --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| i | 1 KB text | 1,385 | 2 | 6,212 | 4.49x | 0 | 6,188 | - |
| i | 64 KB text | 67,121 | 2 | 393,284 | 5.86x | 0 | 393,260 | - |
| i | 1 MB text | 1,068,796 | 2 | 6,291,524 | 5.89x | 0 | 6,291,500 | - |
| i | 1 MB object | 1,068,884 | 2 | 6,291,611 | 5.89x | 0 | 6,291,587 | - |
| ii | 1 KB text | 1,385 | **1** | **1,385** | **1.00x** | 0 | 0 | - |
| ii | 64 KB text | 67,121 | 1 | 67,121 | 1.00x | 0 | 0 | - |
| ii | 1 MB text | 1,068,796 | 1 | 1,068,796 | 1.00x | 0 | 0 | - |
| ii | 1 MB object (falls through to iii) | 1,068,884 | 1 | 1,068,884 | 1.00x | 6,291,587 | 1,068,884 | 5.89x |
| ii-simd | 1 KB text | 1,385 | 3 | 7,597 | 5.49x | 0 | 0 | - |
| ii-simd | 64 KB text | 67,121 | 3 | 460,405 | 6.86x | 0 | 0 | - |
| ii-simd | 1 MB text | 1,068,796 | 3 | 7,360,320 | 6.89x | 0 | 0 | - |
| ii-simd | 1 MB object | 1,068,884 | 1 | 1,068,884 | 1.00x | 6,291,587 | 1,068,884 | 5.89x |
| iii | 1 KB text | 1,385 | **1** | **1,385** | **1.00x** | 6,188 | 1,385 | 4.47x |
| iii | 64 KB text | 67,121 | 1 | 67,121 | 1.00x | 393,260 | 67,121 | 5.86x |
| iii | 1 MB text | 1,068,796 | 1 | 1,068,796 | 1.00x | 6,291,500 | 1,068,796 | 5.89x |
| iii | 1 MB object | 1,068,884 | 1 | 1,068,884 | 1.00x | 6,291,587 | 1,068,884 | 5.89x |
| iv | 1 KB text | 1,385 | 1 | 1,385 | 1.00x | 0 | 0 | - |
| iv | 64 KB text | 67,121 | 1 | 67,121 | 1.00x | 0 | 0 | - |
| iv | 1 MB text | 1,068,796 | 1 | 1,068,796 | 1.00x | 0 | 0 | - |
| iv | 1 MB object | 1,068,884 | 1 | 1,068,884 | 1.00x | 6,291,587 | 1,068,884 | 5.89x |

`iii-1shot` and `iv-1shot` are identical to `iii` and `iv` on a single repeated size; they differ only in the
mixed-size sequence below.

AC-P1 asks for `<= 2` blocks, bytes allocated `<= 1.25 x body + 64 KiB`, and retained scratch `<= 8x` the decayed hint,
for BOTH string and object states:

| Variant | blocks <= 2 | bytes within bound | retained <= 8x hint | meets AC-P1 |
| --- | --- | --- | --- | --- |
| i | yes (2) | **no**: 393,284 > 149,437 at 64 KB, 6,291,524 > 1,401,531 at 1 MB | n/a | **no** |
| ii | yes (1) | yes (1.00x) | yes (5.89x) | **yes** |
| ii-simd | **no (3)** | **no** (6.89x at 1 MB) | yes | **no** |
| iii | yes (1) | yes (1.00x) | yes (4.47-5.89x) | **yes** |
| iii-1shot | yes (1) | yes | yes | **yes** |
| iv | yes (1) | yes | yes | **yes** |
| iv-1shot | yes (1) | yes | yes | **yes** |

Variant `i` is the one the 6x reservation was expected to sink, and it does: the hint that tracks post-encode
`capacity()` removes the re-allocation (2 blocks, not 3) but the buffer it allocates is the 6x one, so every call
allocates ~5.9x the body it sends.

### Mixed-size sequence: 1 MB, then sixteen 1 KB calls

```sh
$TARGET/release/encode-buffer mixed <variant>
```

| Call | State | `i` blocks / bytes | `iii` blocks / bytes / retained | `iii-1shot` blocks / bytes / retained | `iv` blocks / bytes / retained |
| ---: | --- | --- | --- | --- | --- |
| 0 | 1 MB | 3 / 6,291,533 | 3 / 7,360,305 / 6,291,500 | 3 / 7,360,305 / 6,291,500 | 1 / 1,068,796 / 0 |
| 1 | 1 KB | 2 / 6,291,524 | 1 / 1,385 / 6,291,500 | 1 / 1,385 / 6,291,500 | 1 / 1,385 / 0 |
| 2 | 1 KB | 2 / 6,291,524 | 1 / 1,385 / 6,291,500 | 1 / 1,385 / 6,291,500 | 1 / 1,385 / 0 |
| 3 | 1 KB | 2 / 6,291,524 | 1 / 1,385 / 6,291,500 | 1 / 1,385 / 6,291,500 | 1 / 1,385 / 0 |
| 4 | 1 KB | 2 / 6,291,524 | 1 / 1,385 / 6,291,500 | 1 / 1,385 / 6,291,500 | 1 / 1,385 / 0 |
| 5 | 1 KB | 2 / 6,291,524 | **2 / 6,193,553** / 6,192,168 | **2 / 775,406** / 774,021 | 1 / 1,385 / 0 |
| 6 | 1 KB | 2 / 6,291,524 | **2 / 5,806,545** / 5,805,160 | 1 / 1,385 / 774,021 | 1 / 1,385 / 0 |
| 7 | 1 KB | 2 / 6,291,524 | **2 / 5,443,729** / 5,442,344 | 1 / 1,385 / 774,021 | 1 / 1,385 / 0 |
| 8 | 1 KB | 2 / 6,291,524 | 2 / 5,103,585 / 5,102,200 | 1 / 1,385 / 774,021 | 1 / 1,385 / 0 |
| 9 | 1 KB | 2 / 6,291,524 | 2 / 4,784,705 / 4,783,320 | 1 / 1,385 / 774,021 | 1 / 1,385 / 0 |
| 10 | 1 KB | 2 / 6,291,524 | 2 / 4,485,753 / 4,484,368 | 1 / 1,385 / 774,021 | 1 / 1,385 / 0 |
| 11 | 1 KB | 2 / 6,291,524 | 2 / 4,205,481 / 4,204,096 | 1 / 1,385 / 774,021 | 1 / 1,385 / 0 |
| 12 | 1 KB | 2 / 6,291,524 | 2 / 3,942,729 / 3,941,344 | 1 / 1,385 / 774,021 | 1 / 1,385 / 0 |
| 13 | 1 KB | 2 / 6,291,524 | 2 / 3,696,401 / 3,695,016 | 1 / 1,385 / 774,021 | 1 / 1,385 / 0 |
| 14 | 1 KB | 2 / 6,291,524 | 2 / 3,465,465 / 3,464,080 | 1 / 1,385 / 774,021 | 1 / 1,385 / 0 |
| 15 | 1 KB | 2 / 6,291,524 | 2 / 3,248,961 / 3,247,576 | 1 / 1,385 / 774,021 | 1 / 1,385 / 0 |
| 16 | 1 KB | 2 / 6,291,524 | 2 / 3,045,993 / 3,044,608 | 1 / 1,385 / 774,021 | 1 / 1,385 / 0 |

**Contradicts the plan, and it is a defect in the rule rather than in the code:** the decay-and-shrink rule as plan
section 3.3 step 1 states it - "shrunk when it exceeds 8x the recent body size", with the hint decaying a sixteenth per
call - re-allocates the whole scratch **on every call** from the moment the condition first holds. In the sequence
above, variant `iii` allocates between 3.0 MB and 6.2 MB per 1 KB request for twelve consecutive calls, and it would
keep doing so for dozens more. The cause is that shrinking exactly to the ceiling leaves the capacity at the ceiling
while the hint keeps decaying, so the condition holds again on the next call.

Shrinking in ONE step down to the hint (`iii-1shot`) fixes it: one re-allocation of 775,406 bytes at call 5, then
1 block / 1,385 bytes for every call after it. The condition only becomes true again after the hint has decayed by
another factor of eight, so the number of re-allocations after an outlier is logarithmic rather than linear.

Variant `i` never shrinks at all: it keeps allocating 6,291,524 bytes for a 1,385-byte body indefinitely, because its
hint tracks `capacity()` and therefore never decays.

### Body-type conversion

```sh
$TARGET/release/encode-buffer bytes-convert
```

Second identical shape, for a 1 KiB and a 1 MiB payload (both sizes gave identical counts):

| Source | conversion blocks / bytes | first `clone()` blocks / bytes | second `clone()` blocks |
| --- | ---: | ---: | ---: |
| `Bytes::from(Vec)`, `len == capacity` | 0 / 0 | **1 / 24** | 0 |
| `Bytes::from(Vec)`, `len != capacity` (a codec-grown buffer) | **1 / 24** | 0 / 0 | 0 |
| `BytesMut::freeze()`, `len == capacity` | 0 / 0 | **1 / 24** | 0 |

**Confirms plan section 3.3 step 5:** the first `clone()` of a `Vec`-backed `Bytes` costs 1 block of 24 bytes (the
shared header), and skipping the clone when `max_retries == 0` saves exactly that. Handing over an exactly-full buffer
is free; handing over a buffer with slack pays the same 24 bytes immediately, whether or not it is ever cloned, which
is a second reason for the body buffer to be exactly sized.

### Wall clock (report-only)

```sh
cd spikes/encode-buffer && env -u RUSTFLAGS cargo --config ~/.config/rust/config.dev.toml bench
```

divan 0.1.21, timer precision 41 ns, 100 samples each, no RUSTFLAGS, run alone. Median of a second identical call:

| Variant | 1 KB text | 64 KB text | 1 MB text | 1 MB object |
| --- | ---: | ---: | ---: | ---: |
| i | 187.2 ns | 9.415 µs | 152.9 µs | 152.7 µs |
| ii (scalar two-pass) | 1.853 µs | 115.9 µs | **1.874 ms** | 165.1 µs |
| ii-simd | 780.9 ns | 44.24 µs | 743.3 µs | 165.0 µs |
| iii | **187.2 ns** | **10.62 µs** | **165.2 µs** | 165.7 µs |
| iii-1shot | 187.2 ns | 10.58 µs | 166.7 µs | 165.5 µs |
| iv | 1.874 µs | 115.7 µs | 1.920 ms | 168.1 µs |
| iv-1shot | 1.937 µs | 115.5 µs | 1.939 ms | 165.5 µs |

No acceptance criterion depends on these; they are here because they change the recommendation. The hand-written
scalar escaper is **11x slower** than sonic-rs on a 1 MB string (1.874 ms against 165.2 µs) and **10x slower** on 1 KB
(1.853 µs against 187.2 ns); `json-escape-simd` is faster than the scalar loop but still 4.5x slower than routing the
same string through sonic-rs into a pre-grown buffer.

### S6 decision

The plan's rule: among the variants meeting AC-P1's bounds for both string and object states, the tie-break on this
macOS machine is bytes allocated per call, because instruction counts need Linux and valgrind.

Variants meeting the bounds: `ii`, `iii`, `iii-1shot`, `iv`, `iv-1shot`. **All five allocate exactly the body length
per call (1.00x), so the stated tie-break does not separate them.** The rule is therefore exhausted, and the
recommendation below rests on the next measurements in the table, stated explicitly rather than folded in silently:

**Recommended winner: `iii-1shot` - the thread-local retained scratch with a decaying hint and a one-shot shrink.**

- It is the fastest variant at every state size (187 ns / 10.6 µs / 167 µs / 166 µs), tied with `i` and 10-11x ahead of
  every two-pass variant, because the state string goes through sonic-rs' SIMD writer instead of a scalar loop.
- It allocates 1 block of exactly the body length per call, the joint best in the table.
- It needs no second escaper implementation. `ii` and `iv` require a hand-written escaper that must stay byte-identical
  to the codec for ever - the sort of thing AC-F7(a) already has to prove for the derive macro - and it buys nothing
  that `iii-1shot` does not already have.
- The `-1shot` shrink is a departure from the plan's wording and is required: see the mixed-size table.

Cost of the recommendation, stated plainly: `iii-1shot` retains up to ~6x the largest recent body per runtime worker
thread (6.29 MB after a 1 MB state), where `iv` retains nothing for string states. The decay plus the one-shot shrink
bounds that over time and it satisfies AC-P1's `<= 8x hint` rule, but on a 16-worker runtime a burst of 1 MB states
leaves ~100 MB retained until the hint decays. If that matters more than 11x the encode CPU, `iv-1shot` is the
alternative.

**Open question for the freeze worker:** an absolute ceiling on the retained scratch (drop it entirely when a body
exceeded, say, 256 KiB) was not measured. It would bound the retained memory without the 11x CPU of the two-pass path,
and it is a cheaper experiment than either variant here.

**No variant failed the bounds for reasons that implicate D2.** sonic-rs' 6x reservation is fully absorbed by a
retained scratch plus an exact copy, so the escalation clause of AC-P1 ("if no sonic-rs based variant meets these
bounds, execution STOPS") is not triggered.

---

## S2a - TLS, ALPN and extra roots

Binary: `spikes/transport-probe`. The live endpoint is contacted **without an API key**; 403 is the expected answer and
no credential is read, set or sent anywhere in this crate.

```sh
cd spikes/transport-probe && env -u RUSTFLAGS cargo --config ~/.config/rust/config.dev.toml build --release
$TARGET/release/transport-probe s2a
```

The client is built the way the SDK will build it: hyper-util legacy `Client` over a hyper-rustls connector
(`default-features = false`, features `http1`, `http2`, `tls12`, `aws-lc-rs`) with a `rustls::ClientConfig` of this
crate's own, `builder_with_provider(Arc::new(rustls::crypto::aws_lc_rs::default_provider()))`, the platform verifier,
and `alpn_protocols` left **empty**.

| # | Case | Result |
| --- | --- | --- |
| 1 | `GET https://api.typesafe.ai/v1/models`, no `Authorization`, plain platform verifier | status **403**, version **HTTP/2.0** |
| 3 | loopback TLS TestServer, **no** extra root | handshake refused: `invalid peer certificate: Other(OtherError("“rcgen self signed cert” certificate is not trusted: -67843"))`; the server accepted the TCP connection (+1) and the client dropped it |
| 2 | loopback TLS TestServer, extra root = the server's certificate | status **200**, version **HTTP/2.0**, 1 connection |
| 2 | the SAME extra-roots config against the live API | status **403**, version **HTTP/2.0** |

Response headers of the unauthenticated live call, verbatim:

```
content-length: 118
content-type: application/json
date: Fri, 18 Sep 2026 19:06:33 GMT
server: istio-envoy
x-envoy-upstream-service-time: 7
x-typesafe-request-id: req_01a0b5e9ce0b7e9394746c6818917706
```

Body: `{"detail":{"error_type":"authentication_error","message":"Must supply an API key! Check your request and try again."}}`

**Decision:** the configuration works. ALPN negotiates h2 against both the live API and the loopback server;
`Verifier::new_with_extra_roots` ADDS to the operating system store rather than replacing it, which is what
`ClientBuilder::add_root_certificate` needs; and a client that has not added the root is refused, so the negative case
is a real check and not a silent pass.

**Stated explicitly: this proves `add_root_certificate` on macOS only. Linux and Windows remain unproven until CI can
run them**, and rustls-platform-verifier takes a different code path on each (`src/verification/apple.rs`,
`.../linux.rs`, `.../windows.rs`). The Phase 0 exit gate says "CI green on 3 OSes"; CI later closed that part of the
gate on all three.

Two facts worth carrying into Phase 2: the request-id header is spelled **`x-typesafe-request-id`**, and the 403 body
matches AC-F4's expected `error_type` and message exactly, so that criterion is confirmed against the live server
rather than against the documentation.

## S2b - cold fan-out connection count

```sh
$TARGET/release/transport-probe s2b
```

64 concurrent `GET` requests against the HTTP/2 + TLS TestServer, 10 repetitions, each repetition on a **brand-new**
client. The base URL is an IP literal, so no second socket is raced across resolved addresses. Connections are counted
twice independently: by the server at accept time, and inside the client by a connector wrapper whose counter goes up
when a stream is produced and down when it is dropped. The two counts agree in every row.

| Case | warm | min | median | max | client opened | live 1 s later |
| --- | --- | ---: | ---: | ---: | ---: | ---: |
| A: `Auto` (`enable_http1().enable_http2()`, pool default) | no | **64** | **64** | **64** | 64 | 1 |
| A: `Auto` | yes | 0 | 0 | 0 | 1 | 1 |
| B: `http2_only(true)`, ALPN h2 only | no | **1** | **1** | **1** | 1 | 1 |
| B: `http2_only(true)` | yes | 0 | 0 | 0 | 1 | 1 |

min/median/max are the connections the server accepted for the 64-way fan-out itself; "warm = yes" means one request
completed first, and then the 64 opened **zero** new connections in both modes. 2,580 requests were served in total
across the four cases.

The **minimum of case A cold varies between runs**: 64 in the run tabulated above, 3 in the lead's re-run of the same
binary (median 64, max 64, client_opened 64 in both). It depends on how far the first handshake gets before the other
63 tasks are polled, which is a scheduling race. The decision does not rest on it: what matters is that A's median and
maximum are 64 while B was 1/1/1 in both runs.

B's 1/1/1 was two runs of 10 repetitions on an idle macOS machine, and it was not the whole picture: on Linux,
hyper-util's own client opened a second connection in 5 of 4,000 cold bursts on 4 idle CPUs and in 174 of 2,000 on 2
CPUs, and the SDK's cold fan-out in up to 16.73 % of them, which S2c measures and explains.

**Decision (plan section 5, S2b rule: "if `http2_only` yields 1 and `Auto` yields > 1, the default for `https` base
URLs is `Http2Only`, with `Auto` as the documented knob"): the rule fires exactly. `Http2Only` is the default for
`https` base URLs; `Auto` is the documented knob for HTTP/1.1-only proxies; `http://` base URLs always use `Auto`.**

Two details for Phase 2. Under `Auto` the pool does collapse to a single connection afterwards - 64 opened, 1 alive a
second later - so the cost is 64 TLS handshakes on the cold path, not 64 connections held. And AC-P4(c) is already
visible here: after one warm-up request, 64 concurrent calls open zero new connections under either mode, which is why
`warm_up()` is worth documenting before a fan-out even once `Http2Only` is the default.

## S2c - the cold fan-out race

`crates/sdk/tests/connection_reuse.rs` `a_cold_fan_out_opens_one_connection` (64 calls started together on a client with no
connection, HTTP/2 over TLS, `Http2Only`) failed CI twice on `ubuntu-26.04`, and never on macOS. The first table below
is the run that found the cause, before the change; the second measures the change against the code before it. Both ran
on 2026-09-28, the test's body in a loop, each iteration with a new server, a new client and a new 4-worker runtime.
Linux is a Docker Desktop linux/arm64 container on an Apple M3 Max host, with its CPUs limited by `--cpuset-cpus`; "8
processes" is eight such loops at once in that container.

| system | CPUs | load | cold bursts | with 2 or more connections | rate |
| --- | ---: | --- | ---: | ---: | ---: |
| Linux | 4 | idle | 3,000 | 4 | 0.13 % |
| Linux | 4 | 8 processes | 4,000 | 420 | 10.5 % |
| Linux | 4 | 3 bodies in one process, as libtest runs a binary's tests | 3,000 | 409 (7 of them with 3) | 13.6 % |
| Linux, coverage-instrumented | 4 | idle | 2,000 | 89 | 4.45 % |
| Linux | 2 | idle | 2,000 | 169 | 8.45 % |
| macOS | 16 | idle | 3,000 | 0 | 0 |
| macOS | 16 | 48 processes | 4,800 | 3 | 0.06 % |
| Linux and macOS, warm pool (100 calls one after another, then 100 at once): Linux 4 CPUs idle and with 8 processes, coverage-instrumented idle and with 8 processes, Linux 2 CPUs idle and with 4 processes, macOS idle and with 24 processes | | | 17,200 | 0 | 0 |

The change against the code before it: the same harness, the two builds run in alternating blocks inside one container
per configuration, cold bursts of 64 calls with 2 or more connections, no call failed in any run. The table is the gate
as it ships.

| configuration | before the change | with the gate |
| --- | ---: | ---: |
| 4 CPUs, 3 bodies in one process | 502 / 3,000 (16.73 %) | 0 / 3,000 |
| 4 CPUs, 8 processes | 469 / 4,000 (11.72 %) | 0 / 4,000 |
| 2 CPUs, idle | 106 / 2,000 (5.30 %) | 0 / 2,000 |
| all three | 1,077 / 9,000 | 0 / 9,000 |

Three such runs were made that day, the last with the gate as it ships and two with earlier forms of it whose success
path was the same: 0 cold bursts with a second connection in 29,000 with the gate, against 3,306 before it. One of them
also ran the body instrumented for coverage: 0 / 2,000 with the gate, and 0 / 2,000 before it (25 / 2,000 before it in
an earlier run).

hyper-util alone, without the SDK, did the same: 2 connector calls in 5 of 4,000 cold bursts on 4 idle Linux CPUs, and a
second connection in 174 of 2,000 (8.70 %) on 2 CPUs. The rate follows the machine's scheduling, not the code path: the
retry policy made no measurable difference (33 against 42 in 4,000 interleaved bursts each), and tracing raised it (16
against 6 in 4,000).

**What the second connection was.** The client's own: from the next ephemeral port, a completed TLS handshake with
ALPN `h2`, 0 requests, and closed by the client a median 48 µs (at most 2.1 ms, over 101 records) after its handshake.
No call failed. hyper-util 0.1.20's pool lets one task at a time connect to an HTTP/2 host (`pool.rs` `connecting`),
and each request polls its pool checkout and then its lazy connect in one `select` (`client.rs`
`one_connection_for`). A task that registered as a waiter before the first connection was pooled, and whose connect
was polled after `pooled` had released the connecting lock, found the lock free and connected again - in 13 of 16
traces because its thread made no progress for 1.5 to 10.4 ms inside one poll, which is why contention raises the
rate. The pool keeps one HTTP/2 connection per host (`put; existing idle HTTP/2 connection`), so it dropped the spare,
but the server had counted it at `accept()`. The cost is one TCP and one TLS handshake.

**What the change does.** Under `Http2Only` the transport now holds every request but one at a gate while its pool
holds no connection: no request was given a pooled connection since the last connect began, which hyper-util reports
through `capture_connection`, or no stream the connector made is still open, which the transport counts at the socket.
The one request (the opener) goes to hyper-util with `capture_connection` attached; hyper-util fills it after the new
connection is in the pool, and the gate then releases every waiter at once, so each one's checkout finds the connection
on its first poll and its connect is never started. When the opener's turn ends without a connection - its connect
failed (TCP, TLS or the HTTP/2 handshake after it), or it was dropped before a connection existed (its caller's
deadline, a cancelled task; S2d replaces this case: the connect now outlives the request that started it) - every
waiting request fails with it, as before the change; an opener whose response
fails on a connection passes the turn to one waiter instead, and a call's deadline covers its wait. A request
sent while the pool holds a connection pays two atomic loads; one sent while it does not pays a boxed future and a
clone of the client. `Auto` is unchanged. Every call future over the default transport grew by 16 bytes (the gate's
reference count in the transport and in its connector): 2,344 / 2,328 / 2,328 / 2,032 became 2,360 / 2,344 / 2,344 /
2,048, measured on macOS arm64 in both profiles, inside the bounds of `crates/sdk/tests/static_assertions.rs`. The transport's
three settings (the HTTP version, the count of added roots, the connect timeout) then moved behind the `Arc` it already
held for the gate (`e08280c`), which took 32 bytes back: 2,328 / 2,312 / 2,312 / 2,016, measured on macOS arm64 and
Linux arm64 in both profiles, with the bounds, the futures over a custom transport and every allocation count
unchanged; the model listing's future is now under tokio's 2,048-byte debug box threshold, and its bound was then
lowered to 2,048 - that threshold, and the measured size plus 32 as every bound - so that a model listing boxed in a
debug build fails `crates/sdk/tests/static_assertions.rs`. A request sent while the
pool holds a connection pays one pointer dereference more for its settings and no atomic operation more than before:
`call` does the two loads, takes no reference count and copies the connect timeout into the response future by value.
hyper-util's `Client::request` still clones its client for every request, and with it the connector's `Arc<Gate>` (an
increment, and a decrement when the request ends), as before the move. A request sent through the gate also clones the
gate's `Arc` once more.

What it does not cover: a connection that ends while requests are in flight is replaced inside hyper-util, which
resends a request that had not started (`retry_canceled_requests`) without passing the gate, so a burst at that
moment can still open a second connection that is closed at once.

**What a connect that outlasts the deadline costs.** At an endpoint that accepts TCP and never finishes the TLS
handshake - an overloaded TLS terminator, a stalled load balancer; `connect_timeout` is off by default and covers TCP
only - the request that opened is dropped at its deadline before a connection exists. Before the change, hyper-util
failed every request waiting for that connect with `request was canceled`. An earlier form of the gate handed the
dropped request's turn to a waiting one instead, which started a connect of its own and was dropped at its own deadline
in turn: one connect per waiting call's deadline. The waiting requests now fail at once, as before the change, with a
connection error that says the request that started the connect was dropped before a connection existed (S2d replaces
this rule: the connect now outlives the request that started it, under a bound); the default
policy retries them after its backoff, with one opener again when the wave's retries start within one attempt's deadline
of each other, which the default backoff (windows of 125 and 250 ms) and the default 10 s attempt guarantee; with a 100
ms deadline a wave of 64 made 6 or 7 connects and with a 50 ms one 11 to 13, before the change and with the gate alike
(macOS, 2 retries, 3 runs of each on both runtimes). 64 calls under `Http2Only` with a 1 s deadline each at a server
that accepts and never answers, measured on 2026-09-28, 3 runs of each; the columns are the connections the server
accepted per run.

| system | runtime | calls | retries | before the change | the earlier form of the gate | with the gate |
| --- | --- | --- | ---: | --- | --- | --- |
| macOS | current-thread | 1 ms apart | 0 | 1, 1, 1 | 20, 24, 21 | 1, 1, 1 |
| macOS | current-thread | 1 ms apart | 2 | 3, 3, 3 | 117, 96, 99 | 3, 3, 3 |
| macOS | current-thread | all at once | 0 | 1, 1, 1 | 1, 3, 1 | 1, 1, 1 |
| macOS | current-thread | all at once | 2 | 3, 3, 3 | not measured | 3, 3, 3 |
| macOS | multi-thread, 4 workers | 1 ms apart | 0 | 1, 1, 1 | 5, 4, 4 | 1, 1, 1 |
| macOS | multi-thread, 4 workers | 1 ms apart | 2 | 3, 3, 3 | 10, 11, 8 | 3, 3, 3 |
| macOS | multi-thread, 4 workers | all at once | 0 | 1, 1, 1 | 1, 1, 1 | 1, 1, 1 |
| macOS | multi-thread, 4 workers | all at once | 2 | 3, 3, 3 | 9, 13, 9 | 3, 3, 3 |
| Linux, 4 CPUs | both | 1 ms apart or all at once | 0 | 1, 1, 1 | not measured | 1, 1, 1 |
| Linux, 4 CPUs | both | 1 ms apart or all at once | 2 | 3, 3, 3 | not measured | 3, 3, 3 |

Before the change and with the gate alike, every call ended 1,001-1,012 ms after the burst started with no retry, and
4,135-4,163 ms after it with the default policy.

**What a failed connect costs.** Before the change, when the connect hyper-util was making for a burst failed, every
request waiting for it failed at once with `request was canceled`. A first form of the gate handed the turn to one
waiting request after another instead, each with a connect of its own: a burst of 64 with the default two retries at a
server that fails the TLS handshake after 200 ms ran up to 26.7 s, and the server saw 132 connects instead of 4. The
waiting requests now fail with the opener as before, and say why: `the connect this request waited for failed: ` and the
opener's failure, where the code before the change said `request was canceled`. For a connect that fails, what the
burst costs with the gate is what it cost before the change, as the table shows; a connect given up at a caller's
deadline is the paragraph above. A burst of 64 calls under `Http2Only` at a loopback endpoint whose connect fails, the
two builds run alternately on 2026-09-28, the gate as it ships; "fails the TLS handshake" accepts TCP and closes after
the delay, "closed after TLS" completes TLS with ALPN `h2` and closes after the delay without speaking HTTP/2. The
columns are when the calls ended (first to last, over all runs) and the connections the server accepted; no call timed
out.

| system | endpoint | retries | before the change | with the gate |
| --- | --- | ---: | --- | --- |
| macOS | fails the TLS handshake after 200 ms | 0 | 224-226 ms, 1 | 224-239 ms, 1 |
| macOS | fails the TLS handshake after 200 ms | 2 | 1,747-1,977 ms, 4 | 1,764-1,996 ms, 4 |
| macOS | closed right after TLS | 0 | 4-6 ms, 1 | 5-6 ms, 1 |
| macOS | closed right after TLS | 2 | 1,155-1,491 ms, 62-64 | 1,160-1,496 ms, 57-68 |
| macOS | closed 200 ms after TLS | 2 | 1,749-1,982 ms, 4 | 1,745-1,962 ms, 4 |
| Linux, 4 and 2 CPUs | fails the TLS handshake after 200 ms | 0 | 201-209 ms, 1 | 202-208 ms, 1 |
| Linux, 4 and 2 CPUs | fails the TLS handshake after 200 ms | 2 | 1,743-1,976 ms, 4 | 1,747-1,982 ms, 4 |
| Linux, 4 and 2 CPUs | closed right after TLS | 0 | 0-3 ms, 1-2 | 0-3 ms, 1-2 |
| Linux, 4 and 2 CPUs | closed right after TLS | 2 | 1,143-1,501 ms, 53-69 | 1,155-1,508 ms, 53-67 |
| Linux, 4 and 2 CPUs | closed 50 ms after TLS | 2 | 1,298-1,651 ms, 10-11 | 1,305-1,652 ms, 9-11 |
| Linux, 4 and 2 CPUs | closed 200 ms after TLS | 2 | 1,753-1,995 ms, 4 | 1,746-1,980 ms, 4 |

In an earlier run, with an earlier form of the gate, one of 40 Linux bursts at the endpoint closed right after TLS, with
no retry, ended at 13 ms with 3 connections: the opener's HTTP/2 handshake had failed after TLS, which hyper-util
reports as a `SendRequest` error rather than a connect error, and that form of the gate, which went by the error's kind,
passed the turn on. The gate now fails the waiting requests on any error that happened before there was a connection,
which hyper-util states on the error itself (it attaches no connection to it); the table above is measured with it.

A connection count had a second, rarer possible source: a stranger's request. Two tests of `crates/sdk/tests/client.rs` sent calls
to a port they had bound and released, and Linux hands a released port to the next `bind(0)` about once in 7,058 binds,
so a test server started inside their retry window could count that request as a connection (seen 62 times in 18,272
servers in a direct test, each a TLS handshake failed by a plaintext `GET /v1/models`). They now send to `127.0.0.1:1`,
which no `bind(0)` hands out, and the test server records per connection the peer, the TLS outcome with the first bytes
of a failed handshake, and the requests served, so a failed count says which of the two it was.

## S2d - the connect outlives the request that started it

**What the change does.** Before the change, the connect that opens a cold client's connection under `Http2Only` ran
inside the future of the request that started it, so a caller that gave up - its deadline, a cancelled task - took the
connect down for every request waiting at the gate (S2c). With the change, the connect runs in a task no request owns.
Its request is a `CONNECT` to the base URL's scheme and authority, declaring a one-byte body it does not send, which
hyper 1.11.1's HTTP/2 client refuses before it writes a frame (`proto/h2/client.rs:716-729`); hyper-util reports the
refusal with the connection, which stays pooled. The task releases every waiting request, the one that started the
connect included, when hyper-util reports the connection, as before the change (`capture_connection`), and drops the
probe's response in that poll. The connect is never given up while the request that started it waits. Once that request
is gone, it is given up when `connect_timeout` has passed since it began (under `Http2Only` that bounds the whole
connect: TCP, TLS and the HTTP/2 handshake), else the client's deadline of one attempt; the waiting requests then fail
with `ErrorKind::Timeout` carrying the connect timeout, or with a connection error that says the connect did not
complete within the deadline. A TCP connect that runs past the connect timeout ends the same way, for the request that
started it and every request waiting: the TCP connector's own connect timeout fires at the bound's instant and is seen
first, and a connect that failed with a timed-out chain while the bound is the connect timeout ends as that bound. (A
first form of the change quoted that failure as a connection error to the waiting requests; the verification of
2026-09-28 saw it at an endpoint that drops the TCP handshake, 10 of 10 on Linux.) A client with `no_timeout()` and no
`connect_timeout` keeps the rule of S2c. The bound is a timer in the task, acted on only when the task is polled, so on
a runtime that nothing drives between calls (a current-thread runtime and a `block_on` per call) a bound can pass unseen
until the next call: the failure carries the bound's deadline, and a request that began waiting after it opens a new
connect instead of failing with it. A call's future over the default transport keeps its size (2,328 / 2,312 / 2,312 /
2,016 bytes, macOS arm64 and Linux arm64, both profiles), and the warm calls' allocation counts are unchanged; the cold
path allocates a 1,368-byte box and a task whose future is 1,680 bytes (and a oneshot), where it allocated one
1,704-byte box before the change.

**How it was measured.** On 2026-09-28, 15:42-16:13 JST, before the change and with it alternating run by run, on the
macOS arm64 host (M3 Max) and in a Docker Desktop linux/arm64 container with 4 CPUs (`--cpuset-cpus=0-3`), each build
on a current-thread runtime and on a multi-thread runtime with 4 workers, 3 runs of each (1 run of each for the price
table). The record is the maintainers' run record of 2026-09-28: 284 rows per system, one per run, scenario, build and
runtime, each with the calls that succeeded, the connections accepted, the times and the requests each server served,
and the logs of the three cold-burst configurations. A cell is the calls that succeeded, the connections the server
accepted, and the first success (or the last call's end) in ms from the burst's start, over all runs of both runtimes.
"Connect timeout 2 s" is the same build with `connect_timeout(2 s)`: before the change it covers the TCP connect only.
Every runtime here was driven throughout, so the rule for a bound that passed unseen, added after these runs, changes
none of them. Ledger section written 2026-09-28, amended 2026-09-28.

A slow endpoint: TCP accepted at once, TLS and the HTTP/2 handshake finished 1.5 s later; 64 calls, a 1 s deadline.

| calls | retries | system | before the change | with the change | before the change, connect timeout 2 s | with the change, connect timeout 2 s |
| --- | ---: | --- | --- | --- | --- | --- |
| all at once | 2 | macOS | 0/64, 3, ends 4,138-4,161 | 0/64, 3, ends 4,136-4,143 | 0/64, 3, ends 4,140-4,150 | **64/64, 1, first ok 1,504-1,514** |
| all at once | 2 | Linux | 0/64, 3, ends 4,145-4,163 | 0/64, 3, ends 4,150-4,170 | 0/64, 3, ends 4,146-4,170 | **64/64, 1, first ok 1,504-1,508** |
| 1 ms apart | 2 | macOS | 0/64, 3, ends 4,138-4,158 | 0/64, 3, ends 4,139-4,157 | 0/64, 3, ends 4,140-4,149 | **64/64, 1, first ok 1,507-1,518** |
| 1 ms apart | 2 | Linux | 0/64, 3, ends 4,150-4,165 | 0/64, 3, ends 4,149-4,170 | 0/64, 3, ends 4,148-4,166 | **64/64, 1, first ok 1,505-1,509** |
| all at once | 0 | both | 0/64, 1, ends 1,002-1,008 | 0/64, 1, ends 1,001-1,009 | 0/64, 1, ends 1,001-1,008 | 0/64, 1, ends 1,001-1,006 |

With no retry no design serves a burst whose every deadline ends before the handshake does.

Callers that give up early: the same endpoint, 300 calls 10 ms apart under the SDK's defaults (a 10 s deadline, 2
retries), each cancelled by its caller's own 1 s `tokio::time::timeout`.

| system | before the change | with the change | with the change, connect timeout 2 s |
| --- | --- | --- | --- |
| macOS | 0/300, 3, ends 3,024-3,028 | **248-249/300, 1, first ok 1,507-1,517** | 248-249/300, 1, first ok 1,506-1,517 |
| Linux | 0/300, 3, ends 3,026-3,034 | **248-249/300, 1, first ok 1,506-1,509** | 249/300, 1, first ok 1,503-1,509 |

Before the change with a connect timeout of 2 s: 0/300 and 3 connections on both. The 51-52 calls lost are those whose
caller gave up before 1.5 s; a server that served one `POST` more than the calls that succeeded saw a call cancelled
while its request was in flight.

A call's own deadline longer than the client's: the same endpoint, the client's deadline 1 s, every call's 5 s, 64 at
once, no retry.

| system | before the change | with the change |
| --- | --- | --- |
| macOS | 64/64, 1, first ok 1,508-1,517 | 64/64, 1, first ok 1,507-1,516 |
| Linux | 64/64, 1, first ok 1,504-1,509 | 64/64, 1, first ok 1,504-1,509 |

With a connect timeout of 2 s the same, before the change and with it. This is why the connect is never given up while
the request that started it waits: a bound applied without looking at that request made this 0/64 in the measurements
that chose the rule.

What the servers saw of the connect's request, in every run above, of every build and scenario.

| runs | requests the servers served | `CONNECT` or any other request |
| --- | --- | ---: |
| 284 per system, macOS and Linux | the callers' `POST /v1/systemone` only (or none) | 0 |

`crates/sdk/tests/connection_reuse.rs` `a_cold_burst_sends_the_server_nothing_but_its_calls` pins it in the test suite, and a unit
test in `crates/sdk/src/transport/gate_tests.rs` pins the probe itself: a `CONNECT`, the base URL's scheme and authority, an empty
body, and no header but `content-length: 1`.

The price, at an endpoint whose first connection is held silent forever while every later one is served at once: 64
calls 1 ms apart, the default policy, 1 run per runtime.

| system | before the change | with the change | with the change, connect timeout 2 s | with the change, connect timeout 10 s |
| --- | --- | --- | --- | --- |
| macOS | 64/64, 2, first ok 1,388-1,389 | 64/64, 2, first ok 1,387-1,401 | 64/64, 2, first ok 2,760-2,766 | 0/64, 1, ends 4,499-4,525 |
| Linux | 64/64, 2, first ok 1,385-1,390 | 64/64, 2, first ok 1,387-1,391 | 64/64, 2, first ok 2,775-2,784 | 0/64, 1, ends 4,507-4,574 |

Without a connect timeout the bound is the deadline, and nothing changes. A connect timeout above the deadline makes
the retries wait for the stalled connect until it: recovery comes one retry wave (about 1.4 s) later with 2 s, and a
connect timeout longer than the three attempts last loses the burst.

A silent endpoint: TCP accepted, never a byte; 64 calls, a 1 s deadline. The connections the server accepted, and when
the last call ended.

| calls | retries | system | before the change | with the change | before the change, connect timeout 2 s | with the change, connect timeout 2 s |
| --- | ---: | --- | --- | --- | --- | --- |
| all at once | 0 | both | 1, 1,002-1,011 | 1, 1,001-1,007 | 1, 1,002-1,009 | 1, 1,001-1,009 |
| all at once | 2 | macOS | 3, 4,136-4,147 | 3, 4,137-4,151 | 3, 4,139-4,159 | 2, 3,992-4,004 |
| all at once | 2 | Linux | 3, 4,151-4,163 | 3, 4,145-4,192 | 3, 4,151-4,169 | 2, 4,002-4,012 |
| 1 ms apart | 0 | macOS | 1, 1,003-1,005 | 1, 1,003-1,005 | 1, 1,002-1,005 | 1, 1,064-1,067 |
| 1 ms apart | 0 | Linux | 1, 1,003-1,013 | 1, 1,005-1,014 | 1, 1,005-1,010 | 1, 1,065-1,066 |
| 1 ms apart | 2 | macOS | 3, 4,139-4,146 | 3, 4,138-4,153 | 3, 4,141-4,151 | 2, 3,990-4,006 |
| 1 ms apart | 2 | Linux | 3, 4,146-4,163 | 3, 4,149-4,167 | 3, 4,150-4,174 | 2, 3,997-4,014 |

Every call ended within its deadline plus 100 ms. With a connect timeout of 2 s a waiting call ends at its own deadline
(the last of a spread burst, started at 63 ms, at 1,064-1,067 ms) instead of with the first, and a retry wave waits for
the connect still running instead of opening one of its own: 2 connections instead of 3.

A retry wave opens one connection only when its retries start within one bound of each other: a retry that starts
after the wave's connect was given up opens one of its own. The default backoff and the default 10 s deadline guarantee
it; a deadline shorter than the backoff's spread (about 250 ms) does not, before the change and with it alike. From the
verification of 2026-09-28 (macOS, both runtimes): an endpoint that never answers, 64 calls, 2 retries; the connections
the server accepted.

| calls | deadline | before the change | with the change | with the change, connect timeout 2 s |
| --- | --- | ---: | ---: | ---: |
| all at once | 100 ms or 50 ms | 7-11 | 7-11 | 1 |
| 1 ms apart | 100 ms | 6-7 | 6-7 | 1 |

A bound that passes while nothing drives the runtime: one current-thread runtime driven only inside a `block_on` per
call, an endpoint that holds the first connection without a byte and serves every later one, the client's deadline
1 s. The first call's caller gives up after 300 ms, the runtime then sits undriven for 1.5 s, and a second call
follows. From the same verification: before the change the second call was served on a new connection in 10-47 ms;
with a first form of the change it failed in 0-1 ms with the stale bound's error (with a connect timeout of 2 s, a
`Timeout` carrying it), and with the rule above it is served on a new connection again, in a few to about twenty
milliseconds (2-21 ms measured) in the test suite (`crates/sdk/tests/connection_reuse.rs`
`a_call_after_a_bound_that_passed_unpolled_opens_a_new_connect`).

A connect that fails: TCP accepted, closed 200 ms later; 64 calls, none succeeds. The connections, and when the calls
ended.

| retries | system | before the change | with the change | before the change, connect timeout 2 s | with the change, connect timeout 2 s |
| ---: | --- | --- | --- | --- | --- |
| 0 | macOS | 1, 223-241 | 1, 202-245 | 1, 223-243 | 1, 204-246 |
| 0 | Linux | 1, 204-247 | 1, 204-244 | 1, 205-247 | 1, 205-246 |
| 2 | macOS | 4, 1,952-1,982 | 4, 1,947-1,995 | 4, 1,964-1,996 | 4, 1,948-1,986 |
| 2 | Linux | 4, 1,956-2,001 | 4, 1,962-1,994 | 4, 1,969-2,019 | 4, 1,964-1,994 |

Cold bursts of 64 with a second connection, the harness of S2c in the same container, the builds in alternating blocks.
Before the change the gate of S2c already holds it at 0; the check is that the change keeps it there.

| configuration | before the change | with the change |
| --- | ---: | ---: |
| 4 CPUs, 3 bodies in one process | 0 / 3,000 | 0 / 3,000 |
| 4 CPUs, 8 processes | 0 / 4,000 | 0 / 4,000 |
| 2 CPUs, idle | 0 / 2,000 | 0 / 2,000 |

A cold call's latency: 100 single calls per run, each on a new client, the server with `TCP_NODELAY`; medians and p99 in
µs, ranges over 3 runs.

| system | runtime | before the change, median / p99 | with the change, median / p99 |
| --- | --- | --- | --- |
| macOS | current-thread | 793-919 / 1,260-1,671 | 826-938 / 983-1,284 |
| macOS | multi-thread | 860-948 / 1,656-1,817 | 785-869 / 992-1,971 |
| Linux | current-thread | 294-356 / 477-724 | 296-337 / 545-712 |
| Linux | multi-thread | 393-395 / 622-876 | 393-412 / 651-780 |

Within the run-to-run spread: the task hop costs no round trip, because the waiting requests go on when hyper-util
reports the connection, before any response.

## S2e - the Python SDK in the same scenarios

**Why it was measured.** The README's deviations table says how the two SDKs differ in choosing a protocol and in
opening a connection; this is the record behind those rows. The Python SDK measured is `typesafe-sdk` 0.7.2 installed
with its `http2` extra (httpx2 2.13.1, httpcore2 2.13.1, h2 4.4.1, Python 3.14). 0.7.2 differs from 0.7.1 in its
packaging only - the extra, and the documents that name it - and `src/` and `tests/` are identical in both.

**How it was measured.** On 2026-09-29, 01:47-02:22 JST, on the macOS arm64 host (M3 Max), 3 runs of each row, the
server in the client's process and event loop. The server offers `h2` and `http/1.1` through ALPN and speaks the one
selected; for the slow endpoint it accepts TCP at once and starts the TLS handshake 1.5 s later. "Default" is a client
built as the SDK builds its own when it is given none (`httpx2.AsyncClient` without `http2`, so ALPN is offered
`http/1.1` alone), passed as `http_client` so that it trusts the test server; "HTTP/2" is
`http_client=httpx2.AsyncClient(http2=True)`, as the SDK's usage guide gives it. For the slow endpoint both had
`timeout=1.0`, which httpx2 applies to each phase and httpcore2 to the TCP connect and to the TLS handshake each; this
endpoint accepts TCP at once, so the TLS handshake has 1 s. "Connect timeout 2 s" is `httpx2.Timeout(1.0, connect=2.0)`.
The Python SDK's calls are `models.list()`. The rows of this crate are S2c's and S2d's, on the same host, except the
slow endpoint with no retry, which S2d gives as one range over macOS and Linux; S2d's calls are `POST /v1/systemone`.
A cell is the calls that succeeded, the connections the server accepted, and the first success (or the last call's
end) in ms from the burst's start. Ledger section written 2026-09-29.

A healthy endpoint, 64 calls at once.

| client | protocol | cold client | after one call |
| --- | --- | --- | --- |
| Python, default | `http/1.1` | 64/64, 64 connections | 64/64, 64 connections, the first call's reused |
| Python, HTTP/2 | `h2` | 64/64, 1 connection | 64/64, 1 connection |
| this crate, `Http2Only` (S2c) | `h2` | 64/64, 1 connection | 64/64, 1 connection |

The slow endpoint, 64 calls at once, a 1 s timeout.

| client | retries | result | with a connect timeout of 2 s |
| --- | ---: | --- | --- |
| Python, default | 0 | 0/64, 64, ends 1,028-1,046 | 64/64, 64, first ok 1,564-1,593 |
| Python, default | 2 | 0/64, 192, ends 4,517-4,536 | 64/64, 64, first ok 1,556-1,602 |
| Python, HTTP/2 | 0 | 0/64, 64, ends 64,148-64,329 | 64/64, 1, first ok 1,523-1,537 |
| Python, HTTP/2 | 2 | 0/64, 118-119, ends 64,216-64,314 | 64/64, 1, first ok 1,523-1,535 |
| this crate | 0 | 0/64, 1, ends 1,001-1,009 | 0/64, 1, ends 1,001-1,006 |
| this crate | 2 | 0/64, 3, ends 4,136-4,143 | 64/64, 1, first ok 1,504-1,514 |

With a connect timeout longer than the handshake both SDKs serve the burst, the Python SDK without a retry: it has no
deadline of an attempt, so nothing ends a call at 1 s while its connect goes on. This crate's deadline of an attempt
does, and its calls are served by their retries, on the connect their first attempts started.

With the calls 1 ms apart, a 1 s connect timeout and 2 retries: Python, default 0/64, 192, ends 4,477-4,540; Python,
HTTP/2 0/64, 119-120, ends 64,282-64,304. Over HTTP/2 httpcore2 gives the burst one connection, whose connect the
first request makes while the others wait for it; when that connect times out, the next request makes a connect of
its own, and so on, one after another. The connect timeout is counted for each connect, and no timeout covers the wait
for the requests ahead, so the last of 64 calls with a 1 s timeout ends 64 s after it started. It is the cost this
crate's gate was refused for in its first form (S2c: a waiting request never takes the connect over).

Callers that give up early: the slow endpoint, 300 calls 10 ms apart, a 10 s timeout, each cancelled by its caller
after 1 s: the Python SDK with no retry (`asyncio.wait_for`), this crate with its default 2 retries (S2d,
`tokio::time::timeout`).

| client | result |
| --- | --- |
| Python, default | 0/300, 300, ends 3,992-3,993 |
| Python, HTTP/2 | 0-1/300, 226-299, ends 3,992-3,993 |
| this crate, before S2d's change | 0/300, 3, ends 3,024-3,028 |
| this crate | 248-249/300, 1, first ok 1,507-1,517 |

In both Python clients a cancelled call stops its connect: no TLS handshake completed in 5 of the 6 runs. The one in
the sixth was made by a call whose cancellation did not take effect: repeated with each call's start and end recorded
(the verification of 2026-09-29), 1 of 15 runs over HTTP/2 had one call that ran 2,507 ms against its caller's 1 s
limit and succeeded, while every call of the other 14, and of 15 runs with the default client, ended within 1,122 ms.

What it does not cover: Linux, a network that is not loopback, the synchronous client, and an endpoint of which only
some backends are slow, where the default client's connections, one per request, do not share one fate.

## S4 - the server's HTTP/2 SETTINGS

```sh
$TARGET/release/transport-probe s4
```

Read twice from `api.typesafe.ai:443`, with no credential. First by writing the HTTP/2 preface by hand and decoding the
server's first SETTINGS frame off the TLS stream, which gives every parameter; then through the `h2` crate, which is
what hyper's HTTP/2 client uses underneath.

ALPN negotiated `h2` on both connections.

| Parameter | Value |
| --- | ---: |
| `SETTINGS_HEADER_TABLE_SIZE` | 4,096 |
| `SETTINGS_ENABLE_CONNECT_PROTOCOL` | 0 |
| `SETTINGS_MAX_CONCURRENT_STREAMS` | **1,024** |
| `SETTINGS_INITIAL_WINDOW_SIZE` | **16,777,216** (16 MiB) |
| `SETTINGS_MAX_FRAME_SIZE` | not sent, so the protocol default 16,384 applies |
| `SETTINGS_MAX_HEADER_LIST_SIZE` | not sent, so unlimited |

Immediately after its SETTINGS the server sent a connection-level `WINDOW_UPDATE` on stream 0 of **+25,100,289**, which
takes the connection send window to 25,165,824 bytes (24 MiB) from the protocol default of 65,535.

Through `h2`:

| Reading | Value |
| --- | ---: |
| `Connection::max_concurrent_send_streams()` | 1,024 |
| first `poll_capacity` grant on a fresh stream, after `reserve_capacity(16 MiB)` | **409,600** |
| `SendStream::capacity()` after that grant | 409,600 |

**Decision (plan section 5, S4 rule): neither branch of the rule fires, and nothing changes client-side.**
`SETTINGS_INITIAL_WINDOW_SIZE` is 16 MiB, far above the 1 MiB threshold, so there is no upload ceiling to document -
and it is the server's receive window in any case, which a client must not try to raise.
`SETTINGS_MAX_CONCURRENT_STREAMS` is 1,024, far above 100, so there is no fan-out limit to document either: the SDK's
own concurrency will be the binding constraint long before the server's.

One finding that is not in the plan's rule and matters for a large `state`: the 409,600-byte first grant is **not** the
server's window. It is `h2`'s own client-side send buffer cap, `proto::DEFAULT_MAX_SEND_BUFFER_SIZE = 1024 * 400`
(h2 0.4.19 `src/proto/mod.rs:47`), settable through `h2::client::Builder::max_send_buffer_size`. A multi-megabyte body
is therefore written in 400 KiB instalments as capacity is released, regardless of the 16 MiB the server advertises.
That is correct behaviour and needs no change, but it is the number to look at if a large upload is ever found to be
slower than the link allows - not the server's window.

**Keep-alive behaviour is NOT measured here.** Whether the load balancer counts an HTTP/2 PING as activity needs an
authenticated idle test.

## S3 - request compression

Date: 2026-09-19 (the server's clock read Fri, 18 Sep 2026 21:45 GMT). Made with `curl` against the live API **with**
an API key, taken from the environment variable `TYPESAFE_API_KEY` and passed on stdin, so it appears in no process
argument, file or output. Two API calls in total. The header name and scheme come from upstream
`src/typesafe_sdk/_core/transport.py` (`Authorization: Bearer <key>`); the request shape comes from the public
`https://api.typesafe.ai/openapi.json` (`SystemOneRequest`), and `jev-latest` is the only model name its documentation
gives.

The control body, 133 bytes, one noul question:

```json
{"model":"jev-latest","state":"I was charged twice.","questions":{"billing":{"type":"noul","instructions":"Is this about billing?"}}}
```

```sh
gzip -9 -n -c small.json > small.json.gz          # 133 -> 123 bytes; gzip -dc gives back the same bytes
# 1: control, uncompressed
curl -sS -o c1.body -D c1.hdr -w '%{http_code}\n' -X POST https://api.typesafe.ai/v1/systemone \
  -H 'Content-Type: application/json' --data-binary @small.json \
  -H @- <<< "Authorization: Bearer ${TYPESAFE_API_KEY}"
# 2: probe, the same bytes gzip-compressed
curl -sS -o c2.body -D c2.hdr -w '%{http_code}\n' -X POST https://api.typesafe.ai/v1/systemone \
  -H 'Content-Type: application/json' -H 'Content-Encoding: gzip' --data-binary @small.json.gz \
  -H @- <<< "Authorization: Bearer ${TYPESAFE_API_KEY}"
```

| # | Request | Bytes sent | Status | `content-type` | `x-envoy-upstream-service-time` | Response body |
| --- | --- | ---: | ---: | --- | ---: | --- |
| 1 | uncompressed, no `Content-Encoding` | 133 | **200** | `application/json` | 248 | `{"model":"jev-1.13.0","answers":{"billing":{"type":"noul","noul":0.97}},"usage":{"input_tokens":276,"output_tokens":20}}` |
| 2 | same bytes, `gzip -9`, `Content-Encoding: gzip` | 123 | **400** | `application/json` | 2 | `{"detail":"There was an error parsing the body"}` |

Both responses came over HTTP/2 from `server: istio-envoy` and carried an `x-typesafe-request-id`
(`req_01a0b67bb15b7bf897a8b30fe716ec1f` for 1, `req_01a0b67bc5287b93844dc4fd8956daff` for 2). Neither response sent
an `Accept-Encoding` or any other header naming the encodings the server takes.

The status is **400, not 415**. The server does not say "unsupported encoding": it reports the gzip body as a body it
cannot parse, in a `detail` string that is not the `{"error_type": ..., "message": ...}` object the 403 in S2a returned.
Taken with the 2 ms upstream time against 248 ms for the control, this is consistent with the compressed bytes reaching
the application's body parser as they were sent, with neither the envoy proxy nor the application decoding them. That
reading is an inference from these two responses; the server's configuration was not inspected.

Step 3 of the brief (a 64 KB `state`, compressed) was **not run**: it applies only when the probe succeeds. The body
for it had been built - 65,649 bytes, 404 bytes after `gzip -9` - so the saving the feature would have offered on a
repetitive `state` is known, but no server behaviour at that size was measured.

**Decision (plan section 5, S3 rule: "accepted -> opt-in `compress_requests(min_bytes)` enters Phase 5;
4xx -> recorded and dropped"): 4xx. Request compression is dropped.** No `compress_requests` knob is added, and the
client sends request bodies uncompressed.

Not measured here: `deflate`, `br` and `zstd` request encodings (the rule is about gzip, and one 4xx settles it for
the plan); response compression (`Accept-Encoding` on the response side), which is a separate question; and whether
the 400 would count as retryable - it is a 4xx the SDK must surface, and it cannot arise once the client never sets
`Content-Encoding`.

---

## AC-P0 - itemized allocation inventory

Binary: `spikes/alloc-inventory`. Every step of plan section 3.3 prototyped and measured on the **second identical
call**, after one warm-up pass of the same shape. The body encoder is not re-implemented here: the crate path-depends
on `spikes/encode-buffer` and measures the S6 winner itself.

```sh
cd spikes/alloc-inventory && env -u RUSTFLAGS cargo --config ~/.config/rust/config.dev.toml build --release
$TARGET/release/alloc-inventory steps
$TARGET/release/alloc-inventory decode
$TARGET/release/alloc-inventory verify
```

### Steps 1 to 5: request assembly

| Step | Item | blocks | bytes |
| --- | --- | ---: | ---: |
| 1 | body encode, 1 KB string `state`, S6 winner (`iii-1shot`) | **1** | 1,385 |
| 2 | retain the body for retry (`Bytes::clone`) | **1** | 24 |
| 3 | clone the base `HeaderMap` (6 headers) | **2** | 656 |
| 4 | build the `http::Request` from a pre-parsed `Uri` (headers moved in, not re-cloned) | **0** | 0 |
| 4a | clone the pre-parsed `Uri` on its own, second time | 0 | 0 |
| 4b | clone a freshly parsed `Uri`, **first** time | **0** | 0 |
| 4c | clone that same `Uri`, second time | 0 | 0 |
| 5 | `BodyExt::collect` + `to_bytes` on a single-frame body | **1** | 128 |

The body was 1,385 bytes and the thread's retained scratch was 6,188 bytes afterwards.

Three of these were open questions and are now settled. Cloning an `http::Uri` is **free even the first time** - unlike
`Bytes`, it does not pay a shared-header allocation on first clone - so step 4 costs nothing at all once the body's
own clone is accounted for under step 2. The `HeaderMap` clone costs **2** blocks, which is exactly what AC-P6 assumed.
`BodyExt::collect` costs **1** block for its frame queue, which is also what AC-P6 assumed.

### Steps 6 to 8: decode

| Step | Representation | blocks | bytes |
| --- | --- | ---: | ---: |
| 6 | prototype visitor -> `Vec<(String, Answer)>`, single pass, order independent, pre-sized | **14** | 554 |
| 7 | naive comparator: `#[serde(tag = "type")]` answers in a `HashMap<String, _>`, maps throughout | **26** | 2,672 |
| 8 | derived typed struct (`Ticket { spam, tone, quality }`), the `#[derive(QuestionSet)]` shape | **10** | 275 |

The plan's three budget questions, all on the same codec and the same fixture:

| Question | Measured | Verdict |
| --- | --- | --- |
| is (6) <= 14 + C blocks, with C = 0 from S1(c)? | 14 <= 14 | **yes, exactly** |
| is (6) <= 0.7 x (7)? | 14 <= 18.2 (the ratio is 0.54) | **yes** |
| is (8) < (6)? | 10 < 14 | **yes** |

The prototype's 14 blocks are the plan's own item list, one for one: 1 model `String`, 1 answers `Vec`, 3 question-name
`String`s, 1 choice `String`, 1 choice-probabilities `Vec`, 2 option-name `String`s, 1 legend `Vec`, 3 legend
`String`s, 1 score-probabilities `Vec`. `usage`, every `f64`, every score level and every probability key cost nothing:
levels are parsed from the object key text straight into `u32`.

**Warning for the freeze worker: (6) meets its budget with ZERO headroom.** One extra `String`, one extra `Vec`, one
`Box` in the answer representation and AC-P2 fails. The 0.7x ratio, by contrast, has room (0.54 measured).

### Correctness of the prototype visitor

```sh
$TARGET/release/alloc-inventory verify
```

Decoded twice from a document whose answers, and whose fields within every answer object, are in a different order from
the fixture, and which carries one extra answer of the unknown type `"prediction"`:

| Check | Result |
| --- | --- |
| wire order: sonic-rs result == serde_json result | **true** |
| shuffled order: sonic-rs result == serde_json result | **true** |
| shuffled order yields the same answers, compared as sets | **true** |
| shuffled order yields the same answers, compared in order | false, by design (see below) |
| `model` and `usage` unchanged | true |
| answers kept from the shuffled document | `["quality", "tone", "spam"]` |
| the unknown `"prediction"` answer was dropped | **true** |
| answer count | 3, for 3 questions asked |

The visitor is order independent in the sense that matters: `type` may arrive after the data fields, and it does in the
shuffled document, where `probabilities` is read before the answer type is known. The score's probability keys are then
read as names and converted to levels at dispatch. That path is exercised by the shuffled document and costs nothing in
the measured case, where `type` comes first.

**A design point the next worker has to decide:** every container keeps **wire order**, so a response whose keys arrive
in another order produces the same pairs in another order, and two decodes compare unequal as sequences while comparing
equal as sets. Either the SDK sorts `legend` and the score `probabilities` by level when it builds them - cheap, at
most ten elements, and it makes equality and `Debug` output stable - or every test that compares answers has to
normalize first. This is not covered by any current acceptance criterion.

### Proposed frozen budget table

Proposed only, and superseded: the budgets as frozen are in the final AC-P1 / AC-P2 / AC-P3 / AC-P6 check below, and
as tightened under `compact_str` for names.

Every number is from the tables above, with the plan's rule applied: tightening is free, loosening needs a ledger entry
and the user's sign-off. Where a measurement left no headroom, the budget is set at the measurement and said to be
tight.

**AC-P1 - body encode, second identical call, variant `iii-1shot`**

| Bound | Plan as written | Proposed | Measured |
| --- | --- | --- | --- |
| blocks, encode alone | <= 2 | **<= 1** | 1 |
| blocks, including retaining the body for retry | <= 3 (2 + 1) | **<= 2** | 2 |
| bytes allocated during the call | <= 1.25 x body + 64 KiB | **<= 1.05 x body + 4 KiB** | 1.00 x body |
| retained scratch | <= 8x the decayed hint | **<= 8x the decayed hint** (unchanged) | 4.47x - 5.89x |

Carve-out that has to be written into the criterion: a call on which the scratch **shrinks** allocates one extra block
of at most the decayed hint (measured: 775,406 bytes on call 5 of the mixed sequence). AC-P1 is stated for a repeated
identical call, where no shrink happens; the shrink is bounded by the retained-scratch rule instead. Without this
sentence the criterion is ambiguous the first time a test mixes sizes.

**AC-P2 - decode of the 3-answer fixture**

| Bound | Plan as written | Proposed | Measured |
| --- | --- | --- | --- |
| blocks | <= 14 + C | **<= 14** (C = 0, measured in S1(c)) | 14, **tight** |
| bytes | not specified | **<= 700** | 554 |
| ratio to the naive comparator | <= 0.7x | **<= 0.7x** (unchanged; the ratio depends on serde's version as well as on ours) | 0.54x |

**AC-P3 - derived typed decode** uses fewer blocks than AC-P2: 10 < 14, confirmed.

**AC-P6 - full call through an in-memory service, second identical call, 1 KB string `state`, default retry policy
(body retained), no per-call headers**

| Item | blocks | bytes |
| --- | ---: | ---: |
| body encode (step 1) | 1 | 1,385 |
| body retention for retry (step 2) | 1 | 24 |
| `HeaderMap` clone (step 3) | 2 | 656 |
| request assembly from the pre-parsed `Uri` (step 4) | 0 | 0 |
| `collect` frame queue (step 5) | 1 | 128 |
| decode (step 6) | 14 | 554 |
| **total** | **19** | **2,747** |

Proposed AC-P6 budget: **<= 19 blocks** above the floor of calling the same service directly with a pre-built request,
for that pinned scenario. The plan's own assumptions inside AC-P6 - "`HeaderMap` clone = 2, frame queue of `collect` =
1" - are both confirmed rather than assumed.

### Open questions this inventory does not answer

- The floor itself (what an in-memory service costs when called directly with a pre-built request) is not measured
  here; AC-P6 is a difference, and the subtrahend belongs to the Phase 5 harness.
- Every number is for a 1 KB string `state`. An object `state` adds the retained scratch of variant `iii-1shot` to the
  picture but not to the per-call block count; a 1 MB `state` moves only the bytes, not the blocks.
- `tracing` is off in all of these runs. A subscriber's own allocations are not part of any budget here, which matches
  AC-P6's wording ("no `tracing` subscriber").

---

## Allocation harness

### 2026-09-19 - libtest's own allocations in a measured section

**What flaked.** CI run 35416188952 on `6658076`, job `coverage`, step "Line coverage"
(`cargo llvm-cov nextest -p typesafe-sdk-rust --all-features --fail-under-lines 85`): exit 100, a failed test, after
63 s. The same step passed on `383e2fb`, and the uninstrumented `check` job passed on the same commit.

**Reproduced** on a Linux x86_64 host (Debian 13, 44 CPUs) at `6658076`, built as cargo-llvm-cov builds
(`-C instrument-coverage --cfg=coverage`, `--target-dir <repo>/target/llvm-cov-target`). Every test of
`-p typesafe-sdk-rust` ran in its own process, as nextest runs it, 4 at a time, each under `taskset -c 0-3`: 4
failures in 31 rounds x 390 tests. All four were allocation budgets, and all four had the same extra charge:

| Round | Test | Measured | Normally |
| --- | --- | --- | --- |
| 11, 26 | `alloc_call` whole call, SDK's own | 23 blocks (the retained-body line 5 / 924) | 19 (1 / 24) |
| 25, 27 | `alloc_encode` 64 KB string | 5 blocks, 69,508 bytes | 1, 68,608 |

**+4 blocks, +900 bytes** each time. No timing-dependent test failed in those rounds.

**Root cause.** dhat's `HeapStats` counts every thread of the process, and each budget is a delta of those counters.
libtest runs a test on a thread it spawns. Right after the spawn, its main thread allocates its own bookkeeping, once
per process: rustc 1.98.1 `library/test/src/lib.rs` lines 460-463, `running_tests.insert(id, RunningTest {
join_handle })` and `timeout_queue.push_back(TimeoutEntry { id, desc, timeout })`, then it waits in
`rx.recv_timeout`. A nextest process is the same libtest binary running one test, so it does the same. On a loaded
machine the main thread can be descheduled between the spawn and those allocations, and they land in the section the
test is measuring. The SDK's allocations did not change; the budgets were sound.

**Method now used** (`crates/sdk/tests/support/mod.rs`, shared by the four `alloc_*` tests):
- Each asserted section runs five times after its warm-up call, and the budget is held to the **minimum**.
- At least **three of the five** runs must equal that minimum in blocks and bytes.
- Every run is printed (`runs of <section> blocks/bytes: ...`).

Why this cannot hide a regression:
- Another thread can only add to a process-wide count, never remove one. The SDK's cost of a repeated identical call
  is the same every time by design, so the minimum is that cost.
- An allocation the SDK makes on every call raises all five runs, and so the minimum.
- One it makes on only some calls leaves fewer than three runs at the minimum and fails the stability rule.
- The foreign bookkeeping happens once per process, but it is several allocations over three calls
  (`running_tests.insert` +1, `timeout_queue.push_back` +1, and `rx.recv_timeout`'s own), +4 blocks / +900 bytes in
  all on Linux x86_64. Nothing in libtest keeps them inside one run: that they land in ONE run of five is an
  **empirical** bound (46 of 46 polluted windows below), not a guarantee. If they ever split across runs so that fewer
  than three equal the minimum, the worst case is a failed stability rule - a flake - and never a false pass, because
  a foreign allocation can only raise a run, never lower the minimum below the SDK's own cost.

What is not repeated, and why:
- `prepared()`'s first call is measured once, because only the first call is a first call.
- The mixed-size sequence of `alloc_encode` repeats as a whole, five times from an empty scratch, and each call of it
  is held to its own stable minimum. Its 1 MB line stays printed-only.

**The frozen budgets did not move:** encode 1 block per call; decode 14 blocks / 626 bytes (budget 14 / 700), struct
set 10 / 347; a call 19 blocks (18 without retry) above the transport's 3; `prepared()` 0 / 0;
`Questions::prepare()` 3 / 1,524. Every one of them, measured on this machine (see **Environment**) with the harness
above, was the same in all five runs.

**Proof** on the same Linux host at `cb57396`, the same instrumented, pinned, 4-at-a-time loop: **0 failures in
35 rounds x 390 tests**. The alloc tests ran with `--nocapture`, 140 runs of them in all. **46 windows** had one
polluted run, never two, and it always carried the same +4 blocks / +900 bytes, e.g. `whole call 22/3405 26/4305
22/3405 22/3405 22/3405`. That is more than the four failures before because every section now runs five times, so
the process's one foreign charge has more windows to land in. The minimum filtered every one of them. One
platform difference, not a pollution: the naive comparator's bytes are 2,704 on Linux x86_64 against 2,672 on the
M3, the same 26 blocks.

---

## Phase 5 - performance

Everything below was measured at the commit its table names; the final tables are at `fd22977`. Nothing was run
while another benchmark or a build of this checkout ran.

### Environment

| Item | macOS | Linux |
| --- | --- | --- |
| CPU | Apple M3 Max, arm64, 16 cores | Intel Xeon Platinum 8481C, x86_64, 44 vCPUs (AVX2, AVX-512F) |
| OS | macOS 27.2, Darwin 27.2.0 | Debian 13.7, kernel 6.12.105+deb13-cloud-amd64, glibc 2.41 |
| Toolchain | rustc 1.98.1 (`rust-toolchain.toml`) | rustc 1.98.1 (`rust-toolchain.toml`) |
| Bench crates | `codspeed-divan-compat` 5.0.2 (its walltime layer is a divan 0.1 fork) | the same |
| Instruction counts | - | valgrind 3.24.0 (callgrind), cargo-codspeed 5.0.1 |
| Load while measuring | **not idle**: other sessions' test binaries held about 3 of 16 cores; load average 8-15 | idle apart from these runs (load average under 2) |

`RUSTFLAGS` was cleared on every run and no `target-cpu` was set, so sonic-rs ran its default SIMD level (SSE2
baseline on x86_64, NEON on arm64): the numbers are those of a consumer's default build. The macOS wall-clock numbers
carry the load above and are report-only; the Linux instruction counts are the reference.

### Method

Wall clock (divan, 100 samples, median shown; `loopback` 50 samples), `bench` profile without the dev config:

```sh
env -u RUSTFLAGS cargo bench --all-features --bench sdk         # macOS
env -u RUSTFLAGS cargo bench --all-features --bench loopback
taskset -c 2 cargo bench --all-features --bench sdk             # Linux, pinned to one core
cargo bench --all-features --bench loopback                     # Linux, not pinned: it needs both runtimes' threads
```

Instruction counts are callgrind `Ir`, the instruction component of CodSpeed's simulation, taken without CodSpeed's
runner (which is installed by its GitHub action and is not on the host), a token, an account or an upload.
`cargo codspeed build` produces the instrumented binary; valgrind's callgrind runs it with instrumentation off, and
each benchmark switches it on and off around its own measured call through `codspeed`'s client requests and dumps its
counts under its own name:

```sh
cargo codspeed build -m simulation -p typesafe-sdk-rust --features internals --bench sdk
CODSPEED_ENV=local CODSPEED_CARGO_WORKSPACE_ROOT=$PWD taskset -c 2 \
  valgrind --tool=callgrind --instr-atstart=no --compress-strings=no \
  --callgrind-out-file=out.%p target/codspeed/analysis/typesafe-sdk-rust/sdk
callgrind_annotate out.<pid>.<n>          # PROGRAM TOTALS, one dump per benchmark
```

callgrind writes `summary: 0` into these dumps (the header is computed before instrumentation is toggled);
`callgrind_annotate` recomputes the total from the cost lines. Every configuration was run three to five times.

`Ir` is not what CodSpeed reports. Its runner (`CodSpeedHQ/codspeed`, `src/executor/valgrind/measure.rs`) runs its
own valgrind with cache simulation (`--cache-sim=yes --I1=32768,8,64 --D1=32768,8,64 --LL=8388608,16,64`) and, since
the action's `cycle-estimation` defaults to on, `--cycle-estimation=yes`, and reports an estimated time in which cache
misses weigh heavily: each benchmark runs once, on cold caches. The verifier (`p5-verify`, report of the Phase 5 exit
gate) re-ran the whole-call pair with the same cache flags on stock valgrind (which has no cycle estimation), three
runs at `acd4df3`:

| Event | `call::sdk` | `call::naive` |
| --- | ---: | ---: |
| Ir | 33,908 | 65,347 |
| I1mr | 1,073 | 1,320 |
| D1mr | 203 | 308 |
| D1mw | 263 | 241 |
| ILmr | 1,030 | 1,229 |
| DLmr | 199 | 289 |
| DLmw | 261 | 238 |

The SDK is lower in every class except write misses, higher there by 22 to 23 events, against 31,000 fewer
instructions and 200 fewer instruction misses in the last-level cache: the comparison holds under any plausible
weighting of the events.

**Run-to-run spread**, five runs at `acd4df3` on the Linux host (min-max, as a share of the min). It comes from
malloc's state, which depends on the order the benchmarks ran in, so a benchmark that allocates in its measured call
is not exactly repeatable; only those at 0.0% are:

| Benchmark | Spread | Benchmark | Spread |
| --- | ---: | --- | ---: |
| `assembly::request` | **20.4%** | `decode::typed` | 0.0% (22,982 each run) |
| `assembly::header_map_clone` | 0.0% | `decode::codec::sonic_rs[3]` / `[20]` | 0.0% / 1.1% |
| `call::floor` | 0.0% | `decode::codec::serde_json[3]` / `[20]` | 0.0% / 0.1% |
| `call::sdk` / `sdk_20` | 1.6% / 0.4% | `encode::prepared[1024]` / `[65536]` / `[1048576]` | 1.9% / 0.0% / 0.0% |
| `call::naive` / `naive_20` | 2.4% / 0.5% | `encode::unprepared[1024]` / `[65536]` / `[1048576]` | 1.6% / 0.1% / 0.0% |
| `decode::answers[3]` | 3.1% | `encode::object_1mb`, `mixed_sizes`, `encode::codec::*` | 0.0% |
| `decode::answers[20]` | 0.6% | `retry::*` | 0.0% |

The verifier's own five runs at the same commit agree in kind: `answers[3]` 23,219-23,611 (1.7%), `call::sdk`
33,890-34,891 (2.95%), `assembly::request` 5,258-6,365 (21%). The five runs at `0303980` this section first
reported (0.0% for every decode) were narrower than both: `answers[*]` is not deterministic, `typed` is. A rebuild
alone also moves some counts by up to about 1% without any change to their code (code layout): the serde_json decode
moved +0.5% between `0303980` and `fd22977`, and nothing in its path changed.

**What the benches measure and how they can mislead** is written at the top of each bench module
(`crates/sdk/benches/sdk/*.rs`, `crates/sdk/benches/loopback.rs`). In short: B1 to B5 time steady state (warm scratch, warm header map);
B3 rebuilds crate-private steps from the same parts and checks its body byte for byte against one a real call sent;
B5's transport answers at once, so it contains no network at all; B6 is loopback, an upper bound on one connection's
throughput and never a prediction for a network.

### B1 - encode (`fd22977`)

| Bench | macOS median | Linux median | Linux instructions (min-max of 3) |
| --- | ---: | ---: | ---: |
| `prepared[1 KB]` | 333.2 ns | 437.1 ns | 3,317-3,342 |
| `prepared[64 KB]` | 20.70 µs | 25.31 µs | 227,996-228,094 |
| `prepared[1 MB]` | 321.9 µs | 437.4 µs | 2,663,300-2,663,496 |
| `unprepared[1 KB]` | 603.9 ns | 824.6 ns | 7,140-7,540 |
| `unprepared[64 KB]` | 21.24 µs | 25.69 µs | 231,826-232,202 |
| `unprepared[1 MB]` | 322.4 µs | 436.6 µs | 2,667,191-2,667,462 |
| `object_1mb` | 321.6 µs | 427.4 µs | 2,664,122-2,664,139 |
| `mixed_sizes` (1 MB then 16 x 1 KB, fresh scratch) | 338.4 µs | 724.7 µs | 2,721,562-2,722,805 |

Preparing the three questions on every call instead of once costs 3,800 to 4,200 instructions at every size, which is
the whole difference between `unprepared` and `prepared`: 270 ns (macOS) and 390 ns (Linux) at 1 KB, and lost in the
spread at 64 KB and 1 MB.

### B2 - decode (`fd22977`)

| Bench | macOS median | Linux median | Linux instructions |
| --- | ---: | ---: | ---: |
| `answers[3]` (`SystemOneResponse<Answers>`) | 1.541 µs | 2.304 µs | 23,323-24,271 |
| `answers[20]` | 11.29 µs | 17.65 µs | 184,731-185,630 |
| `typed` (`#[derive(QuestionSet)]` struct) | 1.260 µs | 2.331 µs | 22,982 |

### B3 - request assembly (`fd22977`)

| Bench | macOS median | Linux median | Linux instructions |
| --- | ---: | ---: | ---: |
| `request` (encode 1 KB + retained body + header map + request) | 419.0 ns | 607.9 ns | 5,136-6,343 |
| `header_map_clone` (6 headers) | 44.97 ns | 119.9 ns | 674 |

### B4 - `Retry-After` and backoff (`fd22977`)

| Bench | macOS median | Linux median | Linux instructions |
| --- | ---: | ---: | ---: |
| `retry_after[ms]` | 39.44 ns | 99.91 ns | 789-793 |
| `retry_after[seconds]` | 45.95 ns | 90.11 ns | 897 |
| `retry_after[date]` (includes `SystemTime::now()`) | 81.75 ns | 169.4 ns | 1,773 |
| `backoff[1]` | 112.3 ns | 247.9 ns | 2,672 |
| `backoff[6]` (capped) | 117.5 ns | 261.9 ns | 2,852 |
| `backoff[1000]` | 117.5 ns | 260.8 ns | 2,852 |

The backoff delay costs more than parsing a header because `round_to_millis` rounds exactly as Python's
`round(x, 3)` does, by formatting to three decimals and parsing back (no allocation). It runs once per retry, beside a
sleep of at least hundreds of milliseconds, and it is the frozen arithmetic of `retry.rs`: recorded, not changed.

### B5 - a whole call, its floor and the naive comparator (`fd22977`)

| Bench | macOS median | Linux median | Linux instructions |
| --- | ---: | ---: | ---: |
| `floor` (transport called directly, pre-built request) | 198.9 ns | 390.1 ns | 2,526 |
| `sdk` (3 questions, 1 KB state) | **2.395 µs** | **4.088 µs** | **33,964-34,174** |
| `naive` (comparator A, same transport) | 5.041 µs | 8.032 µs | 64,601-65,057 |
| `sdk_20` (20 questions, six 5-level scores) | 11.12 µs | 19.34 µs | 193,825-194,086 |
| `naive_20` | 25.49 µs | 45.85 µs | 422,759-426,347 |

The SDK's call costs 0.52x the naive client's instructions (0.46x with 20 questions): AC-P7's condition ("the
instruction count of the SDK full-call benchmark is lower than the naive comparator's") holds on this host. AC-P7
itself is proved only by a pull-request run of `bench.yaml` on CodSpeed's runner, which is the owner's to set up.

Comparator (B), the published `typesafe-rs` 0.1.0, was **not** measured: it is a new crate, which this phase does not
add. Measuring it would take adding it as a dev-dependency (a license and advisory check through `cargo deny`), and
it can only be compared if its client accepts a base URL, so that it can be pointed at the loopback TestServer rather
than at the live API.

### B6 - loopback HTTP/2 over TLS (`fd22977`, wall clock only)

| Bench | macOS median | Linux median |
| --- | ---: | ---: |
| `sequential` (one call at a time, warm connection) | 57.88 µs | 89.60 µs |
| `concurrent_64` (64 calls spawned at once, until the last answers) | 1.539 ms | 1.621 ms |

64 concurrent calls take about 27x (macOS) and 18x (Linux) the time of one: they share one connection and one
server, and the loopback round trip is the floor of both.

### sonic-rs and serde_json, no flags (`fd22977`)

The SDK's own decoder cannot be pointed at serde_json (`codec.rs` is the only module naming a codec), so both parsers
decode the naive comparator's serde types from the same bytes, and both encode the same state string into a retained
buffer.

| Bench | macOS sonic-rs | macOS serde_json | Linux sonic-rs | Linux serde_json | Linux instr. sonic-rs | Linux instr. serde_json |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| decode, 3 answers | 1.208 µs | 1.405 µs | 2.124 µs | 2.252 µs | 21,448 | 27,110-27,115 |
| decode, 20 answers | 9.832 µs | 11.04 µs | 20.98 µs | 21.91 µs | 198,859-199,476 | 221,402-221,535 |
| encode, 1 KB | 296.6 ns | 510.1 ns | 373.1 ns | 805.2 ns | 2,589-2,657 | 11,161 |
| encode, 64 KB | 18.91 µs | 32.29 µs | 23.35 µs | 53.71 µs | 158,969 | 709,291 |
| encode, 1 MB | 305.1 µs | 518.9 µs | 376.3 µs | 844.9 µs | 2,542,539 | 11,347,676 |

**sonic-rs does not lose on the small-response decode (R3, decision D2):** 21% fewer instructions on the 3-answer
document, and faster on both machines. In wall-clock time on Linux x86_64 without `target-cpu` the gap is small and
was a tie in one run (2.187 µs against 2.193 µs at `0303980`), so the advantage there is in instructions rather than
in time. On encode sonic-rs is 1.7x (macOS) to 2.3x (Linux) faster and runs 4.3x to 4.5x fewer instructions.

### Candidates (Linux instructions against the run-to-run spread above; blocks from the dhat tests on macOS)

| # | Candidate | Result | Numbers | Decision |
| --- | --- | --- | --- | --- |
| 1 | `compact_str` 0.10.0 for the model, answer names, choice pick and option names | measured in the working tree only | decode 14 -> **7** blocks, 626 -> 578 bytes; derived 10 -> 6; a call's own 19 -> **12**; instructions: `answers[3]` -2.1%, `answers[20]` -8.5%, `call::sdk` -2.9%, nothing up | waited for a ruling (a new crate); **adopted** after the owner approved the dependency: `821d970`, see "`compact_str` for names" below |
| 2 | `Bytes`-slice zero-copy names (a crate-private `Text`: a slice of the retained body, or owned when escaped; the body reaches the visitors through a thread-local) | reverted | decode 14 -> 7 blocks but 626 -> **658** bytes (`Text` is 32 bytes, `String` 24); instructions: `answers[3]` **+2.4%** (within its 3.1% spread), `typed` **+2.5%** (a benchmark that repeats exactly), `answers[20]` -4.5%, `call::sdk` -1.4% | **reverted**: an instruction regression elsewhere. It would also make a kept answer pin the whole response body (up to 16 MiB), a behaviour change |
| 3 | dense instead of sparse score-level storage | not built: bounded by measurement | all of `insert_by_level` is 1.86% of `answers[3]` (3.94% of `answers[20]`) and vector growth 0.44% (1.20%), exclusive callgrind cost; a dense layout keeps one vector per list, so no block moves | **rejected**: even free storage could not reach 5% on B2, and dense storage cannot keep the documented "a level named twice keeps both entries" |
| 4 | pre-baked `,"model":...,"questions":...}` suffix | measured as a bench variant | `prepared[1 KB]` 3,356-3,400 against 3,331-3,384 now; 64 KB and 1 MB within 0.1%; 0 blocks either way | **reverted**: no gain; the four `extend_from_slice` calls it replaces are too cheap to see |
| 5 | call-future sizes against tokio's debug box threshold (v3.5 (1)) | **kept**, `7493955` + `fd22977` | every call's future -352 bytes: System One over a custom transport 2,392 -> **2,040** (under 2,048), models 2,080 -> 1,728; over hyper 2,760 -> 2,344 (still over) and 2,448 -> 2,032; an unpinned debug call 20 -> **19** blocks of its own; instructions unchanged within the spread | kept under v3.5 (1), which named exactly this block as the target ("shrink the futures if it is cheap"), not under the generic keep rule: it moved no instruction count and no pinned or release block, only the unpinned debug call's box (release was never boxed, 16,384) |
| 6 | score level lists sized from the questions asked (v3.5 (2): 5 to 8 levels cost a block) | **kept**, `06573d7` + `fd22977` | whole call, measured by a throwaway dhat test over a transport that allocates nothing: 20 questions with six 5-level scores 117 -> **111** blocks, 9,439 -> 8,095 bytes; 3 questions 19 -> 19 blocks, 2,728 -> 2,696 bytes (`alloc_call`'s whole call: 22 / 3,405 -> 22 / 3,373). `call::sdk_20` -1.3% instructions (ranges do not overlap) | kept. `06573d7` alone grew the future to 2,064 bytes and brought the debug box back; `fd22977` holds the context in two `u32`s and restores 2,040 |
| 7 | a hard ceiling on the retained scratch (R17), 1 MiB, in `codec.rs` | measured in the working tree only | AC-P1 at 1 MB: 1 -> **3** blocks, 1,092,608 -> **7,384,117** bytes per call (**fails AC-P1**); 1 KB and 64 KB unchanged. Time: `prepared[1 MB]` 433.0 -> 434.3 µs on Linux, 321.7 -> 326.4 µs on macOS; `mixed_sizes` 646 -> 444 µs (Linux); instructions within 1% | **proposal for the lead and the user** (R5(b), (d)): it caps the memory a thread keeps after a large body at 1 MiB instead of about 6x the body, costs no measurable time on either allocator, and turns AC-P1's 1 MB row from 1 block / 1.00x into 3 blocks / 6.8x |

Commands: candidates 1, 2, 5, 6 and 7 were applied as a patch to the Linux scratch checkout, built with
`cargo codspeed build` and counted with the callgrind command above, three runs each; their blocks come from
`cargo test --all-features --test alloc_decode --test alloc_call --test alloc_derive --test alloc_encode -- --nocapture`
on macOS, and candidate 6's 20-question call from a throwaway dhat test of the same shape (not committed). Candidate 3's
bound is `callgrind_annotate` on the `decode::answers` dumps of the baseline.

### Final AC-P1 / AC-P2 / AC-P3 / AC-P6 check (`fd22977`, macOS, budgets as frozen)

```sh
env -u RUSTFLAGS cargo --config ~/.config/rust/config.dev.toml test --all-features \
  --test alloc_encode --test alloc_decode --test alloc_derive --test alloc_call -- --nocapture
env -u RUSTFLAGS cargo --config ~/.config/rust/config.dev.toml test --release --all-features \
  --test alloc_encode --test alloc_decode --test alloc_derive --test alloc_call -- --nocapture
```

All four tests pass in both profiles, and every one of the 36 measured sections printed the **same five runs in the
dev and the release profile**:

```
  runs of whole call                             blocks/bytes: 22/3373 22/3373 22/3373 22/3373 22/3373
  runs of whole call, no deadline                blocks/bytes: 22/3373 22/3373 22/3373 22/3373 22/3373
  runs of whole call, no retry                   blocks/bytes: 21/3349 21/3349 21/3349 21/3349 21/3349
  runs of whole call, unpinned                   blocks/bytes: 22/3373 22/3373 22/3373 22/3373 22/3373
  runs of whole call, unpinned, no retry         blocks/bytes: 21/3349 21/3349 21/3349 21/3349 21/3349
  runs of transport called directly              blocks/bytes: 3/677 3/677 3/677 3/677 3/677
  runs of encode                                 blocks/bytes: 1/1072 1/1072 1/1072 1/1072 1/1072
  runs of header map clone                       blocks/bytes: 2/656 2/656 2/656 2/656 2/656
  runs of decode                                 blocks/bytes: 14/626 14/626 14/626 14/626 14/626
  runs of SystemOneResponse<Answers>             blocks/bytes: 14/626 14/626 14/626 14/626 14/626
  runs of naive: #[serde(tag)] answers in a HashMap blocks/bytes: 26/2672 26/2672 26/2672 26/2672 26/2672
  runs of SystemOneResponse<Ticket>, field dispatch blocks/bytes: 10/347 10/347 10/347 10/347 10/347
  runs of prepared() x 1000 with names() walked  blocks/bytes: 0/0 0/0 0/0 0/0 0/0
  runs of SystemOneResponse<Answers>             blocks/bytes: 14/626 14/626 14/626 14/626 14/626
  runs of SystemOneResponse<Review>, derived     blocks/bytes: 10/347 10/347 10/347 10/347 10/347
  runs of Questions::prepare(), runtime example set blocks/bytes: 3/1524 3/1524 3/1524 3/1524 3/1524
  runs of 1 KB string                            blocks/bytes: 1/1408 1/1408 1/1408 1/1408 1/1408
  runs of 64 KB string                           blocks/bytes: 1/68608 1/68608 1/68608 1/68608 1/68608
  runs of 1 MB string                            blocks/bytes: 1/1092608 1/1092608 1/1092608 1/1092608 1/1092608
  runs of 1 MB object                            blocks/bytes: 1/1092696 1/1092696 1/1092696 1/1092696 1/1092696
  runs of mixed: 1 KB #1                         blocks/bytes: 1/1408 1/1408 1/1408 1/1408 1/1408
  runs of mixed: 1 KB #2                         blocks/bytes: 1/1408 1/1408 1/1408 1/1408 1/1408
  runs of mixed: 1 KB #3                         blocks/bytes: 1/1408 1/1408 1/1408 1/1408 1/1408
  runs of mixed: 1 KB #4                         blocks/bytes: 1/1408 1/1408 1/1408 1/1408 1/1408
  runs of mixed: 1 KB #5                         blocks/bytes: 1/1408 1/1408 1/1408 1/1408 1/1408
  runs of mixed: 1 KB #6                         blocks/bytes: 2/743219 2/743219 2/743219 2/743219 2/743219
  runs of mixed: 1 KB #7                         blocks/bytes: 1/1408 1/1408 1/1408 1/1408 1/1408
  runs of mixed: 1 KB #8                         blocks/bytes: 1/1408 1/1408 1/1408 1/1408 1/1408
  runs of mixed: 1 KB #9                         blocks/bytes: 1/1408 1/1408 1/1408 1/1408 1/1408
  runs of mixed: 1 KB #10                        blocks/bytes: 1/1408 1/1408 1/1408 1/1408 1/1408
  runs of mixed: 1 KB #11                        blocks/bytes: 1/1408 1/1408 1/1408 1/1408 1/1408
  runs of mixed: 1 KB #12                        blocks/bytes: 1/1408 1/1408 1/1408 1/1408 1/1408
  runs of mixed: 1 KB #13                        blocks/bytes: 1/1408 1/1408 1/1408 1/1408 1/1408
  runs of mixed: 1 KB #14                        blocks/bytes: 1/1408 1/1408 1/1408 1/1408 1/1408
  runs of mixed: 1 KB #15                        blocks/bytes: 1/1408 1/1408 1/1408 1/1408 1/1408
  runs of mixed: 1 KB #16                        blocks/bytes: 1/1408 1/1408 1/1408 1/1408 1/1408
```

| Criterion | Budget | Measured | Verdict |
| --- | --- | --- | --- |
| AC-P1 encode, 1 KB / 64 KB / 1 MB string, 1 MB object | 1 block, bytes <= 1.05x body + 4 KiB | 1 block each, exactly the body length (1,408 / 68,608 / 1,092,608 / 1,092,696) | holds |
| AC-P1 retained scratch | <= 8x the decayed hint | 4.39x / 5.73x / 5.76x / 5.76x | holds |
| AC-P2 decode, 3 answers | <= 14 blocks, <= 700 bytes, <= 0.7x naive | 14 blocks, 626 bytes, 0.54x | holds, zero block headroom as before |
| AC-P3 derived | fewer than AC-P2 | 10 blocks / 347 bytes | holds |
| AC-P6 a call's own blocks | 19 (18 without retry) | 19 (18); 3,373 bytes where Phase 4 had 3,405 | holds |

The unpinned whole call now costs the same as the pinned one in the dev profile too (22 blocks), because the System
One future over this transport is 2,040 bytes, under tokio's debug box size; before `7493955` it cost 23.

### After the exit gate

The verifier (`p5-verify`, at `acd4df3`) found defects that were fixed forward; each entry names its commit.

**D1: the level hint is bounded (`ae65058`).** Candidate 6's hint is the largest score the request asked, and every
score answer's first level list started at it, empty ones included, with no bound: a server answering many score
answers turned it into memory. `AnswerContext::with_levels` now holds it to 8 (`MAX_LEVEL_HINT`: 5 to 8 levels is the
case the candidate targets), and a list reserves it only at its first entry, so an empty `{}` allocates nothing.
`crates/sdk/tests/alloc_level_hint.rs` asks one score of 1,000 levels, answers 500 empty and 500 one-level scores (a 90,359 B
body), and holds what the response keeps to at most 2x what the same body keeps with no hint, which is what
`f99594e`, before the hint, keeps for both (170,902 B, measured there with the same test):

| Tree | Kept, no hint | Kept, 1,000 levels asked | Ratio |
| --- | ---: | ---: | ---: |
| `f99594e` | 170,902 B | 170,902 B | 1.00 |
| `acd4df3` | 170,902 B | 32,106,902 B | 187.9 (the test fails) |
| the clamp alone | 170,902 B | 362,902 B | 2.12 (the test fails) |
| `ae65058` | 170,902 B | 234,902 B | 1.37 |

The verifier's own probe, re-run on `ae65058`: its 20,002-answer empty flood keeps 2,295,425 B (exactly `f99594e`;
`acd4df3` kept 642,295,425 B), its one-level flood 793,393 B (`f99594e` 569,393 B, `acd4df3` 64,313,393 B), and its
12 wrong-hint cases decode to output identical to both trees. Nothing else moved: the 36 `alloc_*` sections are as
above in dev and release, the 20-question call is still 111 blocks / 8,299 B (a throwaway dhat test of candidate 6's
shape, five equal runs, dev and release), and the call futures are unchanged. Instructions, five runs each on the
Linux host (`acd4df3` -> `ae65058`): `typed` 22,982 -> 22,974, `call::sdk` 33,907-34,441 -> 33,645-34,256,
`call::sdk_20` 193,356-194,152 -> 193,001-193,656, `answers[3]` 23,219-23,928 -> 23,306-23,630, `answers[20]`
184,280-185,307 -> 183,771-184,993. Reserving at the first entry is free only in this form. Against `acd4df3`, five
runs each: the clamp alone cost `typed` +154 (23,136); the clamp with an emptiness check inside the loop cost `typed`
+194 (23,176, so the check itself +40 over the clamp alone) and `sdk_20` +0.4% (min 193,356 -> 194,150); and the
peeled loop with the hint bounded a second time inside the decoder cost `typed` +115 (23,097), because the decoder was
no longer inlined into its hint-0 wrapper. The committed form: `typed` -8 (22,974). The raw callgrind table these
rows come from was not kept (it lived in a temporary session directory); the `decode::typed` rows it held are these,
each the same in all five runs:

| Variant | Form | `decode::typed` Ir | Against base |
| --- | --- | ---: | ---: |
| base | `acd4df3`, no clamp | 22,982 | 0 |
| a | the clamp alone | 23,136 | +154 |
| ab | the clamp and an emptiness check inside the loop | 23,176 | +194 |
| ac | the peeled loop, the hint bounded again inside the decoder | 23,097 | +115 |
| ad | the peeled loop, bounded once (committed in `ae65058`) | 22,974 | -8 |

**D5: observable changes of candidate 6, accepted.** `AnswerContext` (a public type): its `Debug` output gained the
field (`AnswerContext { expected_answers: 0, levels: 0 }`), its alignment went from 8 to 4 on 64-bit targets (the
size stays 8; it goes from 4 to 8 on 32-bit ones), and `Eq` and `Hash` now include the level hint, so contexts of
two requests can differ. `size_of::<PreparedQuestions>()` went from 64 to 72 bytes. No signature, no derive
expansion and no `PartialEq` result of `PreparedQuestions` changed, and no size was ever documented.

**D6: the future-size guard holds the property (`b0cd3f6`).** Its bounds (2,816 / 2,560) could not see tokio's
debug box threshold, which `06573d7` crossed unnoticed (2,064 bytes). Futures over a custom transport are now
asserted <= 2,048 on every platform; futures over the default transport at the sizes measured on macOS arm64 and
Linux x86_64 (2,344 / 2,328 / 2,328 / 2,032, identical on both, dev and release) plus 32 bytes. Targets other than
macOS and Linux (Windows among them) keep the old bounds: they have not been measured.

**CI (`5a51d1f`):** the feature-powerset step runs clippy with warnings denied instead of `cargo hack check`, so a
lint in any of the ten combinations between none and all fails CI. **D8 (`0749117`):** the unused `divan` entry is
gone from `[workspace.dependencies]`; `Cargo.lock` did not change. **D4, D7, N1** are wording fixes in
`crates/sdk/benches/sdk/decode.rs`, `__internals`, and `transport/mod.rs`.

**R17: an 8 MiB ceiling on the retained encode scratch (`codec.rs`, `MAX_RETAINED_SCRATCH`).** Candidate 7 at
1 MiB failed AC-P1's 1 MB rows. A thread kept six times the largest string state it ever encoded, without bound (a
64 MiB state left 402,653,228 B on its thread), released only by later calls on that same thread. A scratch over
8 MiB is now dropped after its call. Every frozen AC-P1 row needs a ceiling of at least 6,291,587 B (the 1 MB object
row's scratch; 6 MiB fails it), so 8 MiB leaves every row as it was: the 20 `alloc_encode` sections print the same
five runs as at `acd4df3`, in dev and in release (diffed). A state whose scratch passes the ceiling (a string over
about 1.33 MiB) pays on every call what a first call pays. B1 gained a 4 MiB row for that case (`prepared[4194304]`);
its wall clock with the ceiling against without it, three runs of 100 samples each, medians:

| Machine | Without the ceiling | With it | Change |
| --- | ---: | ---: | ---: |
| macOS arm64 (load average 6 to 10) | 1.270-1.274 ms | 1.272-1.279 ms | +0.3% |
| Linux x86_64, `taskset -c 2` | 1.813-1.819 ms | 1.871-1.872 ms | +2.9% |

The 1 KB, 64 KB and 1 MB rows moved by less than 1% on both machines. The unit tests
`a_scratch_past_the_ceiling_is_not_kept` (a 2 MiB state keeps 0 B after each of three calls) and
`a_scratch_under_the_ceiling_is_kept` (a 1 MiB state keeps its scratch) pin the rule.

### `compact_str` for names (`821d970`)

Candidate 1, adopted after the owner approved the dependency (`compact_str` 0.10.0, `default-features = false`,
`features = ["std"]`; its `serde` feature is not used). The model name, the answer names, a choice's pick and its option
names are a crate-private `Name` (`crates/sdk/src/name.rs`), which stores up to 24 bytes inline on a 64-bit target. The
legend descriptions are `Content` and did not change. `crates/sdk/src/name.rs` is the only file that names the crate:
the unit test `only_this_module_names_the_small_string_crate` reads every `.rs` file under `crates` and fails on any
other file that does. Swapping the crate out, or going back to `String`, is a change to that file alone. `Name` decodes
through a visitor of its own (`visit_str`, `visit_string`), so the decode never builds a `String` first.

Nothing public moved: accessors return `&str`, `ChoiceAnswer::new` and `Answers: FromIterator` take `Into<String>`,
and `size_of` is 216 / 24 / 64 / 56 bytes for `SystemOneResponse<Answers>` / `Answers` / `Answer` / `ChoiceAnswer`
before and after (pinned in `crates/sdk/tests/static_assertions.rs` for 64-bit targets, `Option` of the last two included). The
test `long_multi_byte_and_escaped_names_decode_serialize_and_print_as_text` decodes names of 24 and 25 bytes,
multi-byte names of 15, 27 and 33 bytes, and names written with escapes. It checks that serializing writes the body
back byte for byte, through the codec and through serde_json, and pins the `Debug` output. The same test, run at
`521736d`, passes with the same expected strings.

**Accepted: a blanket impl on public rustdoc pages.** `compact_str` has
`impl<T: Display + ?Sized> ToCompactString for T`, so rustdoc lists it under "Blanket Implementations" on the 7 public
types that implement `Display`: `ApiError`, `DecodeError`, `EncodeError`, `Error`, `RawJson`,
`ResponseValidationError` and `ContentError`. It is the only difference between the rustdoc JSON of `521736d` and
`ca5f02e` (the Phase 5 verifier's diff, 775 public items each). It cannot be avoided while depending on the crate,
it is the same class as the `tracing` (`Instrument`, `WithSubscriber`) and `zerocopy` blanket impls already listed
there, and it is no API the SDK commits to: no SDK item names the trait.

**Accepted: names of 25 to 31 bytes keep a little more memory.** `compact_str`'s heap buffer holds at least 32 bytes
where a `String` holds exactly its length, so a name just past the inline limit keeps a few more bytes. The Phase 5
verifier's probe, 2,000 answers with three such names each: 628,248 B kept against 588,258 B at `521736d` (+6.8%),
while the blocks fall from 10,003 to 8,002. At 40 and 100 bytes, and for names written with escapes, the tree keeps
the same or less.

**Blocks** (macOS, `alloc_*` tests, five runs each, identical in the dev and the release profile):

| Section | `521736d` | `821d970` |
| --- | ---: | ---: |
| AC-P2 decode, 3 answers (blocks / bytes) | 14 / 626 | **7 / 578** |
| AC-P2 ratio to the naive comparator (26 / 2,672) | 0.54 | **0.27** |
| AC-P3 derived set, hand-written and derived | 10 / 347 | **6 / 314** |
| AC-P6 a call's own blocks (no retry) | 19 (18) | **12 (11)** |
| whole call, pinned or unpinned (no retry) | 22 / 3,373 (21 / 3,349) | 15 / 3,325 (14 / 3,301) |
| AC-P1 encode rows, `mixed` rows, transport, header map | unchanged | unchanged |

```
521736d  runs of decode                                 blocks/bytes: 14/626 14/626 14/626 14/626 14/626
821d970  runs of decode                                 blocks/bytes: 7/578 7/578 7/578 7/578 7/578
521736d  runs of SystemOneResponse<Review>, derived     blocks/bytes: 10/347 10/347 10/347 10/347 10/347
821d970  runs of SystemOneResponse<Review>, derived     blocks/bytes: 6/314 6/314 6/314 6/314 6/314
521736d  runs of whole call                             blocks/bytes: 22/3373 22/3373 22/3373 22/3373 22/3373
821d970  runs of whole call                             blocks/bytes: 15/3325 15/3325 15/3325 15/3325 15/3325
521736d  runs of whole call, no retry                   blocks/bytes: 21/3349 21/3349 21/3349 21/3349 21/3349
821d970  runs of whole call, no retry                   blocks/bytes: 14/3301 14/3301 14/3301 14/3301 14/3301
```

The budgets were tightened to the measurements; none was loosened, and `RUNS` / `AGREE` did not change. The tightened
values are: `alloc_decode` `MAX_BLOCKS` 14 -> 7 and, after the verifier's review, `MAX_BYTES` 700 -> 650 (`03ee23f`:
578 plus 12.5%, close to the 11.8% the first bound had over 626; 700 had left 21%),
`alloc_derive` `ANSWERS_BUDGET` 14 -> 7 and `HAND_WRITTEN_BLOCKS` 10 -> 6, and `alloc_call` `MAX_BLOCKS` 19 -> 12 and
`MAX_BLOCKS_WITHOUT_RETRY` 18 -> 11. AC-P3 is still "fewer blocks than AC-P2": 6 against 7. All of these are 64-bit
numbers, as the budgets always were. On a 32-bit target the inline limit is 12 bytes: the fixture's names still fit,
but a longer name costs a block there that it does not cost here.

**Instructions** (Linux host, callgrind `Ir` as under Method, five runs of the whole `sdk` target per tree, min-max
with the spread as a share of the min):

| Benchmark | `521736d` | `821d970` | Change of the min |
| --- | ---: | ---: | ---: |
| `decode::answers[3]` | 23,368-24,091 (3.1%) | 22,886-23,110 (1.0%) | **-2.1%**, ranges apart |
| `decode::answers[20]` | 183,795-185,041 (0.7%) | 169,898-170,586 (0.4%) | **-7.6%**, ranges apart |
| `decode::typed` | 22,957 (0.0%) | 22,883 (0.0%) | -0.3% |
| `call::sdk` | 34,000-34,216 (0.6%) | 32,628-33,446 (2.5%) | **-4.0%**, ranges apart |
| `call::sdk_20` | 193,459-194,334 (0.5%) | 178,336-179,081 (0.4%) | **-7.8%**, ranges apart |
| `call::naive` / `call::floor` | 64,663-66,198 / 2,526 | 64,629-66,595 / 2,526 | overlap / 0 |

No other benchmark's range lies above its old range, apart from two that do not reach a name.
`retry::retry_after[seconds]` goes from 897 to 898 and `retry_after[date]` from 1,773 to 1,774 (+1 instruction,
+0.1%, in all five runs). Their per-function counts show where: `http`'s `HdrName::from_bytes` (102 + 75 -> 101 + 76)
and one more instruction elsewhere in the `http` header lookup. That is the code-layout effect of a rebuild described
under Method, not code this change runs. The encode, assembly and retry benchmarks overlap their old ranges.

Commands: the alloc tests as under "Final AC-P1 / AC-P2 / AC-P3 / AC-P6 check", at `521736d` and at `821d970`, dev
and release. On the Linux host there were two scratch clones, one at `521736d` and one with `821d970`'s diff applied.
Each was built with `cargo codspeed build -m simulation -p typesafe-sdk-rust --features internals --bench sdk`, and
the callgrind command under Method was run on each tree five times, pinned to one core, one run at a time. Both
clones were removed afterwards.

### AC-P7: proven on CodSpeed's runner

| Item | Evidence |
| --- | --- |
| First pull-request run (`ca5f02e`) | run 35428694878: build ok, then CodSpeed's runner stopped with "Unsupported system" on `ubuntu-26.04`. Its valgrind setup accepts Ubuntu 22.04 / 24.04 and Debian 12 only (`CodSpeedHQ/runner`, `src/executor/valgrind/setup.rs`, runner 5.2.1 and 5.3.1) |
| Image pin | `a5a0795`: the `codspeed` job alone runs on `ubuntu-24.04`, the newest GitHub-hosted image the runner supports, with the reason and the condition for going back beside the label (wording made exact in `15952c4`) |
| Proof | run 35429672326 at `a5a0795`, on `ubuntu-24.04`: 35 benchmarks measured, uploaded through OIDC, and CodSpeed's check reports the app installed. `call::sdk` 108.8 µs against `call::naive` 159.3 µs (**0.68x**), `call::sdk_20` 233 µs against `call::naive_20` 511.3 µs (**0.46x**). These are CodSpeed's simulated times (instruction counts with its cache and cycle estimate), checked by the Phase 5 verifier, not measured on the Linux host |
| `push` trigger | added after that run: `bench.yaml` now also runs on every push to `main`, the baseline CodSpeed compares pull requests with. A `main` run is never cancelled by a newer one (`cancel-in-progress` is off for `refs/heads/main`, as in `ci.yaml`) |

AC-P7's condition, "the instruction count of the SDK full-call benchmark is lower than the naive comparator's", holds on
CodSpeed's runner as it held on the Linux host (0.51x to 0.52x in raw instructions there). CodSpeed reports estimated
times rather than raw instructions, so its ratios are not expected to equal the host's; why the 3-question ratio is
higher there (0.68x) was not measured.

### Unmeasured

- Instruction counts on arm64 (callgrind is not available for macOS arm64).
- Comparator (B), `typesafe-rs` 0.1.0 (a new crate).
- Wall-clock numbers on an idle macOS machine: every macOS table here was taken under a load average of 8 to 15 from
  other sessions.
- Instruction counts of `loopback` (kept out of the instrumented run on purpose).
- The R17 ceiling at any value other than 1 MiB and 8 MiB, and under a musl or jemalloc allocator.
- The name budgets on a 32-bit target (inline limit 12 bytes), and `compact_str`'s instruction counts on arm64.
- Future sizes on targets other than macOS and Linux (the size guard keeps loose bounds there).

## Phase 6 - hardening

### F1: a body that is not UTF-8 is refused before the parser reads it (`ab5c4b3`)

**What.** Fuzzing `decode_response` found, after 41 executions, that bytes that are not UTF-8 inside a JSON string
reached sonic-rs 0.5.10's `as_str` (`src/parser.rs:104`), which is a `debug_assert!` followed by
`from_utf8_unchecked`: a panic with debug assertions, an invalid `&str` handed to serde without them. `from_slice`
records the first bad byte up front but raises the error only after deserialization has finished. The SDK reached it
through the error-body reader (any non-2xx whose `RawJson` members hold such a string) and through the path-tracking
second pass of `decode_seed` (a 200 whose score legend value is an object).

**Where and the fix.** `crates/sdk/src/codec.rs`: `as_text`, one `std::str::from_utf8` (no allocation) at the two places every
body enters the codec, `decode` and `decode_seed`, before the depth pre-scan. The checked `&str` then goes to
`sonic_rs::from_str` / `Deserializer::from_str`, which skip sonic's own UTF-8 pass, and `describe_failure` takes the
same `&str`. So a body is validated once: a `decode_seed` body was validated twice before. A failure is a
`DecodeErrorKind::Syntax` at the first bad byte.

**Cost, as measured.**

- Allocations: the five `alloc_*` tests pass unchanged; no budget or assertion moved. The check allocates nothing.
- Wall clock, divan on macOS arm64 (`cargo bench --features internals,macros --bench sdk`), two alternating rounds
  per tree (fix, base, fix, base), one at a time. Medians:

  | Bench | fix r1 / r2 | base r1 / r2 | Delta range |
  | --- | --- | --- | --- |
  | `decode::answers[3]` | 1.332 / 1.291 us | 1.322 / 1.270 us | +0.8 to +1.7% |
  | `decode::answers[20]` | 9.999 / 9.666 us | 9.729 / 9.249 us | +2.8 to +4.5% |
  | `decode::typed` | 1.239 / 1.207 us | 1.291 / 1.239 us | -2.6 to -4.0% |
  | `call::sdk` | 2.291 / 2.270 us | 2.291 / 2.207 us | 0 to +2.9% |

  Round-to-round drift within one tree reaches 4.9% (base `answers[20]`: 9.729 to 9.249 us), so none of these moves
  is resolved.

**Not measured.** Instruction counts on the Linux host: none were taken, and the host was retired, so CodSpeed's run is
the Linux evidence. The expected delta is small (std's UTF-8 pass replaces sonic's for most bodies). Nor was any
fuzzing done on x86_64, where sonic-rs picks other SIMD paths.

## S7 - serde_json as the default backend

Since 0.2.0, serde_json 1.0.151 is the default backend; `sonic` selects sonic-rs 0.5.10.
The implementation measured here is `873f431`. Measurements began at 2026-09-24 03:09:13 JST
(the measurement command's `date` output), on Darwin arm64 with rustc 1.98.1 and cargo 1.98.1.
`RUSTFLAGS` and `TYPESAFE_API_KEY` were unset for every cargo invocation. No benchmark ran in
parallel with these measurements, and no CPU-specific flags were used.

The earlier sonic numbers in S1, S6 and Phase 5 remain unchanged as historical measurements.
The direct-engine comparison under **sonic-rs and serde_json, no flags (`fd22977`)** is the
existing throughput evidence, including the large-body encode advantage. No new SDK wall-clock
or instruction-count comparison is claimed here. CodSpeed's default build now selects serde_json;
its next default-backend result is not a like-for-like continuation of the old sonic build.

### Three allocation runs per backend

All five allocation tests ran three times per backend, serially. Encode, decode, call and derive
retain their five-sample minimum after warming the identical operation; their repeated samples
agreed. The level-hint test measures one warmed response per hinted/unhinted case in each run.
Reproduce with the pinned toolchain and a dedicated target directory:

```bash
export CARGO_TARGET_DIR="$HOME/.cache/rust/target-main"
for backend in sonic serde; do
  if [ "$backend" = sonic ]; then
    features=(--all-features)
  else
    features=(--features internals)
  fi
  for round in 1 2 3; do
    for test in alloc_encode alloc_decode alloc_call alloc_derive alloc_level_hint; do
      env -u RUSTFLAGS -u TYPESAFE_API_KEY cargo --config ~/.config/rust/config.dev.toml \
        test "${features[@]}" --test "$test" -- --nocapture
    done
  done
done
```

In the next table, `blocks/bytes` means the dhat allocation delta, not retained capacity.

| Measurement | sonic r1 / r2 / r3 | serde_json r1 / r2 / r3 |
| --- | --- | --- |
| 1 KB string encode, blocks/bytes | 1/1408; 1/1408; 1/1408 | 1/1408; 1/1408; 1/1408 |
| 64 KB string encode, blocks/bytes | 1/68608; 1/68608; 1/68608 | 1/68608; 1/68608; 1/68608 |
| 1 MB string encode, blocks/bytes | 1/1092608; 1/1092608; 1/1092608 | 1/1092608; 1/1092608; 1/1092608 |
| 1 MB object encode, blocks/bytes | 1/1092696; 1/1092696; 1/1092696 | 1/1092696; 1/1092696; 1/1092696 |
| `Answers` decode, blocks/bytes | 7/578; 7/578; 7/578 | 7/578; 7/578; 7/578 |
| Hand-written `Ticket` decode, blocks/bytes | 6/314; 6/314; 6/314 | 6/314; 6/314; 6/314 |
| Derived `Review` decode, blocks/bytes | 6/314; 6/314; 6/314 | 6/314; 6/314; 6/314 |
| Whole call's SDK-owned blocks, retry / no retry | 12/11; 12/11; 12/11 | 12/11; 12/11; 12/11 |
| Derived `prepared()`, first / repeated blocks | 0/0; 0/0; 0/0 | 0/0; 0/0; 0/0 |
| Runtime `Questions::prepare`, blocks/bytes | 3/1524; 3/1524; 3/1524 | 3/1524; 3/1524; 3/1524 |
| Score level-hint retained-memory ratio (bound 2.0) | 1.390; 1.390; 1.390 | 1.390; 1.390; 1.390 |

**The five tests' allocation budgets are equal under both backends.** No numeric budget was
loosened and no duplicate per-backend constants are needed: decode remains 7 blocks / at most
650 bytes (578 measured plus the existing 12.5% headroom), the struct set costs 6 blocks, and
the SDK's own call costs at most 12 blocks or 11 without retry retention. Steady encode remains
exactly one block and at most `1.05 * body_bytes + 4096` bytes. The level-hint bound remains 2.0.
The naive-comparator ratio assertion remains unchanged in both ordinary backend runs.

### Retained scratch and the mixed-size fixture

The scratch policy is shared and unchanged: an 8 MiB retention cap, a hint that decays by
one sixteenth per call, and one shrink to the hint when capacity exceeds eight times the hint.
The engines reserve differently, so capacity and the call that crosses that threshold differ.
All three runs of each backend produced these capacities:

| State | sonic retained bytes | serde_json retained bytes |
| --- | ---: | ---: |
| 1 KB string | 6,188 | 2,304 |
| 64 KB string | 393,260 | 73,728 |
| 1 MB string | 6,291,500 | 1,179,648 |
| 1 MB object | 6,291,587 | 1,179,648 |

The six-fold string reservation and the roughly 1.33 MiB state threshold that exhausts the
8 MiB cap describe sonic-rs, not serde_json. serde_json's retained buffer grows with the encoded
body, rounded up by `Vec` growth.

The mixed fixture keeps sonic's original 16 small calls and every original assertion. For
serde_json it uses 33: **shrink at 32, steady at 33**. At call 32 the serde_json result is
2 blocks / 139,946 bytes, with capacity and hint both 138,538; at call 33 it is back to
1 block / 1,408 bytes, capacity 138,538 and hint 129,880. Sonic still shrinks at call 6,
2 blocks / 743,219 bytes, capacity and hint 741,811. The different sequence lengths exercise
the same post-shrink condition, not different allocation limits. A 16-call serde_json sequence
correctly stays within the 8x bound but has not shrunk; ending on call 32 observes the shrink's
extra block rather than the following steady call.

Three unit fixtures also distinguish reservation behavior without changing their names or the
production policy:

| Test | sonic parameter / bound | serde_json parameter / bound |
| --- | --- | --- |
| `an_outlier_body_stops_pinning_the_scratch` | 16 small calls | 32 small calls |
| `a_scratch_past_the_ceiling_is_not_kept` | 2 MiB state crosses the cap through the six-fold reserve | 8 MiB + 1 byte state crosses the cap directly |
| `a_scratch_under_the_ceiling_is_kept` | capacity > 6 * state length, and <= 8 MiB | capacity >= encoded string length (state + 2), and <= 8 MiB |

### Backend behavior and positions

The codec seam is `crates/sdk/src/codec/backend.rs`. SDK decode keeps raw wire text verbatim; a caller's
serde_json reload renders it back to compact JSON. The synchronous `DecoderMark` is entered
inside `decode` and `decode_seed`, spans the failure re-read, nests by counter and clears on
unwind. It identifies an SDK decode on the current thread, not an arbitrary deserializer's type.

`DecodeError` continues to expose the SDK's kind and field path without backend text. The
512-case path comparison at the original `codec_tests.rs:382` passes through the selected
backend's deserializer. These existing tests retain their names except the two explicitly named
negative-zero replacements:

| Case | sonic SDK build | serde_json SDK build |
| --- | --- | --- |
| Literal `-0.0` decoded as f64 | bits 0; `a_literal_negative_zero_loses_its_sign_with_sonic` | bits 0x8000000000000000; `a_literal_negative_zero_keeps_its_sign` |
| `-1e-400` underflow | negative zero | negative zero |
| Raw `-0.0` transcoded by `a_negative_zero_on_the_transcode_path_is_written_as_the_backend_reads_it` | `0.0` | `-0.0` |
| Same codec wrapped outside the SDK encoder, field `a` | `0.0` | `-0.0` |
| Empty raw/content input | line 1, column 1 | line 1, column 0 |
| Derived answer set receives `[]` instead of an object | `answers`, column 79 | `answers`, column 77 |
| Standalone answer `[1]`, read by the other engine | serde_json reference: missing `type`, column 1 | sonic reference: missing `type`, column 2 |
| Unencodable raw-question tuple key | `Expected the key to be string/bool/number when serializing map, now is tuple` | `key must be a string` |
| Failing foreign I/O writer | serde_json names `the sink is full` | sonic names `io error while serializing or deserializing` |

Raw splicing remains byte-exact for both backends, including whitespace, order and negative zero.
The foreign-codec tests compare the visitor message and position without sonic's appended input
excerpt. `EncodeError::message()` remains the selected serializer's text, so its exact wording is
asserted only under the matching feature selection.

The overflow probe, run before changing differential expectations, found both engines refusing
`1e400`, `-1e309`, `[1e400]` and `[-1e309]`. The error positions are respectively 1:5, 1:6, 1:6
and 1:7 for both. Thus those cases remain agreement by mutual refusal, not a one-sided divergence.
The differential suite compares against serde_json under `sonic` and sonic-rs otherwise, with
10,000 documents and 10,000 states per run. Its negative-zero normalization follows that direction.

### Unified arbitrary_precision

A separate full test run enables `internals,serde_json/arbitrary_precision` and sets
`TYPESAFE_SDK_TEST_ARBITRARY_PRECISION=1` in that command's environment only. The tests compare
this switch with a real `1E2` parse/serialize probe: `100.0` normally, `1e+2` under the feature;
an accidental feature unification therefore fails rather than silently changing expectations.

A caller's serde_json reload keeps its scanned numeric text under this feature: `0.50` stays
`0.50`, `1E2` becomes `1e+2`, and integer `-0` becomes `0`. The ordinary reload is canonical
(`0.5`, `100.0`, `-0.0`). Transcoding still uses u64, i64, then finite f64: the 23-digit integer
`12345678901234567890123` becomes `1.2345678901234568e+22` in every tested build. A non-finite
number token is refused by the transcoder; raw rendering can preserve `1e400` as text.
The combination of `sonic` with a caller's arbitrary_precision feature is not a tested run.

Two explicit test adjustments apply only to the probed arbitrary-precision run:

- The differential probe's `[1e400]` error is `Data`, not `Syntax`: the parser yields a number
  token and the visitor rejects infinity with a custom error. Other syntax cases stay `Syntax`,
  and none of these errors is excused by the SDK-refusal classifier.
- The unchanged naive comparator uses an internally tagged serde derive. Its buffered floats
  become token maps that its f64 fields cannot read, so only that comparator section and the
  ratio-to-naive assertion are omitted, with the line
  `naive comparator skipped: serde_json arbitrary_precision turns floats into token maps an internally tagged derive cannot read`.
  SDK block/byte bounds, typed-set comparison and field equality remain asserted. No test is
  ignored and the whole run still passes 495 tests; the CodSpeed comparator type is not changed.

### Coverage and scope

At `873f431`, the test totals are 495 with all features, 356 without defaults, 495 with internals
and 495 with arbitrary_precision. The no-default count is 441 + 12 - 97: 97 existing tests need
the built-in hyper transport and remain in the all-features suite, rather than being deleted.

Fresh merged coverage from sonic and serde_json is **96.09%**: 4,500 lines, 176 missed;
7,079 regions, 395 missed; 772 functions, 33 missed. Each instrumented SDK-only run passes
449 tests. The commands and all uncovered source lines are in [`../uncovered-lines.md`](../uncovered-lines.md).
The arbitrary-precision run is tested separately, not merged into that coverage report.

## The SDK package moves to `crates/sdk` (2026-09-30)

The commit that adds this entry moves the SDK package from the repository root to `crates/sdk` and changes no code
the benchmarks measure. It does change what CodSpeed calls each benchmark. codspeed-divan-compat 5.0.2 names a
benchmark `<file>::<module path>::<name>` (`src/compat/uri.rs`, `format!("{file}::{module_path}{bench_name}")`),
and `<file>` is the source file of the benchmark as `file!()` gives it: the path rustc was handed, relative to the
workspace root. A benchmark of the `sdk` target was `benches/sdk/decode.rs::...` before the move and is
`crates/sdk/benches/sdk/decode.rs::...` from this commit on.

**Measured:** the new form is what the binary holds. Checked on a release build of the bench target, without `--config`
and with `RUSTFLAGS` cleared (CI builds it with `cargo codspeed build`, from the same sources and the same workspace
root):

```sh
env -u RUSTFLAGS cargo bench --no-run --features internals --bench sdk
strings <the sdk bench binary> | rg -c 'crates/sdk/benches/sdk/'                                  # 19 lines
strings <the sdk bench binary> | rg -o 'crates/sdk/benches/[A-Za-z0-9_/.]+\.rs' | sort | uniq -c  # 20 paths
strings <the sdk bench binary> | rg -c '^benches/sdk/'                                            # no match
```

The 19 lines hold 20 paths (one line holds both `retry.rs` and `encode.rs`), the locations of benchmarks and of panics:
the six module files of the target (`assembly.rs` 2, `call.rs` 2, `decode.rs` 5, `encode.rs` 4, `retry.rs` 2,
`service.rs` 3) and the two support modules that `main.rs` includes by `#[path]`, embedded as
`crates/sdk/benches/sdk/../support/naive.rs` 1 and `crates/sdk/benches/sdk/../support/mod.rs` 1.
`crates/sdk/benches/sdk/main.rs` itself only declares the modules. No line holds the old form.

**Expected, not observed yet:** CodSpeed matches a benchmark across runs by its identifier, so it should show the
benchmarks of this commit as new ones with no history before it, and the history under the old identifiers should end
at the commit before. To be confirmed on the first run of `main` after this commit.
