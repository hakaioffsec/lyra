# Development handoff — correctness, scale, and recovery baselines

## LLVM 23 compatibility experiment

LLVM 23 compatibility is a general Lyra goal, independent of any consumer.
The experimental Rust 1.100.0 / LLVM 23 API port is retained separately.
This branch keeps the LLVM 22 implementation and its correctness fixes.
All six obfuscation passes remain available on the supported LLVM 22
pipeline; validation is not relaxed.

The API port builds, passes 16 Cargo bin tests, and passes the startup-policy
preflight 6/6 on the validation Mac (Linux startup fixtures run in Docker).
It adapts the split `UncondBr`/`CondBr` opcodes, conditional-branch operand order,
and `CodeExtractor`'s added deallocation-block argument. These successes do
**not** establish full support: stock LLVM 23.1.6 hangs compiling legal
indirect-branch IR in CodeGenPrepare. No released fix was found at the time
of the experiment. Native tooling and library qualification on Ubuntu, macOS,
and Windows remains required; untested targets are not claimed compatible.

### Portable reproducer

Save these 14 lines as `minimal.ll`:

```llvm
target triple = "x86_64-unknown-linux-gnu"

define ptr @stall(ptr %target) {
entry:
  %base = alloca [32 x i8], align 8
  %p8 = getelementptr i8, ptr %base, i64 8
  indirectbr ptr %target, [label %dest]
dest:
  %v8 = load i64, ptr %p8, align 8
  %p16 = getelementptr i8, ptr %base, i64 16
  %v16 = load ptr, ptr %p16, align 8
  %p24 = getelementptr i8, ptr %base, i64 24
  ret ptr %p24
}
```

With stock LLVM 23.1.6's `opt` and `llc` on PATH, this Python 3 diagnostic
bounds compilation to 10 seconds on every host (no GNU `timeout` dependency):

```sh
opt -passes=verify -disable-output minimal.ll
python3 -c 'import subprocess; subprocess.run(["llc", "-O2", "-filetype=obj", "-relocation-model=pic", "minimal.ll", "-o", "minimal.o"], check=True, timeout=10)'
```

The verifier accepts the IR; the affected `llc` times out. In
`tryUnmergingGEPsAcrossIndirectBr`, the static-allocation offset 16 is rebased
onto `%p8 + 8`; `optimizeMemoryInst` folds it back. Identical IR recurs in
iterations 2/4/6. The original IR and each of the other five passes compile
quickly; indirect-branch alone reproduces the cycle. This is distinct from
the LLVM 22 negative-offset `nuw` poison bug: the release fix still removes
`nuw` on GEPs in rewritten functions, but the minimal cycle has no wrap flags,
so removing `nuw`, `nusw`, or `inbounds` cannot fix it.

Disabling all of CodeGenPrepare is diagnostic only, not an accepted support
strategy or release workaround. It completed the 208-line witness in 0.024 s,
versus a normal 10 s timeout (the full build exceeded an hour); these are
compiler diagnostics, not runtime performance measurements. The scratch
`static-alloca-unmerge.patch` is a candidate only, not a qualified compiler
distribution. No global LLVM installation was patched and no pass disabled.

The candidate LLVM change excludes static-allocation bases from indirect-branch
GEP unmerging: such a base is already treated as always live by the addressing
mode matcher, so the claimed register-liveness benefit does not apply.
It still needs upstream review and cross-platform compiler qualification.
The experimental commit and toolset binaries, original IR, reduced fixture,
and instrumentation trace are preserved separately. The portable reproducer
above does not require machine-local files or any downstream project.

## Start here

**Fix existing-pass correctness before adding passes or increasing complexity.**
The floating-point fix and a scoped integer MBA audit are complete on native
Linux x86-64. Integer MBA now requires proven-defined operands; this deliberate
coverage reduction avoids undef corruption and an LLVM 22.1.8 lowering defect.
The measured baseline for existing passes is complete on the public Linux
fixtures. The main observed runtime cost is indirect branching; MBA's conservative
eligibility remains binding. Two entry-cache variants and a seeded volatile-target
dispatch candidate failed their predeclared gates and were rolled back. The
original per-branch lowering is still shipped. PMU observations repeatedly show
that lower instruction counts need not reduce cycles. The earlier whole-image
ASLR control did not explain the cache regression; no exact predictor cause is
claimed. Do not revive a rejected design on instruction count alone.
A first real-workspace baseline and blinded recovery pilot are now complete:
ten selected rust-analyzer libraries build and pass scoped native checks, while
the model recovered both plain and all-pass versions of the small integer leaf.
The scale work also fixed silent hyphenated-crate bypass and LLVM long-name
corruption. See [scale](#real-workspace-scale-baseline),
[correctness fixes](#scale-preparation-correctness-fixes), and
[the recovery pilot](#blinded-semantic-recovery-pilot).
The new opt-in [indirect-call pass](#indirect-call-pass-s2) is implemented and
measured separately. Its narrow static call-target effect is not a claim that
the broader LLM-resistance goal is solved.
The opt-in [block-outlining pass](#block-outlining-pass-a1) now adds bounded
internal call boundaries before S2. It passes scoped Linux correctness, cost and
large-workspace checks; standalone outlining was still fully recovered.
The [A3 forwarding-wrapper experiment](#function-wrapper-experiment-a3) was
rejected: its extra calls survived, but automated binary analysis removed the
added layer and its hot-work runtime exceeded the declared ceilings. No new
production flag or pass was added.
The [A4 constant-specialisation experiment](#constant-specialisation-experiment-a4)
also failed its incremental matching gate despite correct surviving variants.
The [MBA return-contract experiment](#mba-return-contract-experiment) was rejected
after a candidate-only LLVM lowering failure, despite focused refinement proofs.
The shipped conservative operand policy and floating-point exclusion remain
unchanged. The [B2 switch-lowering experiment](#custom-switch-lowering-experiment-b2)
also remains unshipped: late placement passed the scoped cost gates and hid
static case mappings, but all eight blinded conditions were fully recovered.
The [B3 address-offset experiment](#address-offset-materialisation-experiment-b3)
was rejected on all twelve native text-growth comparisons despite passing its
focused correctness checks. The [B4 byte-alias experiment](#byte-alias-access-experiment-b4)
also failed every text-growth gate; its native accesses only partially recombined.
The [C1 parameter-bundle experiment](#parameter-aggregation-experiment-c1) passed
its scoped correctness and cost gates, but every tested bundle leaf was
mechanically matched to its scalar counterpart. It remains unshipped.
The [C2 register-lane experiment](#register-lane-calling-convention-experiment-c2)
passed native ABI checks but failed two composition runtime gates and remains
unshipped. The [D2 constant-reconstruction experiment](#constant-reconstruction-experiment-d2)
passed native and size checks but failed four runtime gates; its full LLVM
refinement also remains incomplete. The [D3 quadratic-residue experiment](#quadratic-residue-predicate-experiment-d3)
passed its probe correctness checks, but binary-only analysis removed every
tested added predicate/selector component. It stopped before an S1 rewriter.
The [bounded D1 order experiment](#bounded-pipeline-order-experiment-d1) passed
correctness/cost checks but was not promoted. Its complete schema-parsed rerun
recovered 15/16 conditions at each endpoint, including all eight six-pass
conditions; exact model identity remains unavailable. The
[function-layout experiment](#function-layout-experiment-y2) changed native
placement but every tested body remained anonymously matchable, so it was
rejected. The [bounded S3 register VM](#bounded-register-vm-experiment-s3) passed
its scoped execution checks but failed composition text-growth gates.
The listed inventory now has bounded decisions, not product acceptance; no
automatic reorder, VM API or BCF upgrade follows.

The product goals now explicitly include good runtime/build/memory performance,
real Rust workspace support above **300,000 LoC**, and making semantic recovery
very difficult for LLM-led analysts even when they have CPU emulators,
disassemblers, decompilers and execution access. These are **acceptance targets,
not current claims**. LoC must be counted reproducibly with generated/dependency
code separated; a real-project build/run is required. LLM resistance needs
blinded tasks and held-out behavioral scoring, not a decompiler screenshot.
See [the roadmap acceptance pillars](../ROADMAP.md#product-goal-and-acceptance-pillars).

Execution order: preserve correctness → use the measured performance, scale and
recovery gaps to choose one bounded improvement → broaden workloads and native
platform checks. The initial baselines do not complete the expanded M2 goals.
No broad performance/scalability/resistance claim is justified by these results.

Read [ROADMAP.md](../ROADMAP.md) for scope and acceptance gates, and
[README.md](../README.md#ci-and-regression-checks) for the current build workflow.
This handoff records the completed Linux and Apple Silicon macOS checks and
retained counterexamples. It does not claim native Windows verification,
universal equivalence, or a connected IDA/Ghidra MCP bridge. Direct IDA/idalib
export and scoped Alive2 checks are recorded below.

## Native macOS validation

An isolated Apple Silicon macOS 27 workspace built all three Lyra release
binaries with Rust 1.98.1 and Homebrew LLVM 22.1.8. The native Cargo suites pass
15 tests, and `scripts/ci_smoke.py --suite full` passes all 78 executable,
cdylib, workspace, ABI, name-boundary, individual-pass and composed-pass cases.
Linux regression verification still passes all 16 Cargo tests.

The first macOS run reproduced three MCJIT lookup failures in the indirect-call
and block-outlining tests, including an exclusion fixture with no transformed
calls. Their width-only data layout omitted Mach-O mangling. The executable
fixtures now obtain both the native triple and data layout from LLVM before
transformation and JIT execution; deliberate non-native layout boundary tests
remain unchanged. No production pass behavior or assertion was weakened.

Evidence is retained on the validation Mac beneath
`~/Projects/lyra-ready-e487c40b/`: `lyra-validation.json`,
`logs/rust-tests.log` (before), `logs/rust-tests-corrected.log` (after), and
`native-smoke-1791346745/report.json`. This native fixture coverage alone does
not establish compatibility with every embedding application or Windows runtime.

## What changed, and what did not

- Removed all nine floating-point MBA rewrites, their selection/dispatch, and
  obsolete float coverage claims. `--mba` leaves floating-point IR unchanged.
- Kept the existing integer identities and pass order. Removing float targets
  changes RNG consumption, so pre-fix seeded artifact bytes are not preserved.
- Added six C-ABI float exports to `test_dll_project` and exact-result checks to
  `scripts/probe_cdylib.py`, reused by the existing smoke/full runner.
- Established the pinned Linux baseline; the old fixture suites passed despite
  the float bug. The new regression failed before the fix and passes afterward.
- Restricted integer MBA to proven-defined operands after reproducing an undef
  bug. The user selected conservative eligibility over a compiler patch or new
  operand-once recipes. The existing 15 identities remain; no freeze is added.
- Added two native JIT regressions, a pinned local Alive2 build, and a working
  Linux IDA exporter at `scripts/export_ida.py`.
- Added reproducible build/runtime/startup, size, and optional pass-coverage
  measurements, plus stripped-artifact IDA inspection with numeric selectors,
  CFG observations, and recorded settings.
- No pass eligibility, identities, RNG consumption, or pass order changed in the
  measurement work. Broader applications, native cross-platform checks, and
  claims of analyst resistance remain outside this baseline.
- Added a third JIT regression for indirect-branch loops, source volatile memory,
  recursion, a bare entry branch and `musttail`. No experimental entry predicate
  or target cache remains in the production pass.

### Current implementation facts

| Item | Current state |
|---|---|
| Passes | `--string-enc`, `--block-outline`, `--shuffle-blocks`, `--indirect-branch`, `--indirect-call`, `--mba`; all opt-in. |
| Execution order | String encryption → block outlining → block shuffle → indirect branch → indirect call → MBA, fixed in `src/main.rs`. |
| Seed | `--seed <u64>` controls per-pass RNG streams for the same module inputs; pass order is not randomised. Final artifact reproducibility needs separate verification. |
| Removed passes | Legacy instruction substitution, BCF, and CFF are absent. There is no `--substitution` flag or implicit BCF block-splitting dependency. |
| String lifecycle | Eligible globals are decrypted in place at startup. On-disk plaintext absence is not runtime confidentiality. |
| Branch coverage | Eligible `br` terminators are transformed; `switch` terminators remain. Shared eligibility rules skip EH-pad functions and other constructs. |
| Call coverage | Ordinary calls to defined private/internal targets, excluding unsupported ABI/metadata/address-space/layout shapes. Original call instructions and their ABI attributes are preserved; target encoding is not encryption. |
| Outlining | At most one original reachable, non-entry, acyclic call-free block/function; ≥8 non-PHI/non-terminator instructions, ≤6 live-ins and ≤2 live-outs. Internal `noinline` helper; unsupported functions/types and unselected eligible blocks have explicit skip reasons. |
| MBA | Scalar integer add/sub/xor/and/or at widths above one bit with proven-defined operands; three variants per opcode, two rounds, 60% selection probability per round. Unproven operands and floating-point instructions are unchanged. |

Source entry points: `src/obfuscator/mba.rs`, `src/obfuscator/indirect_branch.rs`,
`src/obfuscator/indirect_call.rs`, `src/obfuscator/string_encryption.rs`,
`src/obfuscator/shuffle_blocks.rs`, `src/obfuscator/seed.rs`,
`src/obfuscator/util.rs`, and `src/obfuscator/block_outline.rs` / `.cpp`.

## Resolved floating-point blocker

Started from clean revision `07a89315264391149f3ef81661da16116e3f15c5`.
Native tools: Rust **1.98.1** (`48a229cea`), LLVM/Clang **22.1.8**,
Python **3.14.7**, target **x86_64-unknown-linux-gnu**. Rust 1.98.1 with rustfmt
was installed; `LLVM_SYS_221_PREFIX=/usr` and `LYRA_LLVM_BIN=/usr/bin` were used.

The retained minimal module uses runtime C-ABI operands, not folded constants.
With `LYRA_SEED=0`, `lyra __obfuscate-ir ... --mba` made six substitutions in
three functions over two rounds. Emitted IR confirms the old `2*a + (b-a)`
addition variant fired (its subtraction was expanded again in round two).
Both original and transformed IR were compiled with `clang -shared -fPIC -O2`
and called natively via `ctypes`; `opt -passes=verify` accepted the transformed
IR despite the semantic failures:

| Operation / binary64 inputs | Expected and original | Before fix | After fix |
|---|---|---|---|
| `1e308 + -1e308` | `+0.0` | NaN | `+0.0` |
| `1e308 - 1e308` | `+0.0` | NaN | `+0.0` |
| `1e308 * 1.0` | `1e308` | `+infinity` | `1e308` |

All nine former variants were also transcribed into a separate LLVM module,
verified, compiled, and called natively with the following counterexamples.
This is a counterexample audit, not an Alive2 equivalence proof.

| Former variant (zero-based) | Inputs `(a, b)` | Expected | Actual |
|---|---|---|---|
| FADD 0: `2*a + (b-a)` | `(1e308, -1e308)` | `+0.0` | NaN |
| FADD 1: `a + (2*b-b)` | `(1e308, -1e308)` | `+0.0` | `-infinity` |
| FADD 2: `(a-b) + 2*b` | `(1e308, -1e308)` | `+0.0` | NaN |
| FSUB 0: `2*a - (a+b)` | `(1e308, 1e308)` | `+0.0` | NaN |
| FSUB 1: `(a+b) - 2*b` | `(1e308, 1e308)` | `+0.0` | NaN |
| FSUB 2: `(2*a-b) - a` | `(1e308, 1e308)` | `+0.0` | `+infinity` |
| FMUL 0: `(a+a) * (b*0.5)` | `(1e308, 1.0)` | `1e308` | `+infinity` |
| FMUL 1: `(a*0.5) * (b+b)` | `(1.0, 1e308)` | `1e308` | `+infinity` |
| FMUL 2: `(a*2) * (b*0.5)` | `(1e308, 1.0)` | `1e308` | `+infinity` |

The permanent native regression calls `f32`/`f64` add/sub/mul with 94 explicit
cases across overflow, cancellation, rounding, signed zero, subnormals,
infinities, and NaNs. It compares exact bits for non-NaNs; NaN payload/sign are
deliberately not part of the oracle. No tolerance or input special case was used.

### Linux verification and retained evidence

Local evidence root: `target/linux-handoff.yKIxUF/` (ignored; preserve it before
cleaning builds). The source fixtures and probe are the permanent regression,
not ignored `.ll` files.

| Check | Result / evidence |
|---|---|
| Original baseline | Formatting, locked binary build and Cargo tests passed; that baseline had zero unit tests. `baseline-smoke-configured/report.json`: 7 passed; `baseline-full/report.json`: 39 passed. |
| New regression, old implementation | `regression-before-full/report.json`: 33 passed, 6 failed. Only shared-library MBA and all-pass profiles failed, at seeds 0, 1, 42; failures include NaN or `-infinity` instead of zero for `f32` cancellation. Failed binaries and transformed Rust IR remain under each case. |
| Fixed implementation | Formatting (root and cdylib fixture), locked binary build and Cargo tests passed. `fixed-smoke/report.json`: 7 passed; `fixed-full/report.json`: 39 passed, including individual passes and the combined pipeline. |
| Focused native reproduction | `float-original.ll`, `float-before.ll`, `float-after.ll`, corresponding `.so` files, command logs, and `repro-before-results.json` / `repro-after-results.json`. After the fix, all four focused calls match explicit expected bits and the original; zero float substitutions are emitted. |
| Nine-variant audit | `float-variant-audit.ll`, `.so`, command logs, and `float-variant-audit-results.json`. |
| Source/toolchain provenance | `revision.txt`, `initial-status.txt`, `initial-patch.txt`, `rust-version.txt`, saved changed-source snapshots under `fixed-source/`, and harness version/command records. |

An initial `baseline-smoke` invocation omitted the LLVM environment and failed
preflight before any fixture ran. The explicitly configured rerun above passed;
this was invocation setup, not a repository runtime failure. A first post-edit
format check found one import wrap; it was corrected before the passing checks.
That float-fix session did not run native macOS/Windows, Alive2, IDA, or Ghidra;
the later integer/IDA work is recorded below.

Two incorrect examples from the old roadmap were also evaluated and corrected:
`35*35 - 34*6*6 == 1` disproves its claimed always-true predicate, and
`0x1337 ^ 0xD3C9 == 0xC0FE`, not `0xC0DE`. These were documentation errors, not
newly reproduced runtime bugs.

## Integer MBA eligibility audit

Evidence root: `target/integer-audit.tdVaSI/`. The pre-audit revision and working
patch are retained in `revision.txt` and `initial-patch.txt`; final source
snapshots and hashes accompany the verification summary. These local artifacts
are ignored and must be preserved separately before cleaning `target/`.

### Reproduced failures and selected fix

The old pass duplicated operands that could be undef. In LLVM, each use of an
undef-derived value can observe different bits, invalidating some algebraic
identities. At seed 0 the actual Lyra path changed `or i8 undef, -1` into
`ret i8 undef`: original native result 255, transformed result 0.
`undef-regression-before.log` also records the permanent JIT regression failing
at `filled(0), seed 1` (0 instead of 255).

Freezing operands repaired the IR semantics but was **not shipped**. Alive2
proved the three functions in `frozen-seed3.ll`, including `filled = 255`;
LLVM 22.1.8 `llc -O1/-O2/-O3` nevertheless emitted incorrect machine code.
The retained direct-llc probes returned 252 instead of 255. JIT failures also
showed varying wrong values. `llc -O0` and clang's full `-O0/-O2` pipelines
returned the expected values. Lyra uses direct optimized llc lowering, so the
IR-only repair was insufficient. `after.ll` and `frozen-seed3.ll` are historical
rejected candidates, **not the final fix**.

The user chose conservative eligibility:

- No new freeze instructions, compiler patches, or replacement MBA recipes.
- Integer constants, `noundef` parameters, and loads carrying `!noundef` seed
  definedness. A worklist follows SSA users independently of textual block order.
- Definedness propagates only through supported scalar producers whose operands
  are all proven defined: plain arithmetic/bitwise operations, safe integer
  casts, comparisons, selects, and acyclic PHIs. Poison-producing flags
  (`nsw`, `nuw`, `disjoint`, `nneg`, `samesign`) prevent treating the producer's
  result as generally defined. Bounded shift handling is intentionally limited
  to unflagged left shift by one at widths greater than one.
- An existing freeze is trusted only if its input was already proven defined;
  freeze of an unknown value does not qualify for expansion.
- Unannotated loads, unqualified arguments, cyclic PHIs, unsupported producers,
  vectors, and floating-point values do not establish eligible operands.
  This analysis is deliberately incomplete: safe-but-unproven cases are skipped.
- Eligible source add/sub/or may have nowrap/disjoint flags, but these are not
  copied to intermediate operations. Correctness is **LLVM refinement**:
  defined source results must agree; source poison may become more defined.

The original 15 identities are modulo the operand width. This follows the
[LLVM undef/poison contract](https://llvm.org/docs/UndefinedBehavior.html);
it is not a claim of equivalence for arbitrary mathematical integers.

### Native verification

- `final-tests.log`: both permanent JIT tests passed. Literal, select-derived,
  and frozen undef retain required defined bits across 16 seeds. Modular tests
  execute 993,000 cases: all pairs at widths 1/2/3/4/8 and boundary pairs at
  widths 16/32/64/128, each opcode at seeds 0/1/42. Width 1 is left untouched.
  Initialized test loads express their actual contract using `!noundef`.
- Formatting, locked binary build, smoke **7/7**, and full **39/39** passed.
  Reports: `conservative-smoke/report.json`, `conservative-full/report.json`.
  The float, loader-time greeting, canary, and existing pass checks remain green.
- `conservative-unknown.ll` preserves all 101 unknown-input/undef corpus functions
  without substitution. Actual llc-lowered `and_undef`/`or_undef` return 0/255.
- `defined-original.ll` has 108 noundef/flag functions at widths 1 through 128.
  Seed 0 produced 188 substitutions in 78 functions in
  `conservative-defined.ll`; transformed IR passed `opt -passes=verify`.
- `conservative-branch-{0,1,42}.ll` combines block shuffle, indirect branch, and
  MBA. Each seed rewrote two unconditional and one conditional branch; MBA made
  9/6/6 substitutions. Each llc-lowered binary passed 4,196 runtime input pairs.
  Seed 42 explicitly expands the conditional dispatch index into MBA arithmetic,
  while indices depending on unannotated volatile key loads remain unchanged.

### Formal checks and limits

Local executable: `alive-audit/build/alive-tv` below the evidence root.
Pinned Alive2 commit: `da7b97228a7e3e4832e56c80f19d69ec9a53a9d5`
(`v21.0-57-gda7b9722-dirty`), built against the installed LLVM **22.1.8** with
Z3 **4.15.3.0**. The dirty suffix is documented build integration: Arch shared
libLLVM linkage and the Z3 wheel's missing version header; no Alive2 semantic
code was patched. Exact setup, patches, source hashes, and per-function
verdicts are in `alive-audit/audit-report.json` and its adjacent logs.

The old emitted corpus had 13 incorrect refinements. The rejected freeze-based
corpus had 90 proven / 11 timed out / 0 incorrect; all 11 remained timeouts at
30 seconds per SMT query. Supplemental frozen-recipe checks had 255 proven /
33 timed out; explicit undef/poison/select probes had 76 proven / 5 timed out.
All 24 OR-disjoint recipe cases passed. A separate fixed-value bit-vector check
proved 120 recipe/width combinations; it is **not** an LLVM undef/poison proof.

Final emitted-code checks use `--always-verify --smt-to=30000`, with undef and
poison checks enabled:

```sh
A=target/integer-audit.tdVaSI
"$A/alive-audit/build/alive-tv" --always-verify --smt-to=30000 \
  "$A/original.ll" "$A/conservative-unknown.ll"
"$A/alive-audit/build/alive-tv" --always-verify --smt-to=30000 \
  "$A/defined-original.ll" "$A/conservative-defined.ll"
```

Their outputs are `conservative-unknown-proof.log` and
`conservative-defined-proof.log`. Alive2 can exit zero with incorrect results or
timeouts: inspect the verdict summaries, not just the process status.

Final verdicts:

| Corpus | Proven comparisons | Incorrect | Timeouts | Tool errors |
|---|---:|---:|---:|---:|
| 108 defined-input/flag functions | 108 | 0 | 0 | 0 |
| 101 deliberately unchanged unknown-input functions | 94 | 0 | 7 | 0 |

The seven unproven unchanged cases are `add_16_plain`, `add_32_plain`,
`add_64_plain`, `sub_64_plain`, `add_128_plain`, `add_128_nsw`, and
`sub_128_plain`. `--always-verify` forces actual checks even when source and
target are identical; those timeouts are retained rather than counted as proofs.
Final proof/native/IDA results are aggregated in `verification-summary.json`.

No vector, arbitrary-width, memory-model, loop, indirectbr, interprocedural,
or machine-code formal proof is claimed. Timeouts are gaps, never successes.
Only Linux x86-64 was executed; native macOS and Windows checks remain required.

## Working local IDA exporter

`scripts/export_ida.py` was exercised against retained float and integer
fixtures using the supplied `/home/user/ida-pro-9.1` installation. Observed:
IDA **9.1**, idalib build **250226**, Hex-Rays **9.1.0.250226**,
IDAPython **9.1.0**, `idapro` package **0.0.2**, Python **3.11.16**.

```sh
IDA=/home/user/ida-pro-9.1
A=target/integer-audit.tdVaSI
# These setup commands have already succeeded locally; do not reinstall blindly.
uv venv --python python3.11 "$A/ida/venv"
uv pip install --python "$A/ida/venv/bin/python" "$IDA/idalib/python"
# Use a new output directory for each run.
"$A/ida/venv/bin/python" scripts/export_ida.py "$A/conservative-defined.so" \
  --ida-dir "$IDA" --output "$A/ida/next-inspection" \
  --function add_32_plain --function and_32_plain
```

This uses **direct idalib/IDAPython**, not MCP. The mounted `xd://ida` interface
could not import its required packages using its configured Python; no omp
settings were changed and no connected bridge is claimed. The exporter copies
the binary and privately inherits prior terms acceptance from an existing
`ida.reg`; it does not accept terms, change licenses, or modify the user's
databases/settings. That temporary profile is deleted on exit. Native console
metadata is not exported. Worker execution is bounded to 600 seconds, and
analysis errors/timeouts produce failed JSON/text reports and nonzero exit.

Evidence under `ida/`:

- `integer-conservative-defined/analysis.{json,txt}`: recovered assembly and
  pseudocode for `add_32_plain` and `and_32_plain`. The expressions remain
  expanded in this sample; IDA successfully decompiled both.
- `integer-conservative-unknown/analysis.{json,txt}`: `or_undef` now emits
  `mov al, 0FFh; ret` and decompiles to a byte return of -1 (bits 255).
- `integer-original/` and `integer-before/`: corresponding pre-fix inspection.
- `float-success/` and `float-success-repeat/`: byte-identical exports for
  floating add/mul using the same exporter version.
- `evidence.json` and `timeout-injected/`: missing-function and unaccepted-profile
  failures, plus a real worker killed/reaped with an injected 1 ms timeout.
  This was a timeout-path check, not a claim that a 600-second timeout elapsed.

These are integration and scoped inspection results, not a decompiler-resistance
benchmark. Ghidra was not exercised.

## Measured existing-pass baseline

Evidence root: `target/pass-baseline.eAR0cO/`. Main report:
`full/report.json` (`measurement.rows`), with command logs, `junit.xml`,
source snapshots, input copies, original/transformed IR, binaries and stripped
copies. `seed0-results.csv` is a derived compact view; `verification-summary.json`
records counts, source hashes, seed diversity and analysis caveats.
`measured-tools/` retains the exact three release-built Lyra binaries used for
timing; their hashes are in the report. Later CLI/comment qualifications did not
change pass algorithms. The final input-hash/outcome-reporting additions were
exercised in `reporting-smoke/`, separately from the full timing run.

### Protocol and verification

- Reused `scripts/ci_smoke.py` through `scripts/measure_passes.py`, not a separate
  test framework. `test_project/src/benchmark.rs` supplies integer/control-flow,
  public-text scanning, and repeated length-1024 FFT work. Independent Python
  checksums live in `scripts/benchmark_workloads.py`.
- Plain/no-pass/each individual pass/indirect-branch+MBA/all-pass profiles,
  seeds 0/1/42 for Lyra, both public executable and cdylib fixtures:
  **44 native profiles, 132 timed builds, 990 measured runtime invocations,
  220 warmups, and 36 separate coverage builds**. All native profiles passed.
- Three cold-target builds per profile, warm/offline downloads, LTO off and one
  codegen unit for every profile. “Plain” is controlled no-Lyra compilation, not
  the project's stock fat-LTO build. Profile order was shuffled with seed 0.
- Runtime: two warmups and nine samples per workload/profile. Native work time
  is separate from process startup/exit wall time. CPU 0 was used for runtime;
  build affinity was restored. Host: Ryzen 9 5900X, governor `powersave`;
  Rust 1.98.1, LLVM 22.1.8, Linux x86-64. No governor or system cache changes.
- Timed builds had coverage disabled. Each instrumented build used the same
  project, flags and seed, and its transformed IR matched the timed build.
  Ten additional edge/zero-work comparisons also matched pre-instrumentation,
  metrics-off, and metrics-on IR byte-for-byte. Accounting checks included
  struct-field encryption, excluded functions, i1, vectors and unknown operands.
- Both JIT regressions passed; existing native smoke **7/7** and full **39/39**
  passed. Default fixture output and the strict float oracle are unchanged.
  Benchmark u64-wraparound inputs and invalid CLI requests were also exercised.
- All three repeated artifacts matched within each of the 44 profile/seed cases.
  This is observed repeatability for pinned inputs, not a universal guarantee.
  The no-pass artifacts did not vary with seeds. DLL shuffle/indirect-branch
  profiles were also identical to no-pass because they had no eligible work.

Recorded full-run invocation (choose a new output directory when rerunning):

```sh
export RUSTUP_TOOLCHAIN=1.98.1 LLVM_SYS_221_PREFIX=/usr LYRA_LLVM_BIN=/usr/bin
cargo build --locked --release --bins
python3 scripts/measure_passes.py \
  --output "$PWD/target/pass-baseline-rerun" \
  --ida-python target/integer-audit.tdVaSI/ida/venv/bin/python \
  --ida-dir /home/user/ida-pro-9.1
```

The first pilot exposed a launcher bug: resolving the venv Python symlink
selected its base interpreter, which lacked idapro. The driver now preserves
the venv executable path. The repaired pilot, CFG export, and final reporting
smoke passed. This was infrastructure failure, not protection against IDA.
The changed captured-command timeout path was exercised with a real child:
it retained partial stdout and killed/reaped the timed-out process.

### Observed costs

Executable, seed 0 (plain is unseeded); ratios are per-profile medians relative
to controlled plain compilation. Build uses three samples; runtime uses nine.
Plain medians: build **0.1599 s**, integer **6.789 ms**, text **1.503 ms**,
FFT **1.699 ms**. Workload timing includes input preparation, checksum consumption
and black-box barriers, not just an isolated inner arithmetic instruction.

| Profile | Build × | Startup wall ms | Integer work × | Text work × | FFT work × | Stripped bytes |
|---|---:|---:|---:|---:|---:|---:|
| plain | 1.00 | 1.852 | 1.00 | 1.00 | 1.00 | 390,072 |
| no-passes | 2.29 | 1.776 | 0.98 | 1.02 | 0.96 | 389,840 |
| string-enc | 2.26 | 1.813 | 1.01 | 1.03 | 1.00 | 392,288 |
| shuffle-blocks | 2.28 | 1.804 | 0.99 | 1.00 | 1.00 | 389,840 |
| indirect-branch | 2.37 | 1.847 | 1.37 | 3.45 | 1.95 | 397,488 |
| mba | 2.31 | 1.762 | 1.05 | 1.27 | 0.98 | 390,048 |
| branch-mba | 2.37 | 1.883 | 1.38 | 3.47 | 2.15 | 398,016 |
| all-passes | 2.38 | 1.890 | 1.59 | 4.07 | 2.22 | 401,176 |

Across seeds, all-pass native-work ratios were integer **1.49–1.59×**, text
**4.07–4.49×**, FFT **2.21–2.24×**. Indirect branch alone cost text
**2.90–3.71×**, FFT **1.92–1.98×**. The no-pass build cost shows that pipeline
overhead is a substantial part of this small crate's build time.

Raw samples, min/max and sample standard deviation are retained. For example,
seed-0 all-pass startup coefficient of variation was about 7.1%, so small startup
differences are not evidence of a speedup. These are warm-cache microbenchmarks
on one host, not statistical performance guarantees or an application portfolio.
Whole-file sizes include untransformed Rust runtime code.

The DLL's seed-0 stripped size was 318,368 bytes plain and 318,784 bytes all-pass.
Its roughly 36–37 ms ABI-probe wall time includes Python startup and many typed
checks; do not interpret it as isolated dlopen or per-call latency.

### Coverage findings

`LYRA_COVERAGE=1` emits one JSON record per enabled pass/module. Counts use the
units described in [README](../README.md#measuring-existing-passes); they are not
interchangeable protection percentages. String counts precede ctor construction;
successful builds and runtime checks separately validate that lifecycle.

- Executable MBA-only, seed 0, round zero: **71 candidate instructions,
  15 eligible, 7 transformed**; 56 skipped for unproven operands, 8 by the random
  gate. Round one added 20 substitutions; 27 total visits transformed does not
  mean 27 unique original instructions.
- Executable all-pass MBA entry: **220 candidates, 40 eligible, 23 transformed**
  in round zero; 180 unproven, 17 random nonselections. Earlier passes introduced
  arithmetic, so this denominator is not the original Rust module alone.
- Executable indirect branch alone rewrote all **144 direct `br`** terminators;
  all-pass rewrote 147 including generated startup code. This says nothing about
  `switch`, which is outside that pass's denominator.
- Executable string encryption transformed **35 initial globals**. Shuffle
  moved blocks in 10 of 18 function bodies at seed 0; 7 were too small and one
  eligible permutation was identical.
- The original DLL had no direct `br` terminators and no shuffle-eligible
  functions. Their passing runs are **no-op compatibility checks**, not evidence
  of control-flow protection. All-pass created three branches in startup code.
- DLL MBA seed 0 had zero round-zero substitutions but two in round one. A
  round-zero zero must not be confused with a whole-pass no-op.

### IDA observations

Ten fresh stripped-artifact inspections selected 25 known entry addresses using
the external symbol correspondence files. No source names/types or manual
function repairs were supplied to IDA. Exported DLL ABI names remain legitimate
binary information. Versions: IDA 9.1, Hex-Rays 9.1.0.250226; exact analysis flags,
configuration hashes, CFG blocks/edges and pseudocode are in each report.

- Nine exports returned status `ok`; one returned `failed` with partial results.
  **24/25 selected entries were located and produced pseudocode.** The all-pass
  executable's `bench_text` at `0x1a5d0` was not recognized as a function entry.
  The retained symbol table gives size `0x223`, and read-only llvm-objdump
  inspection confirmed real code beginning there.
- API success is not complete recovery: branch+MBA `bench_fft` had a one-block
  recovered CFG and pseudocode ending in an explicit indirect assembly jump
  after initialization. The actual workload still completed correctly.
- Some arithmetic entry pseudocode was rendered as an indirect function-pointer
  call rather than the source computation. These are scoped automatic-recovery
  limitations, not a claim of universal decompiler defeat or costly manual repair.
- Measurement `PASSED` means the experiment was collected and its runtime checks
  passed, not that every decompilation succeeded. Final console output and the
  nested `ida.status` explicitly expose partial/failed analysis. No IDA timeout
  occurred in this matrix; the configured worker limit remains 600 seconds.

### Next bounded work

The clearest cost target is hot-path indirect branching, especially text/loop
work. Before changing it, choose a workload-specific overhead/coverage budget
and preserve the native/IR correctness gates. Measure whether reducing its cost
also removes the observed automatic-recovery impact; do not optimize only for
larger pseudo-C. Keep proven-defined MBA eligibility intact. Broader applications,
manual-analysis effort, another analysis engine, and macOS/Windows remain gaps.

## Rejected entry-cache experiments

Evidence root: `target/ib-entry-predicate.Dfew4G/`.
**Decision: reject both variants and retain the original lowering.**
The only permanent runtime-test addition is
`indirect_branches_preserve_volatile_loops_recursion_and_tail_calls`; the pass
algorithm, coverage, RNG choices and MBA policy are unchanged.

### Candidates and gates

The proposed optimization reused a generated private-key predicate once per
function invocation rather than loading/comparing the key at every unconditional
branch. The second variant additionally computed each unconditional target
pointer as entry SSA. Neither variant moved source volatile operations or added
a heap/stack/shared-state cache. Entry insertion avoided PHIs and did not split
`musttail` calls from their returns.

`acceptance.json` was recorded before editing; `target-cache-acceptance.json`
kept the same gates for the second variant:

- All native/LLVM correctness checks must pass; direct-branch coverage must match.
- Each of indirect-branch, branch+MBA and all-pass must reduce text-work time
  by at least 15%, using the geometric mean across seeds of paired median ratios.
- No matched integer/text/FFT profile/seed may regress more than 5%.
- Stripped bytes may grow at most 1%; linked `.text` at most 2%.
- Fresh, same-settings IDA inspection must retain at least one observed automatic
  control-flow recovery gap in each selected combined-profile sample group.
  Pseudocode size and API success are not protection/recovery scores.

The existing measurement runner built and checked the candidates. The retained
`compare.py` experiment reused that runner to execute old and candidate artifacts
in balanced, deterministically randomized adjacent pairs on CPU 0. Every run
checked the independent checksum. Each variant had 36 comparison cases,
21 measured pairs per case and three warmups per side: **756 pairs and 1,728
checksum-checked invocations**. Fixture input hashes matched. This fresh paired
comparison, not just the older absolute timings, decided the runtime gates.

### Results and rollback

Values below are candidate/baseline text-work ratios, geometrically averaged
across per-seed paired medians. The required value was **at most 0.85**.

| Variant | Indirect branch | Branch + MBA | All passes | Workload/seed regressions above 5% |
|---|---:|---:|---:|---:|
| Entry predicate only | 1.075 | 1.154 | 1.004 | 10 |
| Entry predicate + target SSA | 1.158 | 1.050 | 0.985 | 7 |

The worst observed paired median regression was **38.4%** for predicate-only
branch+MBA text at seed 0, and **54.7%** for target reuse with indirect-branch text
at seed 42. Neither candidate met the text-improvement gate.

Correctness, coverage and size gates passed. Maximum stripped-size ratios were
1.00024 and 1.00109; maximum `.text` ratios were 1.00017 and 1.00119. The target
variant preserved all 18 matched branch-coverage cases, seeded key values and
biased selector choices. On the executable at seed 0, static generated key-load
counts fell from 48 to 10 (branch-only/branch+MBA), and 50 to 11 (all-pass).
Those IR reductions did **not** imply faster native execution. The exact
microarchitectural cause was not isolated; do not claim a register-pressure or
branch-prediction cause from these timings alone.

Both candidates retained incomplete automatic IDA recovery in the selected
combined-profile samples. Branch+MBA FFT still ended in an unresolved indirect
assembly jump. All-pass text entry discovery improved compared with the baseline,
but its pseudocode still contained unresolved transfers. Target reuse also exposed
more all-pass FFT paths (8 blocks / 14 edges), without complete recovery. These
tradeoffs were recorded, not hidden to favor an optimization.

Because performance failed, all production entry-cache code was removed. The
restored pass produced **byte-identical emitted IR in 18 matched
fixture/profile/seed checks**, all accepted by LLVM verification.
The rebuilt main tool's binary hash differed, so the rollback evidence is
emitted behavior rather than a claim of byte-identical tool binaries.

Retained regression: 309 JIT calls over seeds 0/1/42 validate caller-owned
volatile state, PHI loops, repeated calls, recursion, direct entry branches and
tail-call returns. After rollback: **3 Cargo tests passed**, release build and
formatting passed, native smoke **7/7**, full **39/39**. No macOS/Windows or
machine-code equivalence proof is claimed.

Evidence map:

- `candidate/`, `paired/`: predicate-only builds, native runs, IDA output and pairs.
- `candidate-targets/`, `paired-targets/`: target-SSA variant and paired results.
- `baseline-tools/`, `predicate-only-tools/`, `target-cache-tools/`: exact binaries;
  adjacent `indirect_branch-*.rs` snapshots retain each implementation.
- `decision.json`, `ida-comparison.json`, `target-cache-ir-invariants.json`:
  verdicts and scoped analysis/invariant observations.
- `restored-tests.log`, `restored-build.log`, `restored-smoke/`,
  `restored-full/`, `restoration-ir-checks.json`: final repository verification.

Do not silently relax the gates or reintroduce either variant. The next useful
step is targeted final-code/control-transfer profiling, then a separately
declared design and cost/coverage tradeoff. Conservative MBA safety stays fixed.

## Machine-level profiling follow-up

Evidence root: `target/ib-machine-profile.dtTuoJ/`.
No production pass changed. Perf 7.2.7 and libpfm 4.14.1 were verified against
trusted package hashes/signatures and extracted into `perf-root/`; no system
package installation or kernel permission change was required. A real
`perf_event_open` probe confirmed user-space counter access.

`profile.py` reused the existing runner and independent checksum oracle on ten
retained artifacts: controlled plain plus shipped/predicate-only/target-cache
variants for indirect-branch seed 42 and combined profiles at seed 0.
Text input was scaled to 262,144 scans, or 110,624,768 bytes per invocation.
All **150 PMU runs** passed checksum checks; every four-event group was scheduled
100% of the time. Events count the entire child userspace, not only the workload
function; kernel execution is excluded.
Cycle sampling attributed over 99% to `bench_text` in the inspected shipped and
target-cache cases, with zero lost samples. Sampling is non-precise: PC percentages
can skid and are not exact per-instruction latencies.

Representative whole-process counter medians, indirect-branch seed 42:

| Variant | Instructions/byte | Cycles/byte | IPC | Indirect miss rate |
|---|---:|---:|---:|---:|
| Controlled plain | 15.77 | 4.04 | 3.91 | Not comparable: very few startup indirect branches |
| Shipped | 27.92 | 12.04 | 2.32 | 0.617% |
| Predicate only | 27.05 | 14.36 | 1.88 | 0.757% |
| Target cache | 22.71 | 18.42 | 1.23 | 1.474% |

The worst target-cache case retired about **18.7% fewer instructions** but used
about **52.9% more cycles**; its retired indirect mispredictions were **2.39×**
the shipped case. This does not mean every indirect branch misses: more than
98% were predicted correctly in these long runs. Frontend-stall counters changed
as well, but these measurements do not uniquely apportion every extra cycle.

Actual disassembly—not IR estimates—shows that predicate reuse removes the key
load while comparisons are rematerialized at hot edges. The measured instruction
reduction (96,730,838) agrees with one removed load for each of the 369
non-whitespace bytes per scan (96,731,136), apart from process-startup noise.
The target variant saves five callee-saved registers versus one in the shipped
function, but the inspected inner character loop has no additional stack-memory
operands. Do not attribute the slowdown to hot-loop spilling on this evidence.

A second controlled experiment ran **30 checksum-checked PMU cases** with
per-child ASLR enabled/disabled; mmap records confirmed varying versus fixed image
bases. With ASLR disabled, shipped/target median cycles remained approximately
1.318/2.042 billion and indirect-miss rates 0.631%/1.559%. Whole-image ASLR does
not explain away the regression. Local code layout, dispatch dependencies,
predictor interactions and frontend throughput are not yet individually isolated.
Hardware branch-stack sampling was unavailable for the probed PMU/event; this is
a profiling limitation, not a protection result.

Artifacts: `pmu/report.json` and assembly logs, `samples/` perf recordings/reports/
annotations, `static-findings.json`, `layout-control/report.json` and mmap evidence,
`decision.json`, and signed tool provenance under `perf-root/`.
The unsupported IR-derived static estimates returned by an exploratory helper
were discarded; only actual disassembly and measured counters support this record.

Decision: preserve the shipped lowering and prior safety gates. Further dispatch
changes need controlled final-code evidence plus the expanded performance, real
workspace scale and adversarial-analysis baselines; do not substitute a smaller
IR expression for measured improvement.

## Linux workstation preparation

Prefer native Linux x86-64 to match the configured Linux CI job. The CI matrix
also includes macOS ARM64 and Windows x86-64 MSVC; a Linux run does not verify
those platforms. Retain access to native runners for cross-platform confirmation.

### Required for arithmetic work

- Rust toolchain **1.98.1**, the current CI pin, with rustfmt.
- LLVM **22**, including development headers/libraries and `llvm-config`,
  `clang`, `llc`, `opt`, `llvm-ar`, `llvm-objdump`, `llvm-readobj`, and
  `llvm-reduce`. `Cargo.toml` uses `llvm-sys = "221"` and Inkwell's `llvm22-1`.
- Python 3, a native C/C++ build toolchain, and the LLVM link dependencies.
- A debugger such as LLDB or GDB for native failures.

For Ubuntu, `scripts/setup_ci.py` records the CI package list and repository key
validation. Its Linux provisioning is specifically for Ubuntu Noble/amd64;
do not run it blindly as a generic Linux installer. On another distribution,
install equivalent LLVM 22 development packages rather than unpinned latest LLVM.

The current Ubuntu CI list includes `build-essential`, `pkg-config`, `clang-22`,
`lld-22`, `llvm-22`, `llvm-22-dev`, `llvm-22-tools`, `libpolly-22-dev`,
`libclang-rt-22-dev`, `libffi-dev`, `libzstd-dev`, `zlib1g-dev`, `libxml2-dev`,
`libedit-dev`, and `libncurses-dev`.

### Baseline commands

Run from the repository root after installing the prerequisites. The prefix below
is the Debian/Ubuntu layout; adjust it to the actual LLVM 22 installation.
Do not substitute a newer Rust/LLVM pair silently if the pinned setup fails.

```bash
set -euo pipefail
rustup toolchain install 1.98.1 --profile minimal --component rustfmt
export RUSTUP_TOOLCHAIN=1.98.1
export LLVM_SYS_221_PREFIX=/usr/lib/llvm-22
export LYRA_LLVM_BIN="$LLVM_SYS_221_PREFIX/bin"
export PATH="$LYRA_LLVM_BIN:$PATH"

rustc -vV
llvm-config --version
clang --version
llc --version
opt --version
llvm-ar --version
llvm-reduce --version
python3 --version

cargo fmt --all -- --check
cargo build --locked --bins
cargo test --locked --bins

mkdir -p target
RUN_ROOT="$(mktemp -d "$PWD/target/linux-handoff.XXXXXX")"
printf 'Evidence directory: %s\n' "$RUN_ROOT"
python3 scripts/ci_smoke.py --suite smoke --output "$RUN_ROOT/smoke"
python3 scripts/ci_smoke.py --suite full --output "$RUN_ROOT/full"
```

Keep `lyra`, `lyra_wrapper`, and `lyra_linker` together; `--bins` builds all three.
The smoke runner requires an absolute, previously nonexistent output directory;
using separate children of a fresh `RUN_ROOT` satisfies that requirement. If a
command fails, retain its output and diagnose it before continuing. Report a
pre-existing failure separately from a regression introduced by a later edit.

The harness builds disposable copies of `test_project` and `test_dll_project`,
checks explicit outputs and baseline agreement, validates transformed IR with
`opt`, and checks a full plaintext canary's absence for string encryption. Smoke
uses no-pass/all-pass profiles at seed 0; full adds individual passes and seeds
0, 1, and 42. A green run does not cover arbitrary float inputs or every pass
combination. Reports include `report.json`, `junit.xml`, and command logs.
Successful transformed artifacts/intermediates are removed by the current runner;
failed cases retain evidence. Use a separate retained experiment for decompiler
comparison rather than assuming successful smoke binaries are still present.

## Analysis and measurement tools

Direct IDA export and the pinned local Alive2 build above are working. Other
integrations still need verification. These tools complement the runtime
harness; none makes structural validity or decompiler output a semantic proof.

| Tool | Purpose | Setup / limits |
|---|---|---|
| [Alive2 / alive-tv](https://github.com/AliveToolkit/alive2) | Check small before/after LLVM transformations under LLVM semantics. First target: arithmetic rewrites. | Pin a compatible LLVM/Alive2 pair. Upstream targets LLVM main; do not assume compatibility with Lyra's LLVM 22. Keep any separate analysis toolchain isolated from Lyra's PATH/prefix. Unsupported constructs, timeouts, and bounds are not a successful proof. Interprocedural transformations are explicitly unsupported. |
| [IDA Pro MCP](https://github.com/mrexodia/ida-pro-mcp) | Interactive inspection of instructions, decompilation, and references. | Community integration. Its README lists IDA Pro 8.3+, recommends 9, and excludes IDA Free. Have the appropriate licensed Hex-Rays decompiler. Current guidance prefers `idalib-mcp` over the older GUI plugin. Pin a reviewed version. |
| [Hex-Rays idalib](https://docs.hex-rays.com/core/idalib/getting-started) | Repeatable headless analysis and scripted exports. | `scripts/export_ida.py` is exercised with the installed Linux IDA 9.1 / Python 3.11 setup. It records versions, function lists, assembly, pseudocode, and failures using disposable input copies. Requires a valid license and already accepted terms; it is not an MCP bridge. |
| [GhidraMCP](https://github.com/LaurieWired/GhidraMCP) | Independent decompiler view; free alternative to starting with IDA. | Ghidra plugin plus Python MCP bridge. Verify the selected plugin/Ghidra versions work together. A second engine helps distinguish tool-specific limitations but does not establish correctness by consensus. |
| [llvm-reduce](https://llvm.org/docs/CommandGuide/llvm-reduce.html) | Minimise failing IR. | Its interestingness check must preserve the same semantic mismatch or crash, not any unrelated nonzero exit. |
| [Hyperfine](https://github.com/sharkdp/hyperfine) | Repeated runtime/build measurements with JSON exports. | Control inputs, warmups, cache policy, and environment; report variability. Pair with file/section sizes, not just wall-clock time. |

MCP is for investigation; fixed scripts are for reproducible measurements. Start
with one connected decompiler, not multiple mandatory integrations. Keep bridges
local or behind a private authenticated connection, use read-only capabilities
where available, and work on disposable analysis databases. Do not commit license
keys or client credentials. Installing a server is not enough: register it with
the coding client and confirm its tools are actually exposed to the next session.

Confirm an integration by opening a locally built fixture, listing its functions,
and retrieving disassembly/decompilation for a known function. Record server,
plugin, analysis-engine, and decompiler versions. Never treat a disconnected MCP
server as evidence that a binary defeated analysis.

## Real-workspace scale baseline

Evidence root: `target/acceptance-baselines.ZkuFQz/`; consolidated report
`verification-summary.json`, reproduced by `summarize_acceptance.py`.
The completed run is an **initial Linux feasibility/behavior baseline**, not
product acceptance or comprehensive correctness for arbitrary applications.

### Corpus and selection

- Public [rust-analyzer](https://github.com/rust-lang/rust-analyzer) revision
  `3332931d313fad08166571c05d7237d3a4d77ac2`, MIT/Apache-2.0, 44 workspace
  members and 1,485 tracked Rust files. Rust 1.98.1 / LLVM 22.1.8, native x86-64,
  release, LTO off, one codegen unit and one Cargo job.
- Isolated, signature/hash-verified **tokei 15.0.0** counted **470,852
  first-party, non-generated Rust code lines**: 384,289 other-source lines
  including inline tests, plus 86,563 test/fixture/bench/example lines.
  Excluded: 43,966 generated/mixed-generated and 270 vendored tracked lines.
  Fetched dependencies and build-generated Rust were counted separately, not
  used to qualify the workspace. This is not a production-only LoC count.
- Selected ten libraries: `hir_ty`, `hir_def`, `hir`, `ide`, `ide_assists`,
  `ide_completion`, `ide_diagnostics`, `ide_db`, `parser`, and `syntax`.
  Their first-party non-generated source total is **391,276 lines**, with the
  same test/fixture caveat. `rust_analyzer` itself and other crates are
  intentionally unselected; this is not whole-workspace transformation coverage.
- Both Lyra profiles retain and verify all ten input/output modules. Each
  selected rlib's code-member SHA256 exactly matches its transformed object.
  No selected object silently fell back to its original. `Cargo.lock` remained
  unchanged.

### Build and resource observations

| Profile | Build wall | Sampled peak aggregate RSS | Final executable bytes |
|---|---:|---:|---:|
| Plain | 466.06 s | 2.34 GiB | 43,461,152 |
| Lyra, no passes | 596.12 s | 2.54 GiB | 44,014,704 |
| Lyra, all passes, seed 0 | 611.03 s | 2.54 GiB | 52,550,056 |

All builds completed within predeclared **2,400-second wall, 12-GiB sampled RSS,
and 40-GiB evidence-disk ceilings**. Cumulative evidence disk at each completion
was 1.08 / 4.46 / 6.50 GiB; those are not isolated per-build temporary peaks.
Failed-before artifacts are included in later cumulative totals.

These are single-run feasibility observations, not controlled build-overhead
ratios. The plain host build overlapped earlier Lyra verification and lacked
Lyra's explicit target argument. RSS samples sum owned process-group resident
pages, double-count shared pages, and may miss short peaks/escaping descendants.
Process first/last observations retain sampled stage residence: for example,
all-pass `lyra_ir` and `llc` summed observed intervals were 12.43 s and 100.01 s.
They are lower-bound observations, not exact stage timing or additive CPU time.

### Actual IR and pass coverage

The ten input modules contain **23,857 function definitions, 717,392 basic
blocks, 38,757 global variables**, and 1,447 aliases, in **634,256,843 IR bytes**.
No-pass output preserves those structural counts. All-pass output contains
23,877 definitions, 717,442 blocks and 107,495 globals in 977,503,262 bytes.
These are LLVM-module counts, including monomorphized/inlined code, not source
LoC or a claim that every instruction was transformed.

All **40 pass/module coverage records** passed the existing accounting checks:

| Pass / opportunity unit | Considered | Eligible | Transformed |
|---|---:|---:|---:|
| String encryption / globals | 38,757 | 35,973 | 35,973 |
| Shuffle / functions with bodies | 23,877 | 7,952 | 7,147 |
| Indirect branch / direct `br` terminators | 466,868 | 84,635 | 84,635 |
| MBA round 0 / candidate visits at MBA entry | 267,954 | 23,850 | 14,260 |

Function exclusions account for 382,233 indirect-branch skips. MBA round 0 skips
include 140,410 excluded-function candidates and 101,619 unproven operands;
the conservative policy was not widened. MBA's two-round total is 45,748
substitutions, which includes revisits/generated opportunities and is not
45,748 distinct original source operations.

### Linked behavior and real-source runtime

Plain, no-pass and all-pass binaries all:

- Match a pinned upstream precedence AST golden, reconstruct its input tokens
  losslessly, preserve byte spans, and extract the expected `foo` function symbol.
- Run real HIR/type-inference/MIR/data-layout and IDE analysis on a small
  standalone no-sysroot subject. Exactly the intentional `u32`/`bool` mismatch
  appears at the expected span; generic-call/field/arithmetic inference adds no
  other mismatches and reports no inference panic. This is not a full IDE,
  build-script, proc-macro or sysroot workload.

An additional protocol used three real first-party Rust source files, CPU-pinned
seeded interleaving, two warmups and nine measured runs per input/profile:
**81 measured invocations plus 18 warmups**. Every JSON AST matched the plain
reference; references independently reconstructed source bytes and spans.

| Input | Plain median | No-pass median | All-pass median | All-pass / plain |
|---|---:|---:|---:|---:|
| `crates/hir/src/lib.rs` | 96.18 ms | 94.62 ms | 102.24 ms | 1.063× |
| `crates/hir-def/src/expr_store/lower.rs` | 55.42 ms | 55.00 ms | 62.56 ms | 1.129× |
| `crates/hir-ty/src/display.rs` | 42.87 ms | 41.87 ms | 49.07 ms | 1.145× |

Use **`scale/runtime-corpus-pidfd/report.json`**: Linux pidfd readiness avoids
timed-wait polling. Wall time includes startup, input reading, parsing, JSON
serialization and output spooling, not just parser work. The earlier
`runtime-corpus/` batch showed coarse timed-wait intervals and is explicitly
superseded for performance interpretation; its semantic checks remain valid.
Three parsing inputs and one seed do not establish “good performance,” and do
not erase the earlier 4.07–4.49× public-text workload cost.

Key provenance: `scale/baseline.json`, `qualification.json`,
`rust-file-counts.json`, `selection.json`, `lyra-*.result.json`, each successful
profile's `invocation.json`, `harvest.json`, `runtime/runtime-result.json`,
retained original/transformed IR and rlibs, and `lyra-tools-fixed.json`.
The source snapshot, failed-before evidence and successful verification logs
remain under the common evidence root.

## Scale-preparation correctness fixes

Evidence root: `target/acceptance-baselines.ZkuFQz/`.

- Cargo target names with hyphens previously bypassed interception because
  rustc uses underscore-normalized crate names. `src/main.rs` now normalizes
  crate selection once, while preserving Cargo target names and artifact paths.
  Explicit selections replace the default target selection; library-only
  selection remains intentional and tested.
- `test_workspace_project` and the existing native runner verify actual output
  and plaintext scope for default-bin, explicit hyphen/underscore multi-crate,
  and library-only selection. The retained old tools fail three new cases;
  the normalized explicit-name control already passed before the fix.
- The first real-workspace no-pass build exposed a separate LLVM text-IR issue:
  `ide_assists` contained a 1,026-byte cloned block name. LLVM 22's default
  1,024-byte non-global name limit merged it with its prefix during parsing.
  The input passes LLVM verification with
  `--non-global-value-max-name-size=-1`; default parsing produces a block with
  a terminator in its middle. This was not an obfuscation-pass failure.
- Lyra's single-use internal CLI entry point configures that LLVM option before
  module creation. The backend `llc` call and verification tools use the same
  setting. LLVM's [symbol-table implementation](https://github.com/llvm/llvm-project/blob/llvmorg-22.1.8/llvm/lib/IR/ValueSymbolTable.cpp)
  documents the truncation and negative-limit behavior. No input text rewriting,
  pass relaxation, or selected-crate exclusion was used.
- `tests/long_names.ir.in` is a shared reduced CFG fixture. The backend unit
  test and CLI no-pass/all-pass native cases require `choose(1) == 12` and
  `choose(0) == 22` despite the labels' shared 1,024-byte prefix. Both CLI cases
  fail with retained pre-fix tools and pass after the fix.
- After both fixes: Rust formatting, locked release binary build, **four Cargo
  tests**, and **46 full native cases** passed on Linux. The existing three JIT
  tests remain intact. No production pass, eligibility rule, or pass order changed.

Key records: `hyphen-regression-before/report.json`,
`long-name-regression-before/report.json`, `long-label-repro/`,
`verified-full-name-fix/report.json`, `lyra-source-snapshot/`, and the retained
before/fixed tool binaries under `scale/`. The initial failed scale run retains
its nine input modules under `scale/no-passes/original-ir/`; its failure was
observed before final linking, not counted as scale success.

## Blinded semantic-recovery pilot

Evidence: `target/acceptance-baselines.ZkuFQz/adversarial/`, especially
`protocol.json`, `readiness.json`, `trials.json`, `summary.json`, both
`case-*-exchanges.json` files, and `parent-replay.json`.

The controller tested anonymous stripped plain and retained all-pass seed-0
versions of the public `bench_integer_step` leaf. Each stateless model call
received only the ABI/entry/mapped ranges, generic action grammar, and that
condition's requested tool outputs. Source, profile labels, correspondence,
hidden inputs, oracle, host tools, and other-condition history were withheld.

Available tools were real Capstone disassembly, mapped-memory reads, Unicorn
execution/traces, and numeric-selector IDA/Hex-Rays decompilation. The isolated
environment pinned Unicorn 2.1.4, pyelftools 0.32, and Capstone 5.0.6. Before
model trials, each condition matched an independent oracle on **209 scalar
inputs** and **16 native integer-workload compositions**. Tool-boundary and
restricted-AST checks were exercised; model submissions never ran as host code.

| Anonymous condition | Controller-only mapping | Decisions / limit | Successful tool requests | Held-out agreement |
|---|---|---:|---|---:|
| A | All passes, seed 0 | 6 / 8 | Disassembly, two memory reads | 337 / 337 |
| B | Plain | 5 / 8 | Disassembly | 337 / 337 |

The model needed neither the available emulator nor the decompiler. Five invalid
duplicate-JSON responses counted against the decision budgets; they were not
repaired or replaced. Trial wall times were 32.35 s and 34.30 s respectively,
not evidence of a meaningful effort difference. The parent replayed scoring
and actual CPU emulation successfully.

**Interpretation:** this is a working evaluation method and a recovered small
obfuscated example, not evidence for “very difficult.” Finite held-out agreement
is not formal equivalence. Only one public leaf and one obfuscation seed were
tested; pretraining contamination was not measured. The `default` completion
route did not expose exact backend identity, sampling parameters, token usage,
or cost. A pinned-model, broader-workload evaluation remains necessary.

## Four-family evaluation and rejected volatile-target dispatch

Evidence root: `target/ib-efficiency.M1OLCW/`. The experiment contract,
candidate design and performance protocol were recorded before measurement in
`contract.json`, `candidate.json`, and `performance-protocol.json`.
The runtime decision is **reject**; `decision.json` is derived from the retained
reports by `derive_decision.py`. No candidate pass change remains in production.

### Corpus and correctness controls

The isolated no-dependency Rust cdylib exports anonymous `f0`–`f3` functions
covering scalar branching, a bounded data-dependent loop, mutable two-word
state transitions, and real byte-buffer normalization/pattern processing.
Outputs include both the return value and the full mutated buffer. Independent
Python semantics and disjoint verification/held-out sets include repeated state
sequences; the string case includes genuine positive static-pattern matches.

`corpus/manifest.json` pins eight shipped seed-0 profiles: plain, no-pass, each
individual pass, branch+MBA and all passes. `corpus/candidate/manifest.json`
adds the three IB-bearing candidate profiles at seeds 0/1/42. Every artifact
passed 640 cases per export and 64 state sequences / 1,536 transitions, for both
original and stripped copies. This is scoped semantic evidence, not equivalence.
The reusable build/oracle tooling and actual constructor/relocation requirements
are recorded in `corpus/delivery.json`.

Actual effects matter: shuffle alone produced identical exported-function
machine bytes to no-pass for all four cases. String encryption affected only
`f3`'s static comparison pattern. Pre-existing switch jumps are separated from
pass-generated branches; native indirect-jump totals alone are not credited as
protection.

### Candidate and predeclared gates

The candidate replaced each generated unconditional key-load/comparison/
biased-select/add sequence with a volatile target-slot load and `indirectbr`.
It shuffled the target table once per function to retain seeded layout.
Conditional lowering, exclusions, branch destination sets and source volatile
behavior were unchanged. This was not an entry predicate or target cache.

Gates required unchanged intended branch coverage and correctness, at least
15% hot-text improvement in each IB-bearing profile across seeds, no measured
workload/startup regression above 5%, and the previous stripped/text size limits.
Recovery evidence was required in addition to runtime; preserved indirect
instructions or shorter pseudocode were not accepted as goal effectiveness.

### Measured rejection

`paired/report.json` records **36 cases × 21 adjacent pairs = 756 measured
pairs**, plus warmups, pinned to one CPU with deterministic balanced order and
independent checksums. Eighteen executable/DLL profile/seed coverage pairs
matched exactly, and size/correctness gates passed.

| Profile | Candidate / shipped text-work ratio | Required |
|---|---:|---:|
| Indirect branch | 1.204× | ≤0.85× |
| Branch + MBA | 1.091× | ≤0.85× |
| All passes | 0.919× | ≤0.85× |

All three profile gates failed. **Nine individual cases regressed above 5%**;
the worst was approximately 30%. The all-pass aggregate improvement did not
meet its gate and did not excuse seed-specific regressions.

The scoped PMU follow-up (`pmu-postmortem/report.json`) ran 20 fully scheduled,
checksum-verified user-space cases on the long text workload. For IB seed 42,
the candidate retired **5.15% fewer instructions**, yet the generic group
measured **44.2% more cycles**. The indirect group measured the same retired
indirect-branch count but **23.1% more indirect mispredictions**. Event groups
were sampled separately; this does not isolate layout, dependency or predictor
causality, and is not a whole-pass hardware conclusion.

There was also a concrete static-recovery regression:
`recovery/static-comparison.json` shows IDA recovering both arithmetic branches
and the complete final return of candidate IB `f0`, matching the plain formula.
The shipped counterpart retains bogus indirect-call dispatch. The candidate
still contains three native indirect jumps, demonstrating why an instruction
count is not a resistance metric.

The candidate rust-analyzer run was not performed: mandatory runtime gates
already failed, so the candidate could not be retained. The previous shipped
large-workspace evidence remains scoped as documented above.

### Broadened blinded baseline

`recovery/baseline-clean-v1/trials.json` records **32 completed submissions**
across the eight shipped profiles and four families, using 207 actual model
decisions with zero model-service errors. Each condition had the same twelve
decision / 64 chosen-input budget. Source, family/profile labels, hidden inputs,
oracle and cross-condition history remained outside the stateless analyst.

Before trials, all 32 conditions passed native/Unicorn/oracle agreement on 384
verification cases each, with 32 × 24 state transitions for each stateful
condition. Applicable `DT_INIT` and complete `DT_INIT_ARRAY` initialization,
actual relocations and ELF weak-symbol semantics were exercised; no external-call
stubs supplied missing behavior. Real disassembly, initialized memory,
emulation/traces and numeric-entry IDA were available. Submitted programs ran
only in the bounded AST interpreter, not as host Python.

| Shipped profile | Exact recovered functions / four |
|---|---:|
| Plain | 4 / 4 |
| No passes | 4 / 4 |
| String encryption | 3 / 4 |
| Block shuffle | 4 / 4 |
| Indirect branch | 3 / 4 |
| MBA | 4 / 4 |
| Branch + MBA | 4 / 4 |
| All passes | 4 / 4 |

Exact recovery requires every held-out return and full output buffer to match,
plus every held-out state transition where applicable. Each function had 256
held-out cases; stateful conditions additionally had 32 sequences / 768
transitions. Queried inputs and contaminated sequences were excluded.

The two nonexact conditions must not be overinterpreted:

- IB `f2` matched 118/256 cases and 0/32 sequences; the same behavior was fully
  recovered under branch+MBA and all passes. One trial is not a reliable rate.
- String-encryption `f2` matched 0/256 after a submission with zero successful
  tool requests. `f2` has **no string-encryption opportunity**; this is model
  failure on a no-effect condition, not evidence of protection.

An earlier batch was interrupted by Eval infrastructure. It is retained as
**infrastructure-invalid**, with no difficulty/effort credit. Every baseline
condition was restarted without outcome-based selection under the same budgets;
byte-identical readiness evidence was reused rather than fabricated or rerun
unnecessarily.

The `default` completion route still exposes no exact backend identity,
temperature, token limit, usage or cost. Wave collection/continuation timestamps
include controller delay and are not per-condition attack effort or server
latency. These limits prevent a strong quantitative resistance claim. The
observed complete recovery of every all-pass family is nevertheless a concrete
counterexample to treating this corpus as “very difficult.”

### Matched candidate recovery

`recovery/candidate-seed0/trials.json` contains twelve completed submissions,
95 observed decisions and zero model-service errors. Native/CPU/oracle
readiness covered all 36 candidate profile/seed/function conditions, not only
the seed-0 analyst subset. Together with the baseline this is 26,112 verified
cases and 13,056 state transitions; numeric-entry IDA was available for all
68 conditions. Availability is not complete semantic recovery.

| Matched seed-0 profile | Shipped exact recovery | Candidate exact recovery |
|---|---:|---:|
| Indirect branch | 3 / 4 | 4 / 4 |
| Branch + MBA | 4 / 4 | 4 / 4 |
| All passes | 4 / 4 | 3 / 4 |

Candidate IB `f2` matched 256/256 cases and 32/32 sequences, versus the shipped
trial's 118/256 and 0/32. The shipped model submitted prematurely, so this
single unpinned-model observation does not establish a causal success-rate or
effort difference. It does not rescue the candidate's runtime/static failures.
Candidate all-pass `f3` matched 227/256 full results while correctly reproducing
all 256 mutated buffers; this partial recovery is not proof of useful protection.

The parent independently replayed ten decisive submissions, including both
all-pass sets and the IB stateful contrast; every score matched
(`recovery-replay.json`). `recovery/trial-audit.json` checks all 44 clean trial
budgets and model-facing envelopes. The earlier interrupted batch is excluded.
One baseline output-size tool error occurred in an ultimately recovered
condition and was not credited as resistance.

Use `verification-summary.json`, `recovery/report.json`, and
`recovery/interface.json` for final results and reuse. **No efficient-protection
improvement was demonstrated; the rejected candidate is not shipped.** Preserve
the functioning corpus/controller, but do not extrapolate these four families,
one trial per condition, one analyst seed, or an unidentified model route into
general resistance claims.

### Restoration

The shipped lowering was restored. Formatting, locked release build, **four
Cargo tests**, and **46 full native cases** passed. Re-executing the restored IR
path on eighteen retained inputs reproduced the shipped transformed IR hashes
exactly (`restored-ir/report.json`). Both scale-preparation fixes and all
correctness regressions remain intact. Baseline/candidate tools, sources,
artifacts and rejected results are retained; no gate was relaxed.

## Indirect-call pass (S2)

Implemented as **`--indirect-call`**, opt-in. Evidence root:
`target/indirect-call.nTmgb0/`; design/gates in `design.json`, explicit user size
exception in `size-exception.json`. This is a static call-target representation
change, **not encryption, runtime secrecy, or a demonstrated solution to the
strong LLM-resistance goal**.

### Supported subset and integration

`src/obfuscator/indirect_call.rs` plans ordinary calls and deduplicates eligible
targets into a seeded private mutable integer table. Each entry is
`ptrtoint(callee) + key`; a volatile load, wrapping subtraction and `inttoptr`
recover the original pointer. Only the existing call's callee operand changes.
Argument order/types, return type, calling convention, attributes, metadata,
normal tail marker and surrounding CFG remain intact. No constructor, wrapper,
heap cache or shared runtime state is added.

Eligible callees are exact defined **internal/private** functions. Existing
caller exclusions remain; declarations/interposable targets, intrinsics, inline
assembly, existing indirect/alias targets, varargs, `musttail`, operand bundles,
nonzero address spaces and `callees`/`prof`/`callee_type` target hints are skipped
with explicit reasons. `invoke`/`callbr` are outside the denominator. A nonempty
LLVM layout must support integral 32/64-bit default pointers with matching index
width and default program/global address spaces; absent layouts are not guessed
from the host. Coverage is optional and does not alter emitted IR.

Runtime order is string encryption → block shuffle → indirect branch →
indirect call → MBA. `all-passes` now means **five flags** in CI/measurement;
historical four-pass artifacts keep their recorded meaning. `call-mba` is a
measurement profile. The public CLI and internal `__obfuscate-ir` both support
the new flag. Static-library archive passthrough remains an existing limitation;
the CLI help now states it rather than promising archive rewriting.

### Correctness and native survival

- **Seven Cargo tests and 62 full native cases passed.** Three new unit tests and
  `tests/indirect_calls.ir` exercise C/fastcc, narrow integer extensions, floating
  arguments/results, aggregate/sret/byval copy behavior, memory effects,
  recursion, ordinary tails, exclusions and layout boundaries. Native
  `--indirect-call`, call+MBA and all-five cases run at seeds 0/1/42.
- The call-rich no-std corpus has neutral exports `f0`–`f3` and real private
  helpers, unlike the earlier call-free corpus. Its 15 eligible direct call
  sites include C/fastcc, aggregate/byval and memory calls. The independent
  observable semantics/oracle remain unchanged.
- Plain/no-pass/four-pass-reference and all three new feature profiles at
  seeds 0/1/42 passed 640 cases per export and 64 state sequences / 1,536
  transitions for original and stripped artifacts. Returns and complete buffers
  match, not merely checksums.
- Standalone S2 converted all 15 eligible IR sites. Native code has 13 indirect
  call instructions and three indirect tail transfers, versus zero indirect
  calls and one pre-existing indirect tail in no-pass. The pre-existing dispatch
  is not credited to S2.
- The integrated ABI fixture lowered successfully to x86-64 MSVC COFF and
  AArch64 Mach-O objects. Only Linux x86-64 native execution was exercised;
  cross-object emission does not establish native Windows/macOS correctness.

### Cost and explicit size decision

CPU-0-pinned ABBA runs used a real C hot-loop driver, five opposite-order pair
batches, 10,000 rounds over 80 inputs per family, and independent expected
checksums. Per-call input reset and full-buffer checksum are inside the work
timer equally for all profiles; these are workload costs, not isolated call
instruction latency. The earlier unpinned run is also retained.

| Call-rich family | S2 / no-pass median work ratio |
|---|---:|
| Branching | 0.996× |
| Loops | 1.084× |
| Mutable state | 1.002× |
| String processing | 1.015× |

All standalone call-rich ratios met the 1.25 gate. Existing integer/text/FFT
medians across seeds 0/1/42 were 0.969–1.008× no-pass, within the 1.15 gate;
they are not all call-heavy and must not replace the dedicated corpus.
Sub-percent apparent speedups are not meaningful optimization claims.
Combinations are more expensive: all five passes reached 3.102× on the loop
case and 2.293× on string processing versus no-pass. Do not attribute those
combined costs solely to S2 or enable every flag without measuring.

The tiny stripped call-rich library grew **5,448 → 5,968 bytes**: **+520 bytes,
9.5%**, failing the original 5% size gate. The user explicitly chose **“Keep it
opt-in”** after seeing this result. The original gate and measured exception
remain recorded; no silent threshold change or universal size guarantee is
claimed.

### Real-workspace result

Reused the pinned 470,852-line rust-analyzer corpus and the same ten selected
libraries. The all-five-pass build completed in **638.01 s**, sampled peak
aggregate RSS **2.55 GiB**, within the original 2,400-s / 12-GiB / 40-GiB
resource ceilings. This single feasibility observation is not a controlled
build-overhead ratio.

All ten modules verified; all selected rlib members exactly matched their
transformed objects; all **50 pass/module records** were complete and
`Cargo.lock` was unchanged. S2 considered **605,866** ordinary calls and
transformed **63,146/63,146 eligible** sites. Skips: 440,412 caller exclusions,
74,883 intrinsics, 23,795 declarations, 2,580 nonlocal targets, 1,047 existing
indirect/alias targets and three inline-assembly calls.

The final 53,124,328-byte executable passed the existing AST golden, token/span,
symbol and scoped type-inference/IDE checks. These retain the prior
test/fixture-LoC, selected-crate and small-runtime-subject caveats.
Raw scale records are under
`target/acceptance-baselines.ZkuFQz/scale/indirect-call-five-pass/` and
`lyra-indirect-call-five-pass.*`; `scale-command.json` records their reuse.

### Analysis effect and limits

On a matched stripped probe, IDA resolves the plain helper as `sub_10F0`.
The encoded probe retains a computed call through `off_4008 - key`, with no
automatic helper resolution. This establishes the narrow static-target effect,
not protection against an analyst who reads memory and decodes the entry.

The call-rich blinded evaluation used 24 conditions under equal twelve-decision
budgets, real initialized ELF emulation/disassembly/IDA and a bounded AST
submission interpreter. Every condition submitted an implementation:

| Profile | Exact recovered families / four |
|---|---:|
| Plain | 4 / 4 |
| No passes | 4 / 4 |
| Historical four-pass reference | 4 / 4 |
| Indirect call | 4 / 4 |
| Indirect call + MBA | 3 / 4 |
| All five passes | 3 / 4 |

The nonexact call+MBA string case matched 227/256 full results; the all-five loop
case matched 4/256. Partial or single-trial failures do not establish useful
general resistance. **All four standalone S2 semantics were recovered.** The
`default` route still lacks exact backend identity/settings/usage, so no
pinned-model success-rate or cost claim is made.

An initial queued run lost 23 completion results to handle expiration and is
marked infrastructure-invalid. The clean `recovery/trials-immediate/` run
restarted all conditions with the same budgets and immediate per-response
checkpoints; expired handles were not counted as protection. Reuse the complete
corpus/controller and retained logs, not the invalid batch.

**Decision:** retain the working pass as opt-in for its measured static
call-target effect and bounded standalone workload cost, with the explicit tiny
size exception. It does not justify “very difficult” semantic recovery, broad
platform claims or automatic use of the five-pass combination.

## Block-outlining pass (A1)

Evidence: `target/block-outline.Fz722s/`. The pass is **retained as opt-in**
(`--block-outline`) for its surviving call boundary and measured composition
with S2, not as a solution to the broader semantic-resistance goal.
`design.json` and `recovery-scope.json` predeclare the subset and evaluation.
Historical five-pass artifacts above are unchanged; current all-pass profiles
contain six flags.

### Implementation and build contract

- `src/obfuscator/block_outline.rs` owns seeded traversal, optional coverage and
  final verification. `block_outline.cpp` exposes one per-function C ABI over
  LLVM 22's `CodeExtractor`; LLVM owns SSA, PHI splitting and live-out lowering.
- One reachable, non-entry block per original function at pass entry; direct
  `br`, at least eight non-PHI/non-terminator instructions, at most six live-ins
  and two live-outs. SCC analysis excludes **irreducible cycles as well as
  natural loops**. Entry-PHI input accounting is conservative and can reject
  some otherwise-small boundaries. Selection uses a seeded reservoir without
  a candidate vector; a captured original last function prevents recursion
  into appended helpers. Single-block functions avoid needless CFG analysis.
- Source conventions are C/fast/cold. Boundary values are scalar integers
  through i128, float/double, or integral address-space-zero pointers; stack
  allocation address space must be zero. Regions cannot contain calls,
  intrinsics, inline assembly, allocas, `va_arg`, token/scalable results, or
  unsupported boundary types. Existing shared exclusions remain.
- Whole-function guards additionally exclude personality/EH, GC/coroutines,
  convergent and frame/stack/nonlocal-control constructs, `musttail`,
  `returns_twice`, operand bundles, inline assembly and special ABI arguments.
  Byval/sret storage can cross as ordinary pointers to its original storage;
  helper arguments do not inherit new `noundef` or other speculative promises.
- Helpers are internal and `noinline`, with matching default C helper/call
  conventions. Live-outs can add caller stack slots. No new freeze, exception
  suppression, alloca extraction, or successful-unobfuscated error fallback.
  Outlining runs after strings and before shuffle/branch/call/MBA.
- `build.rs` consumes **`DEP_LLVM_22_CONFIG_PATH`**, exported by the existing
  `llvm-sys` dependency (`links="llvm-22"`), and its C++ flags. Headers, ABI and
  LLVM library must belong to the same installation. Building now requires a
  compatible C++17 compiler and LLVM C++ development headers, not just the C
  SDK. CI setup checks the new headers. Native build/link/run evidence is Linux;
  emitted MSVC COFF and ARM64 Mach-O objects are **not** native-platform proof.

Coverage counts original blocks at pass entry, not extraction-generated
blocks/functions. `eligible` includes regions skipped by the one-per-function
selection budget; every considered block is transformed or explicitly skipped.

### Correctness and reproducibility

Final tools pass **12 Cargo tests and 78 full-suite native CLI cases**.
Five new tests cover entry/exit PHIs, both exits and bypass, live values,
narrow/wide integers and FP boundaries, volatile/atomic effects, recursion,
byval copy isolation, masked poison/undef, cycle exclusion, boundary budgets
and frame-sensitive cases. `tests/block_outlining.ir` executes explicit expected
results and requires four extractions; composed profiles require all four
helper calls to be encoded by S2. No-op transformations cannot satisfy it.

The unchanged four-family Rust corpus has one genuine eligible state-update
block, `h6::bb3`, in `f2`: 11 instructions, two inputs and two scalar outputs.
Other families have no direct A1 opportunity; source was not padded to create
one. All 21 profile/seed artifacts and stripped counterparts match independent
native oracles: 384 verification cases/export and 32 sequences/768 transitions
per artifact. After all model trials, an additional 256 held-out cases/export
and 32 sequences/768 transitions per artifact also passed (42 artifacts,
43,008 held-out native cases and 32,256 transitions).

Candidate initialized-ELF emulation passed 4,608 cases and 2,304 transitions.
All seven focused recovery conditions separately passed native/emulator/IDA
readiness. The native helper survives in every candidate seed; standalone
outlining has a resolved native call to it. Corrected
`native-transfer-inventory.json` leaves indirect jumps **unclassified** rather
than incorrectly counting intra-function indirect-branch dispatch as tail calls.

`tools/` preserves the timed candidate. A final single-block analysis shortcut
changes compiler work only; `final-ir-identity.json` proves **18 exact IR
matches** across all nine candidate profiles with coverage both on and off.
`final-tools/` and `native-final/report.json` identify the final build/run.
The final Lyra SHA256 is
`3defcacc4f7d4707fe2e2d83aadbea74f1fa99178376bfbdc20e8153d3fe2cfa`.
The first integration run's invalid naked-function test IR and incidental skip
count assertion are retained in `integration-first-run.log`; neither was a
production eligibility relaxation.

### Measured cost

CPU 2, native C driver, five ABBA pairs per family/comparison (ten ratios), seeds
0/1/42, 10,000 rounds: **720 candidate-comparison samples**. Input reset and
full-buffer checksum are inside every work timer; independent oracles are not.
Loader/process wall time is recorded separately. Stateful sequences verify
semantics separately; timed state calls reset their input.

| Genuine state-transition comparison | Seed 0 | Seed 1 | Seed 42 | Gate |
|---|---:|---:|---:|---:|
| Outline / no-pass | 0.975× | 1.002× | 0.991× | ≤1.25× |
| Outline+call / indirect-call | 0.996× | 0.990× | 1.023× | ≤1.25× |
| All six / prior five | 1.010× | 0.999× | 1.009× | ≤1.15× |

All corpus medians meet their declared ceilings. Stripped corpus growth is
**72 bytes** standalone, **120 bytes** over S2, and **168/232/360 bytes** over
the respective five-pass seeds, below `max(5%, 1024 bytes)`. Tiny negative
ratios do not establish a speedup. This is one genuinely transformed family,
not a representative application-performance portfolio.

The 26-case public measurement matrix passed correctness but exposed timing
noise: the first blocked text comparison reported **1.552×**, beyond 1.15×.
Both public fixtures had **zero eligible A1 blocks**, and every standalone
artifact was byte-identical to its no-pass reference. The failed numeric row
remains in `runtime-gates.json`; it was not silently replaced or attributed to
changed code. A separately predeclared, all-workload/all-seed ABBA diagnosis
(756 samples, unchanged inputs/iterations/gate) gave **0.987–1.008×**.
See `public-artifact-identity.json`, `public-paired-design.json` and
`public-paired-results.json`. These are no-op controls, not A1 efficacy evidence.

### Real workspace and analysis limits

The pinned 470,852-line rust-analyzer workspace built with all six flags over
the same ten selected libraries in **789.321 s**, with sampled owned aggregate
RSS **2.535 GiB** and cumulative retained scale storage **10.648 GiB**.
No 2400 s / 12 GiB / 40 GiB ceiling was breached. Model/controller activity
overlapped the build, not microbenchmarks; this is not a paired causal build-time
comparison with the older 638-second S2 run.

All ten modules verified, all selected archive members exactly matched their
transformed objects, all **60 coverage records** were present, and Cargo.lock
was unchanged. A1 considered **717,442** blocks, found **203 eligible**, and
extracted **93**; the remaining 110 eligible regions hit the selection budget.
Shared function exclusions account for 607,630 blocks, additional unsafe
functions 73,696, cycles 3,679, and boundary budgets 109; full reasons and
per-crate counts are in `scale-coverage-summary.json`.

The final 53,169,752-byte executable passed the pinned AST golden, lossless
tokens/spans, symbols, and scoped no-sysroot type-inference/IDE checks. Retain
the prior selected-crate, test/fixture-LoC and small-runtime-subject caveats.
Raw records reuse `target/acceptance-baselines.ZkuFQz/scale/` under
`block-outline-six-pass/` and `lyra-block-outline-six-pass.*`; the new root's
`scale-command.json` records the exact invocation.

Matched stripped IDA callers show `sub_1D50(...)` with outlining alone and a
computed call through `off_4110 - 1647497387` with outline+call. This verifies
the narrow new static-target effect, not inability to decode its address.

The seven blinded `f2` trials used equal twelve-decision/64-input budgets and
immediate response checkpoints, real disassembly/emulation/IDA, and independent
held-out return-and-buffer scoring:

| Profile | Held-out exact / 256 | Complete sequences / 32 | Decisions |
|---|---:|---:|---:|
| Plain | 256 | 32 | 8 |
| No passes | 256 | 32 | 7 |
| Indirect call | 256 | 32 | 11 |
| Prior five passes | 256 | 32 | 12 |
| Block outlining | 256 | 32 | 6 |
| Outline + indirect call | 202 | 0 | 12 |
| All six passes | 202 | 0 | 12 |

All conditions submitted; no model-service result was lost. However, **24
malformed JSON responses** consumed decisions, and outline+call also requested
an unmapped address. Exact model identity/settings remain unavailable. These
single-trial partial failures are not a clean protection score; standalone A1
was fully recovered. Full logs, scores, errors and budget/marker audits are in
`recovery/`. Do not retry only failed model conditions or credit protocol/tool
friction as obfuscation strength.

**Decision:** retain opt-in, with no new cost exception, for the observed helper
boundary and S2 composition within the stated scoped budgets. Narrow coverage,
unverified native Windows/macOS builds, broader runtime workloads, and robust
pinned-model resistance remain open. Do not widen the subset or enable it by
default from these results alone.

## Function-wrapper experiment (A3)

Evidence: `target/function-wrap.M6d5fh/`. **Rejected; the shipped six-pass
pipeline is unchanged. There is no `--function-wrap` CLI flag.** The proposed
flag appears only in experiment profile metadata.

### Candidate and correctness

`design.json` predeclared a prototype-first decision: production integration,
permanent regressions, blinded model trials and a new large-workspace run were
conditional on passing correctness, cost and incremental-effect gates.
The candidate preserves the original private/internal Function object, signature,
calling convention, attributes and uses. It moves, rather than clones, the body
into one internal implementation and remaps argument uses. Both functions are
`noinline`; the forwarding call is explicitly `notail`, with matching ABI
parameter/return attributes but no copied function-only callsite attributes.

The conservative subset requires scalar leaf functions, at least eight
non-PHI/non-terminator instructions and at most six parameters. Variadics,
special-storage arguments, EH/personality, GC, frame-sensitive constructs,
allocas, unsupported types/attributes and ordinary calls are excluded. A small
pure-integer intrinsic whitelist remains. Wrapping precedes the existing six
passes, so generated string/A1 helpers are not reflexively wrapped. The
original-end iterator prevents rewrapping generated implementations.

The standalone LLVM tool and fixture live under the evidence root, not `src/`.
Actual Rust attributes initially produced zero candidates; preserving and
explicitly allowing `nonlazybind` and `probe-stack="inline-asm"` restored the
intended ordinary codegen subset. Unknown/custom probing contracts still reject.
The initial result and all integration corrections are retained.

Native ABI probes wrapped **13 functions** and accounted for **33 exclusions**.
Original and wrapped executables passed at O0/O3; **18 additional real shipped-CLI
composition executions** passed at O0/O3 across seeds 0/1/42. Probes exercise
C/fast/cold conventions, narrow extension and i128, FP finite/NaN/infinity and
signed-zero/subnormal cases, pointer/void returns, aliasing, volatile/atomic
memory, loops/PHIs, intrinsics, and address-taken invocation.

The unchanged call-rich Rust corpus has exactly two eligible leaves, `h8` and
`h9`, both used by the text family `f3`. Matched controls and candidates use the
same original optimized Rust IR, LLVM 22 PIC codegen, GNU shared linking and
four-export visibility script. This is **native IR reconstruction**, not a
production Cargo-feature run; the archived Cargo-plain binary is explicitly not
a backend-matched timing control.

All **24 profile/seed variants plus stripped counterparts** passed 1,536 native
cases and 768 state transitions per artifact: **73,728 cases and 36,864
transitions**. Initialized-ELF emulation independently passed **96 export
conditions, 36,864 cases and 18,432 transitions**. All **24 new call boundaries**
survived native codegen; rejection was not caused by optimized-away wrappers.

### Incremental analysis effect

`forwarders.py` receives only a stripped ELF and output path, never source, IR,
private symbols, expected helper names, or expected target addresses. Real
bounded emulation discovers call entries. A conservative instruction-pattern
check recognizes a single forwarding call with preserved argument/return
registers and balanced stack, resolving encoded targets from initialized image
memory. A separate controller grades its output against retained correspondence.
This is call-graph contraction, not binary rewriting or a universal
arbitrary-stack/variadic-ABI equivalence proof.

The recognizer identified and correctly resolved **24/24 added wrappers**:
standalone, with S2, with A1+S2, and with the full six-pass reference, at all three
seeds. The twelve controls had zero recognized forwarding layers. Its first
version lacked a normal `add reg, [RIP+disp]` constant-target form; that setup
limitation is retained and was never credited as protection.

Actual IDA/Hex-Rays runs did not automatically discover the selected private
entries in either the S2 control or wrapped composition. Public `f3` decompiled
in both. These pre-existing discovery gaps do not establish added A3 strength.
No function names/types or manually created boundaries were injected.

### Runtime and retention

**960 native samples**, CPU 2, five ABBA pairs per family/comparison, 10,000
rounds and seeds 0/1/42. Input reset/full-buffer checksum are equally timed;
independent checksums are outside the timer. Loader/process time is separate,
and analysis/build activity did not overlap timed comparisons.

| Genuine text-family comparison | Seed 0 | Seed 1 | Seed 42 | Ceiling |
|---|---:|---:|---:|---:|
| Wrappers / no-pass | 1.358× | 1.345× | 1.377× | 1.25× |
| Wrappers+S2 / S2 | 1.306× | 1.304× | 1.316× | 1.25× |
| Wrappers+A1+S2 / A1+S2 | 1.430× | 1.438× | 1.424× | 1.25× |
| Seven-pass candidate / six-pass reference | 1.365× | 1.082× | 1.367× | 1.15× |

**11/12 transformed-family runtime comparisons failed.** Other families remain
measured controls, not direct wrapper-effect evidence. Stripped whole-file growth
was zero standalone and 16 bytes in compositions, within the size allowance;
alignment slack hides code/unwind growth. For example, standalone `.text` grew
26 bytes, `.eh_frame` 48 bytes and `.eh_frame_hdr` 16 bytes. The section inventory
is distinct from stripped whole-file size; no zero-cost inference is warranted.

**Decision:** do not integrate this forwarding design. It adds material runtime
cost while the new layer is readily normalized under the stated tool model.
Per the predeclared stopping policy, no new LLM semantic-recovery trial,
production full-suite run or >300k-line build was performed after both mandatory
gates failed. Prior A1/S2 results remain historical, not rerun evidence here.
This does not prove every possible wrapper design useless; a different proposal
must establish a new benefit rather than adding unrelated transformations to
rescue this one.

`decision.json`, `cost-gates.json`, `analysis-summary.json`,
`native-boundary-survival.json`, `static-attempts.json`, `abi-composition.json`,
and `corpus/commands.json` retain the exact evidence and reproduction paths.
A4 was assessed next; its separate results follow.

## Constant-specialisation experiment (A4)

Evidence: `target/constant-specialize.03meiQ/`. **Rejected as an obfuscation
pass; production still has six passes and no `--constant-specialize` flag.**
The proposed name exists only in experiment metadata.

### Lowering and supported contract

`design.json` predeclared a native-prototype-first gate. Original ordinary direct
calls are grouped by callee and exact `ConstantInt` argument tuple. A shared
specialised clone serves each selected tuple; constants and newly dead residual
formals are removed, and selected calls are rebuilt with correctly remapped
original callsite attributes, calling convention and tail/notail markers.
There is no forwarding layer or additional runtime call.

LLVM cloning/remapping and integer/CFG simplification handle SSA and dead blocks.
Only clones are simplified; no module-wide optimizer, floating arithmetic
rewrite, new freeze or inferred argument promise is introduced. Signature
trimming moves the already-cloned body rather than copying it again. Original
Function identities, aliases, pointer users and self-address constants remain
generic; only truly unused generics are erased after migration.

The subset is local, explicitly `nounwind` scalar leaves with 2–6 arguments,
at most 128 instructions/16 blocks, no special-storage ABI, GC/EH/frame hazards,
allocas, unsupported metadata/attributes or ordinary body calls. A small
pure-integer intrinsic whitelist is allowed. An existing personality pointer is
preserved on a `nounwind` leaf without EH constructs; this does not change any
shipped pass's exclusions.

Selection is deterministic: more constant parameters, more matching callsites,
then canonical tuple ordering. Limits are two clones/callee, sixteen/module,
and gross added instructions ≤`min(1024,max(32,original instructions/4))`.
Seeds affect subsequent shipped passes, not this selection. Callee and original
callsite attribute sets are handled separately; function-only or stale
argument-indexed contracts are not blindly attached to new calls.

### Correctness and actual opportunities

The ABI fixture rewrote **17 callsites into 16 clones**, sharing a clone between
equivalent tuples, removing two additionally dead parameters, and preserving
required generics. **22 native executions** passed: original/specialised O0/O3
plus eighteen real shipped-CLI composed O0/O3 runs at seeds 0/1/42.
Cases cover C/fast/cold conventions, dropped first/middle/last arguments, narrow
extension and i128, FP forwarding including negative-zero bits, remapped
pointer/callsite-only attributes, aliasing, volatile/atomic memory, loops/PHIs,
constant branch pruning, original self-address returns and indirect generic
invocation. Exact budgets and exclusion accounting are retained.

Original and specialised O0 fixture links both warned about PIE text relocations
in `.llvm_stackmaps` from an unexecuted operand-bundle exclusion specimen.
The diagnostic was retained, not suppressed or attributed to a new regression;
this fixture is not a production-hardening or unsupported-ABI runtime claim.

The unchanged Rust corpus selects two multiply/add tuples from `h0`:
`(multiply=3, add=1)` and `(multiply=0x9e3779b185ebca87, add=0)`, plus rotate
counts 5 and 17 from `h1`. Four calls change, six constant formals disappear,
and eleven clone instructions fit the sixty-instruction budget. The generic
`h0` remains for the text hash; generic/address-taken `h1` remains for other
counts and pointer dispatch. The third tuple per callee is an explicit budget
skip. No Rust source or oracle was altered to create these opportunities.

All **24 profile/seed variants and stripped counterparts** passed **73,728 native
cases and 36,864 state transitions**. Initialized ELF emulation independently
passed **96 export conditions, 36,864 cases and 18,432 transitions**.
Each candidate artifact retains four distinct native entries and body hashes:
**48 clone instances**, not 48 distinct source algorithms.
These are matched LLVM22/PIC/shared-link IR reconstructions, not a production
Cargo-feature run or a backend-matched comparison with archived Cargo plain.

### Binary-only semantic matching

`semantic_match.py` receives only two stripped ELFs and the public four-export
ABI. Thirty-two deterministic probes/export discover real call and unclassified
jump destinations, incoming integer registers and return observations. No
source, IR, private symbols, clone inventory, oracle or expected bindings enter
the matcher. A separate controller grades the result.

Bounded Capstone lifting produces 64-bit scalar return expressions, rejecting
unsupported operations/effects, conditional control or unknown entry state.
Lifted expressions must first agree with real emulated return observations.
The matcher substitutes observed invariant used registers into reference
expressions, compacts live inputs, filters by independent concrete fingerprints,
then asks Z3 whether reference and candidate results can differ.

**All 48 instances matched their true generic functions**, including S2,
A1+S2 and full-pipeline compositions at every seed. Every credited inequality
query was **UNSAT**; none was unknown/timed out. For example, the generic
multiply/add routine with `(3,1)` becomes the same `3*x+1` expression as its
specialised native `lea`. The large multiplier and fixed rotate/XOR variants
were equally recognisable as partial evaluations.

This proves the supported scalar-expression relationship for the chosen
bindings, not that sampled callers always supply constants or that an entire
application has been recovered. Jump destinations are not automatically labelled
tail calls. Z3 4.15.4 is installed only under this experiment's `python-deps`;
production dependencies and the old analysis environment are unchanged.

### Cost and decision

**960 native samples**, CPU 2, five ABBA pairs, 10,000 rounds, three seeds;
independent full-result checksums, loader/process time separate, and no
analysis/build overlap. Median-ratio ranges across seeds:

| Candidate / matched reference | f0 | f1 | f2 | f3 control |
|---|---:|---:|---:|---:|
| Specialise / no-pass | 1.004–1.016× | 1.079–1.082× | 1.003–1.006× | 1.007–1.018× |
| Specialise+S2 / S2 | 0.999–1.017× | 0.973–0.976× | 1.001–1.008× | 1.027–1.028× |
| Specialise+A1+S2 / A1+S2 | 1.005–1.017× | 0.972–0.977× | 0.999–1.004× | 1.099–1.102× |
| Seven-pass / six-pass | 0.984–1.005× | 0.967–1.051× | 0.997–1.009× | 0.940–1.051× |

The ceiling was 1.10× for every family/profile/seed. Two f3 composition/control
medians exceeded it: **1.102214×** at seed 0 and **1.101000×** at seed 42;
seed 1 was 1.098671×. No selected clone executes in f3, although the composed
artifact changes. These are near-threshold observations, not an established
microarchitectural cause; they were not selectively rerun to obtain a pass.
All directly transformed f0/f1/f2 medians passed. Stripped file growth was zero
standalone or 32 bytes composed, and `.text` growth was 16–69 bytes; all size
ceilings passed. Alignment slack does not imply zero code growth.

**Decision:** do not ship this bounded specialisation candidate as obfuscation.
The decisive failure is incremental matching, not missing clones or incorrect
execution. Modest performance changes do not turn an ordinary partial-evaluation
optimization into demonstrated protection. Under the predeclared early-stop
policy, no new LLM trial, IDA run, production full-suite run, cross-platform run
or >300k-line build followed the failed gate. Prior verification remains
historical, not newly exercised coverage.

Retain `decision.json`, `verification-summary.json`, `native-clone-survival.json`,
`matching-protocol.json`, `analysis-summary.json`, `cost-gates.json`,
`size-gates.json`, `abi-composition.json` and `corpus/commands.json`.
B1/C3's bounded return-contract proposal was assessed next; its results follow.

## MBA return-contract experiment

Evidence: `target/mba-return-defined.wp09o3/`. **The B1/C3 candidate was rejected
as proposed. Production MBA and the six-pass pipeline are unchanged.**

### Candidate and semantic basis

Only an isolated exact source copy was modified. The candidate added an
ordinary scalar-integer `call` result with an **explicit callsite return
`noundef` attribute** to the defined-value set. Callee-only and argument-only
attributes did not qualify; inline assembly and operand bundles were excluded.
No call, ABI, attribute, freeze, floating operation or existing identity was
introduced or changed. Two rounds, the 60% gate, seed policy and poison/shift
guards remained intact.

The existing PHI transfer still required all inputs proved. A call contract
could supply a legitimate new anchor; no general cyclic-PHI induction or
range guessing was added. This was an extension of the proof frontier, not a
promise that every call or frozen unknown value was safe.

The [LLVM return-attribute contract](https://llvm.org/docs/LangRef.html#parameter-attributes)
says undefined/poison value bits violate `noundef`. `semantic-basis.json`
preserves the LLVM 22 source and exact rationale. Native backend checks remained
mandatory because the earlier freeze audit already showed IR proof was
insufficient on this toolchain.

The frozen candidate Lyra SHA256 is
`1a6059c842993b11c1a5e7d40a8be02a450d883fdaca65b762e0cf46843cdfe1`;
the reference remains
`3defcacc4f7d4707fe2e2d83aadbea74f1fa99178376bfbdc20e8153d3fe2cfa`.
Only copied `src/obfuscator/mba.rs` differs; sibling wrapper/linker binaries are
hash-identical. All **12 existing Cargo tests** passed in that isolated copy.

### Coverage and normal-data checks

The unchanged Rust corpus and independent oracle produced **48 original/stripped
artifacts**, passing **73,728 native cases and 36,864 state transitions**.
Initialized-ELF emulation passed **96 conditions, 36,864 cases and 18,432
transitions**. These finite normal-data results did not override the later
adversarial failure.

Round-zero eligible counts increased **24→31** for MBA and S2+MBA,
**23→30** for A1+S2+MBA, and **29→37** for all six passes. Round zero is original
to the MBA stage and may contain earlier-pass helpers; round one also visits
generated expressions. Random-gate changes and generated visits were recorded
separately, not counted as additional original protection. Stripped file sizes
were unchanged; `.text` grew **16–194 bytes**, within the size caps.

### Candidate-only lowering regression

The priority harness exercised seven new call/unknown-origin modules across
original, shipped and candidate profiles, seeds 0/1/42, and direct llc O0/O2/O3
plus clang O0/O3. **9,092 executed checks passed**, with no wrong-value or
IR-invariance failure. However, **twelve candidate compilations failed**:

```text
LLVM ERROR: Cannot select: intrinsic %llvm.ssa.copy
Running pass 'X86 DAG->DAG Instruction Selection'
on function '@frozen_ssa_copy_or'
```

The input contains an existing `freeze undef`, an explicit `noundef` result from
`llvm.ssa.copy.i8`, and OR with all ones. Original and shipped-pass controls
compiled and returned the expected value. Newly admitted MBA expansion kept the
copy intrinsic live in a form instruction selection could not lower. The
candidate failed under **llc O0/O2/O3 and clang O0 at every seed**; clang O3
succeeded. This is a lowering compatibility regression for the declared subset,
not evidence of harder analysis or an observed wrong numerical result.

The generic harness reports compiler errors as `compile_gap`.
`backend-regression.json` correctly reclassifies these matched candidate-only
failures rather than treating them as an unavailable environment. No failing
intrinsic was erased, no compiler error suppressed, and no new freeze or
silent supported-subset restriction was used to manufacture a pass.

The previously retained backend counterexample was accepted as ground truth,
not rerun for confirmation. The new contract-mediated cases and unchanged
unknown-origin negatives supplied this experiment's evidence.

### Focused proof and stopping decision

Pinned Alive2, with `--always-verify --smt-to=30000` and undef/poison checks
enabled, proved **two actual emitted functions**: `frozen_ssa_copy_or` and
`frozen_noinline_or`. Both reported one correct transformation, zero incorrect,
zero failed-to-prove and zero tool errors. The first is the exact failing
lowering target: **correct LLVM refinement still did not compile on Lyra's
direct llc path**.

`focused-proof-results.json` retains the observed verdicts, exact commands and
source/target paths. It is a transcript-derived summary, not a raw solver log:
the short completed outputs had no artifact URI and their process handles were
unavailable for archival. The IR itself is retained exactly.

A preliminary binary-only scalar-leaf smoke also proved two expression matches;
it was not a full incremental-effect or whole-program recovery evaluation.
After the hard native gate failed, the full prepared exhaustive/formal matrix,
runtime microbenchmarks, full matching matrix, new LLM trials, production
regression suite and new large-workspace/platform runs were **not performed**.
No performance or resistance acceptance is claimed.

**Decision:** keep the shipped producer set. The return contract is a valid
semantic premise, but this broad producer rule is not backend-compatible as
tested. A narrower contract or another backend strategy can be proposed
separately; this is not a rejection of every proof-based MBA extension.
Retain `design.json`, `decision.json`, `backend-regression.json`,
`verification-summary.json`, `proof-critical-results/`, `focused-proof-results.json`,
the isolated source/tools and `corpus/commands.json`.
B2 was assessed next; its separate results follow.

## Custom switch-lowering experiment (B2)

Evidence: `target/switch-lower.3axz3x/`. **Do not promote this bounded candidate.**
Production remains the six-pass pipeline, with no `--switch-lower` flag.
The isolated candidate demonstrated a static decompiler effect, not an added
semantic-recovery barrier against the tested tool-equipped analyst.

### Supported lowering and effect contracts

The copied Rust implementation replaces eligible switches in their existing
blocks with an encoded block-address table and `indirectbr`. Conditions are
scalar i8/i16/i32/i64, with 3–32 explicit cases, at least two distinct
destinations, unsigned span ≤64 and span ≤twice the case count. Sparse,
wrap-spanning and unsupported switches remain explicitly accounted skips.

Subtract the minimum in the original integer width, use an unsigned bound to
select either the offset or a default sentinel, then convert to pointer width.
A nonzero XOR mask permutes a power-of-two table of at most 128 slots.
Entries are block addresses plus a seeded positive 31-bit key; dispatch
subtracts that key. Holes and padding select the original default. Original
successor order and duplicate multiplicity, including self-edges, are preserved
verbatim, so PHIs require no rewrite. No blocks, calls, constructors, allocation,
freeze instructions or new poison flags are introduced.

The table is **private constant with an ordinary nonvolatile load**. An initial
volatile-table proposal was corrected before candidate measurement: it would
add effects incompatible with existing memory/synchronization summaries.
[LLVM's memory contract](https://llvm.org/docs/LangRef.html#function-attributes)
permits constant-global reads under `memory(none)`. Existing function,
callsite and transitive summaries remain intact; no blanket attribute weakening
was used. `effect-contract-decision.json` retains the LLVM 22 sources and rationale.

Shared function exclusions and S2's integral 32/64-bit pointer-layout policy
are reused. Nondefault function address spaces, naked/presplit-coroutine bodies,
invoke/callbr functions and switch metadata are excluded. Module verification
is mandatory even for skip-only inputs; errors do not produce a successful
unobfuscated fallback.

### Correctness and genuine opportunities

The isolated candidate passed **19 Cargo tests**, including seven new behavioral
regressions. Both early and late placements passed **26 native fixture runs**
each at clang O0/O3: original plus B2, B2+MBA, B2+S1 and seven-pass profiles
at seeds 0/1/42. Coverage includes exhaustive i8 selectors, wider negative and
high-bit ranges, holes, default boundaries, repeated-destination and self-loop
PHIs, recursion, volatile/atomic state, maximum table size, restrictive memory
contracts and exclusion accounting. The fixture transforms five switches into
28 slots/224 bytes. LLVM 22's case-value API is used; its ordinary switch
operand array is not the old alternating value/destination representation.

The unchanged authorized Rust corpus has one supported switch in state helper
`h6`: cases 0/1/2 and default. The sparse `h8` switch is skipped. Each candidate
therefore transforms **one of two considered switches**, adding four slots/
32 bytes. No source padding or oracle changes manufacture coverage.

For **each placement**, 24 matched profile/seed variants and their stripped
counterparts passed **73,728 native cases and 36,864 state transitions**.
Initialized-ELF emulation passed **96 conditions, 36,864 cases and 18,432
transitions**. These are same-LLVM22/PIC/shared-link reconstructions, not a
comparison with a differently compiled archived Cargo plain artifact.
The actual public Cargo CLI was also exercised: early B2-only and late
seven-pass shared objects each passed **1,536 cases and 768 transitions**.

A focused constant-table probe passed 1,792 cases under both direct llc O2 and
LLVM default O3 followed by llc O2, retaining the encoded native jump and
`nounwind nosync memory(none)`. Early seven-pass fixture emission also succeeded
for Windows x64 COFF and macOS ARM64 Mach-O. Object emission and verifier-only
32-bit layout checks are **not native cross-platform execution**.

### Placement and cost

The first placement ran B2 before S1/S2/MBA. Its seven-versus-six text control
at seed 0 took **1.215098×**, exceeding the unchanged 1.10× ceiling.
Although B2 did not transform that family's switch, `h8` changed from 48 to
63 native instructions. New dispatch arithmetic perturbed subsequent MBA
choices; the precise machine-level cause of the runtime change was not isolated.
This result remains in `cost-gates.json`, `composition-interference.json`,
`tools/`, `corpus/` and `early-source-snapshot/`.

`late-order-design.json` predeclared a separate revision: B2 runs **after all
six shipped passes**, with the same algorithm, subset, seeds and ceilings.
All twelve matched old-pass prefixes are byte-identical; only `h6` changes
in the post-B2 IR. The entire matrix was rerun, not just the failing row.

Each placement measured 960 native samples on CPU 2: five ABBA pairs, 10,000
rounds, ten ratios/family/profile/seed. Independent checksums include the full
result buffer; loader/process time is separate. No analysis or build overlapped
timing. Late-placement state-family medians:

| Candidate / matched reference | Seed 0 | Seed 1 | Seed 42 | Ceiling |
|---|---:|---:|---:|---:|
| B2 / no-pass | 1.010× | 1.003× | 1.005× | 1.15× |
| B2+MBA / MBA | 0.996× | 1.009× | 1.005× | 1.15× |
| B2+S1 / S1 | 1.013× | 1.004× | 1.005× | 1.15× |
| Seven-pass / six-pass | 1.008× | 1.000× | 0.999× | 1.10× |

**All 48 late runtime comparisons passed**, including mandatory nontransformed
family controls. Stripped file growth was zero; `.text` growth was 0–16 bytes,
within `max(5%,1024 bytes)` file and `max(5%,256 bytes)` text allowances.
Table storage and alignment slack are not zero-cost protection. Exact ratios,
variation and sizes are retained in `late-cost-gates.json`.

### Static effect, runtime decoding and blinded recovery

At seed 0, matched IDA 9.1/Hex-Rays exports resolve selector/case flow in the
no-pass, S1 and six-pass `h6` controls. B2 counterparts end the recovered
function at `jmp rdx` after selector clamping, without recovering its case
bodies. Settings, stripped inputs, numeric selectors and fresh databases are
matched; no manual boundaries, names or types were injected.

The binary-only observer receives just the stripped ELF and public ABI.
Thirty-two selector probes recover **all twelve candidate four-target dispatch
mappings**, including the observed period of four and constant decode delta.
Initialized integer-table words expose the targets directly after subtraction.
Reference profiles have no newly encoded multiway site. This is an observed
mapping over the probes, not a universal internal-input or whole-program proof;
an indirect jump is not automatically classified as a tail call.

Fresh blinded recovery used eight anonymous seed-0 conditions: the four matched
pairs above, restricted to the genuinely transformed state family. Each had
the same 12-decision/64-chosen-case budget, real disassembly, decompilation,
initialized memory and CPU emulation. Native/emulator/oracle readiness passed
for every condition before model calls. Source, IR, profile labels, hidden
sets, oracle and cross-condition history were withheld.

**All eight submissions were fully recovered:** 256/256 held-out return/buffer
cases and 32/32 stateful sequences, comprising 768 transitions per condition.
Standalone B2, B2+S1 and seven-pass candidates needed no chosen-input emulation
queries; disassembly and initialized memory sufficed. B2+MBA used six queries.
No held-out case or sequence overlapped a queried transition.

There were 71 model responses and 22 protocol errors, all charged to the
original budget. The six-pass control also requested an unmapped disassembly
range and exceeded one tool-output bound; the seven-pass candidate exceeded
one output bound too. None is protection. Exact model identity, sampling
settings and usage remain unavailable. These single trials do not establish
a success-rate distribution or a reliable effort increase. They do establish
complete recovery of every submitted finite condition.

### Real-workspace scale check

The already-running scale validation also completed successfully. It uses the
same pinned rust-analyzer revision
`3332931d313fad08166571c05d7237d3a4d77ac2`: **470,852 first-party non-generated
Rust code lines**, including tests/fixtures. The existing 1,485 source hashes
and lexical-count convention were revalidated, not replaced with generated
padding or a fresh incompatible recount.

The same ten selected libraries represent 391,276 lexical source lines:
`hir-ty`, `hir-def`, `hir`, `ide`, `ide-assists`, `ide-completion`,
`ide-diagnostics`, `ide-db`, `parser` and `syntax`. All seven flags ran at seed 0,
with B2 last. Every selected original/transformed module verified, all ten
archives contained the exact replacement object, and all 70 pass-coverage
records were retained in order. B2 considered **11,864 stage-entry switches**,
transformed **2,412**, and explicitly skipped **9,452**, adding 20,864 slots/
166,912 table bytes. Stage-entry counts may include earlier-pass helpers;
they are not transformed-source-line coverage.

The controlled build took **748.977 seconds** with **2.539 GiB sampled aggregate
owned-process RSS**. Harvest took 31.276 seconds. Final cumulative scale
evidence occupied **12.709 GiB**. No 2,400-second-per-controlled-phase,
12-GiB sampled-RSS or 40-GiB cumulative-evidence safeguard was breached.
Sampling may miss short peaks or escaped descendants; this is a bounded
resource run, not a paired build-speed or application-runtime comparison.

The actual 53,850,504-byte linked executable passed the pinned full AST golden,
token/source-span and function-symbol checks. Real no-sysroot HIR/MIR/IDE
analysis produced exactly the intended `u32`/`bool` mismatch at the expected
span, with zero inference panics. These finite checks are not exhaustive IDE
correctness. The executable SHA256 is
`efae8f3cd13bef7325a89e911b5d9e41e6608f95b5aab44f4ccf5961a54b5e17`.

See `scale-summary.json`, `scale-plan.json`, `scale-command.json` and
`scale-run.log`; retained build/archive/native records are under
`target/acceptance-baselines.ZkuFQz/scale/switch-lower-b2-late-seven-pass/`.
Scale success does not override the complete blinded recovery result.

### Decision and retained evidence

Do not add a production pass merely because its encoded jump survives and its
automatic pseudo-C is incomplete. Late placement repaired the measured cost
failure without demonstrating the desired incremental recovery barrier.
This rejects this bounded obfuscation proposal, not every switch representation
or a future separately justified structural consumer.

Production source and fixtures match their frozen pre-experiment hashes.
No production full-suite rerun or native macOS/Windows run is claimed.
Keep `decision.json`, both designs/source snapshots/tool sets, both corpus
directories and command manifests, `late-prefix-identity.json`,
`late-analysis-effect.json`, both cost reports, public Cargo smoke artifacts,
native fixture results, `verification-summary.json`, `scale-summary.json`,
`recovery/{protocol,summary,trial-audit,tool-errors}.json` and the raw trial
exchanges. The independent reviewer service failed before
analysis; `contract-review.json` records parent review, not independent approval.
B3 was assessed next; its separate results follow.

## Address-offset materialisation experiment (B3)

Evidence: `target/address-offset.MMlFiI/`. **Reject this bounded design.**
Every matched native `.text` growth comparison exceeded its declared allowance.
Production still has six opt-in passes and no `--address-offset` flag.

### Representation and supported contract

A standalone LLVM 22 C++ prototype runs after the unchanged shipped-pass
prefix. It changes only the constant index operand of an existing byte GEP.
The original instruction, base pointer, source type, flags, metadata and users
remain; original loads/stores, volatile/atomic operations and call attributes
are not rebuilt. No data pointer is reconstructed with `inttoptr`.

Eligible sites have one nonzero constant index, source element `i8`, a scalar
default-address-space base directly supplied by an explicit `noundef` function
parameter, and matching integral 32/64-bit pointer/index widths. Unknown or
derived bases, dynamic/multiple/typed indices and parameters aligned enough
to fix the selected address bits are skipped. Function exclusions include
EH/coroutines, naked/presplit bodies, invoke/callbr and nondefault address spaces.
The definedness requirement avoids introducing independent undef choices;
there is no new freeze or widening of the shipped MBA policy.

Each transformed function gets one private constant eight-word table. With
pointer-sized modular arithmetic, pointer `p`, original offset `C`, key `K`
and nonzero XOR permutation `m`:

```text
r = (ptrtoint(p) >> 4) & 7
T[r xor m] = K - r
reconstructed_offset = T[r xor m] + r - (K - C)
```

The original GEP receives `reconstructed_offset`. Table loads are ordinary,
nonvolatile constant-global reads, permitted by the
[LLVM memory-effect contract](https://llvm.org/docs/LangRef.html#function-attributes)
even under `memory(none)`. Existing function/callsite summaries remain intact.
There are no new calls, blocks, constructors or mutable globals. The prototype
uses a recorded SplitMix64 stream at seeds 0/1/42, not a production RNG/API claim.
Any justified integration would still need the existing `seed::for_pass`
convention and new final-artifact validation.

### Correctness and actual coverage

Z3 4.15.4 discharged **28 UNSAT queries**: table bounds and offset identity at
32/64 bits for every supported permutation, with symbolic pointer bits, key and
offset. This is an algebraic proof, **not** an LLVM provenance/refinement or
whole-program proof.

Focused validation passed **40 actual native executions**: original and three
candidate seeds, memory and pointer-result fixtures, direct llc O0/O2/O3 and
clang O0/O3. They performed **123,360 full-buffer memory checks** and
**345,600 pointer-result checks**. Cases include negative/high-bit offsets,
all selected low-address residues, wrap/carry boundaries, valid object-interior
pointers, unaligned/aliased accesses, volatile/atomic operations, repeated
calls and restrictive memory/nosync summaries. Arbitrary pointer-bit results
are never dereferenced.

All **51 coverage/verification conditions** passed, including valid 32-bit
IR, explicit exclusions and rejection of an invalid-SSA no-op module.
Two input-fixture mistakes were repaired, with failed runs retained:
a naked body illegally used its parameter, and unstable/external-state flags
were placed in AS0, which LLVM 22 forbids. The naked exclusion now uses a
constant base; valid AS1 specimens exercise the nondefault-AS exclusion.
No transformation, supported subset, corpus input or gate was changed to fix
these invalid specimens. See `fixture-correction.json`; the completed matrix
is `focused-native-verified/results.json`.

The unchanged Rust corpus supplies **17 eligible byte GEPs out of 19** in five
functions: state-field accesses through `f2/h5/h6/h7` and text scanning in `h9`.
Two dynamic indices are skipped; seven constant-expression GEPs are outside
the instruction denominator. B3-only, B3+MBA and B3+S2 each transform 17 sites.
After the six-pass prefix, 16 of 62 stage-entry GEPs qualify, with 46 explicit
skips. Earlier-pass machinery is not additional original-source coverage.
Each candidate has five tables, 40 slots and 320 initializer bytes.

All **24 matched profile/seed variants and stripped counterparts** passed
**73,728 native cases and 36,864 state transitions**, comparing full returns
and buffers against the independent oracle. Both roles use the same frozen
shipped compiler and byte-identical prefixes; B3 is applied last, without
perturbing earlier-pass RNG choices. These are same-LLVM22 PIC/shared-link
reconstructions, not a new public Cargo-feature or large-workspace run.

### Native growth and stopping decision

Controller-side native inspection confirms actual materialisation, not only
changed IR: `h5` loads a pointer-indexed read-only table, adds the selector, and
uses the result in displaced stores. Table-byte inventories retain exact native
matches separately. This inspection is **not binary-only semantic recovery**.
The output uses legal RIP-relative base materialisation followed by
register-indexed addressing; the old `[rip+rax]` example remains unencodable.

| Candidate / matched reference | Seed 0 `.text` growth | Seed 1 | Seed 42 | Allowance |
|---|---:|---:|---:|---:|
| B3 / no-pass | 512 B | 512 B | 512 B | 256 B |
| B3+MBA / MBA | 496 B | 512 B | 512 B | 256 B |
| B3+S2 / S2 | 528 B | 512 B | 528 B | 256 B |
| Seven-pass / six-pass | 432 B | 448 B | 464 B | 256 B |

**All twelve text-growth gates failed.** The allowance was
`max(5% of reference .text,256 bytes)`, which is 256 bytes for every pair.
Standalone `.text` grew from 1,212 to 1,724 bytes. Stripped whole-file growth
was zero and passed its separate `max(5%,1024 bytes)` cap because layout/
alignment slack absorbed the additions. Zero file growth does not erase
the code and table cost; code growth alone is not a runtime slowdown measurement.

The mandatory early gate stopped the experiment. No runtime timing matrix,
full-corpus emulator run, binary-only normalizer, fresh LLM trial, IDA run,
scale build, production integration/full-suite rerun or native cross-platform
run followed. Prepared downstream protocols are not results. No resistance,
runtime-performance or scale acceptance is inferred from the algebra/native
checks, and prior experiments' outcomes are not rerun evidence for B3.

Keep `design.json`, `decision.json`, `verification-summary.json`,
`size-gates.json`, `algebra-proof.json`, `prototype/`, `prototype-build.json`,
`tools/`, `corpus/commands.json`, `corpus/comparison.json`, both failed focused
input runs and the completed focused-native results. Production source/test
hashes remain unchanged. A different representation would need a new declared
cost/benefit case, not a size waiver or fewer selected sites to rescue this one.
B4 was assessed next; its separate results follow.

## Byte-alias access experiment (B4)

Evidence: `target/alias-access.KWd8KY/`. **Reject this bounded design.**
All twelve matched native `.text` growth gates failed. Production remains the
six-pass pipeline, with no `--alias-access` flag.

### Supported representation

The standalone LLVM 22 prototype rewrites selected ordinary scalar
i16/i32/i64 loads and stores into seeded permutations of i8 accesses to
**exactly the same storage**. Each original access remains one contiguous
instruction group: no byte operation moves across another original access,
call or fence. Loads reconstruct the integer with zext/shift/OR; stores
extract its bytes with logical shifts/truncation. Lane zero uses the original
pointer; other lanes use non-inbounds byte GEPs.

Pointer definedness is checked with LLVM ValueTracking at the original
instruction, using the original function's dominator/assumption context.
Store values need the same proof before duplication. Loads additionally
require original `!noundef`, carried onto the byte loads. Eligibility is planned
before mutation; later lowering reads updated operands after earlier load RAUW.
Byte alignment is `commonAlignment(original alignment, lane offset)`.

The subset requires explicit little-endian, default-address-space, integral
matching 32/64-bit pointer/index layouts. Atomic and volatile operations,
pointer/float/vector/i8/i128 accesses, unsupported metadata and excluded
EH/coroutine/function shapes stay unchanged. Metadata other than debug and
load `!noundef` is rejected, not blindly copied onto narrower accesses.
AtomicRMW, cmpxchg and memory intrinsics are outside the load/store denominator.

There are no new allocas, globals, constructors, calls, branches, barriers,
pointer/integer conversions, freezes or poison flags. Function/callsite memory
summaries and the shipped MBA policy are unchanged. B4 runs after the exact
shipped-pass prefix. Its recorded standalone SplitMix64/Fisher-Yates order is
an experiment mechanism, not a new production seed/API convention.

### Semantic and native checks

An independent hand-emitted overlapping-pointer pilot passed LLVM refinement
and 1,088 full-return/buffer cases for each of original and transformed code.
At llc O2, both emitted exactly the same `movq`, store, memory-XOR and return
instructions. This was an early feasibility observation, **not** evidence that
the actual corpus would all collapse.

The actual prototype passed **54 coverage/verification conditions** and
**20 native shared-library executions** at original/seed 0/1/42 under direct
llc O0/O2/O3 and clang O0/O3. Its independent oracle made **31,500 calls**,
checking complete buffers and return values. Cases cover widths 16/32/64,
natural and byte alignment, high-bit/signed payloads, derived addresses,
identical and partially overlapping p/q ranges, repeated state changes, and
ordinary accesses around unchanged volatile/atomic operations. Guard pages
check the final valid 2/4/8-byte spans without access into the next page.
Verifier-only 32-bit/layout exclusions are not native platform coverage.

Pinned Alive2 proved **54 actual emitted transformations**: sixteen load,
store, aligned, overlap and derived-address functions at all three seeds,
plus two contextual-pointer cases at each seed. Every check reported one
correct transformation, zero incorrect, zero failed-to-prove and zero errors;
undef/poison checks remained enabled. Full stdout/stderr and commands are in
`focused-proof-results.json` and `contextual-pointer-proofs.json`.
These are scoped LLVM refinements, not exhaustive concurrency or application
correctness. The separate pilot verdict is transcript-derived: its completed
process output expired before archival, so no raw pilot solver log is claimed.

One initial harness expectation was wrong: it equated absent pointer-parameter
`noundef` with a failed contextual proof. LLVM proved two such pointers at
their original memory access, where a non-well-defined address already makes
the source operation undefined under the
[load](https://llvm.org/docs/LangRef.html#load-instruction) and
[store](https://llvm.org/docs/LangRef.html#store-instruction) contracts.
The incidental exact count assertion was removed, not repinned. Structural
exclusions and accounting invariants remained, and six actual-emission Alive2
checks proved those two cases. Prototype, fixtures, subset and gates did not
change. The initial failure and rationale are retained in
`definedness-context-decision.json`.

### Genuine corpus work and emitted form

The unchanged Rust corpus transforms **17 original accesses** in `h5/h6/h7`
on the `f2` state path: eight loads and nine stores become 64 byte loads and
72 byte stores. There are 46 considered load/store instructions without S2,
or 61 with its additional volatile call-table loads. Unannotated input loads,
unproven stored values and unsupported widths/effects remain explicit skips.

After all six passes, 137 stage-entry accesses are considered; B4 transforms
17/18/18 at seeds 0/1/42. Seed 0 includes fourteen original-function accesses
and three earlier-pass helper accesses. The generated decryptor/outline
coverage is not new original Rust protection. Input/prefix hashes and exact
static read/write byte sums match every paired reference.

All **24 profile/seed variants and stripped counterparts** passed
**73,728 native cases and 36,864 state transitions**, including all four
families and independent full-return/buffer oracles. These are frozen
LLVM22/PIC/shared-link reconstructions, not a production Cargo feature.

Native results differ from the pilot: **recombination is partial**. The
standalone seed-0 `h5` still has two wide stores but replaces another with
byte/word/dword writes. `h6` grows from 213 to 476 bytes and retains 32 explicit
byte memory operands where its reference has none. Native-form comparison
preserves register names, arithmetic immediates, access widths and instruction
order, normalizing only control/RIP-relative placement. These are
controller-side inspections, not blinded recovery or proof that stack-based
accesses are spills. See `native-effect-summary.json` and `native-forms/`.

### Growth gate and decision

| Candidate / matched reference | Seed 0 `.text` growth | Seed 1 | Seed 42 | Allowance |
|---|---:|---:|---:|---:|
| B4 / no-pass | 272 B | 288 B | 288 B | 256 B |
| B4+MBA / MBA | 448 B | 304 B | 368 B | 256 B |
| B4+S2 / S2 | 288 B | 288 B | 288 B | 256 B |
| Seven-pass / six-pass | 312 B | 474 B | 425 B | 256 B |

**All twelve text-growth gates failed.** The predeclared
`max(5% of reference .text,256 bytes)` allowance is 256 bytes in every pair.
Stripped file growth was zero, within its separate `max(5%,1024 bytes)` cap;
layout slack is not zero code cost. Surviving byte stores do not by themselves
establish harder data-model or semantic recovery.

No runtime timing matrix, full-corpus emulator run, fresh blinded model trial,
IDA run, scale build, production integration/full-suite rerun or native
cross-platform run followed this mandatory early failure. No performance,
resistance or scale acceptance is claimed, and no volatility, barriers,
additional transformations or gate waiver were used to rescue the design.

Keep `design.json`, `decision.json`, `verification-summary.json`,
`size-gates.json`, the native-form reports, actual-emission proof logs,
`focused-{transform,native,proof}-results.json`, `prototype/`, `tools/`,
`corpus/commands.json`, `corpus/comparison.json`, and the separate
`semantic-pilot/`. Production source/test hashes remain unchanged.
C1 was assessed next; its separate results follow.

## Parameter-aggregation experiment (C1)

Evidence: `target/param-aggregate.Bn48bb/`. **Reject this bounded design as
added obfuscation.** A real memory-carried argument bundle survived and passed
the scoped cost gates, but binary-only unpacking recovered every tested added
leaf relationship. Production retains six passes; no `--param-aggregate` flag.

### ABI representation and complete migration

The standalone LLVM 22 prototype replaces selected internal/private integer
leaves' scalar parameters with one pointer to a typed, caller-owned stack
struct. Fields have a seeded nonidentity permutation. Every static callsite
gets one entry-block alloca, with lifetime start, field stores, `notail` call
and lifetime end at the original call position. Loop iterations reuse storage;
there is no heap allocation, wrapper or additional runtime call.

The subset is explicitly `noinline nounwind memory(none)` leaves with 2–6
`noundef` i8/i16/i32/i64 arguments and an integer result, using C/fast/cold
conventions. Pure integer intrinsics have an explicit whitelist; memory
instructions, allocas, recursion, EH/GC/coroutines, frame/stack observers and
unsupported ABI/metadata shapes are excluded. Budgets are 128 instructions/
16 blocks per callee, eight direct calls per callee, sixteen selected
callees/module and 128 bytes of added bundle payload per caller.

Every use must be a migratable exact direct call. One escaped address, alias,
metadata reference, unsupported caller, operand bundle or `musttail` use
rejects the **whole target**. Uncalled definitions receive no transformation
credit. Original function name/order, body, return attributes and calling
convention survive; the old scalar function is erased only after complete
cutover. There is no scalar compatibility entry or partial ABI migration.

`noundef` and the original possibly-wrapping integer `range` become exact
field-load metadata. Parameter sign/zero-extension ABI attributes disappear
with the removed scalar parameters; return extensions remain. LLVM DataLayout
determines typed offsets/alignment/size. Every field is initialized, but
padding is neither copied nor read.

The new pointer is `noundef nonnull readonly captures(none)` with derived
alignment and dereferenceable size. Both callee **and rebuilt callsites**
become `memory(argmem: read)` and lose `speculatable`. Leaving the callsite
`memory(none)` would incorrectly intersect away the new reads. Caller and
transitive summaries remain unchanged: private, nonescaping alloca accesses
are allowed by the
[LLVM memory contract](https://llvm.org/docs/LangRef.html#function-attributes).
Ordinary tail hints become `notail` to keep the caller frame alive; `musttail`
is never weakened. No new freeze, floating rewrite or MBA policy change occurs.

C1 runs before the existing passes, particularly S2, which subsequently stores
function identities in tables. Existing relative pass order is unchanged.
Standalone SplitMix64 controls field order for this experiment; production
integration would still require the existing seed conventions and new evidence.

### Feasibility and a repaired identity bug

The ABI pilot compared scalar parameters, a by-value struct and a pointer
bundle under direct llc O2 and clang O3. All six artifacts passed 512 cases
each. The by-value callee **and caller had exactly the scalar-control native
bytes** in both pipelines: aggregate syntax alone did not change transport.
The pointer bundle retained a 24-byte caller frame, three stores, a pointer
call and three callee reads. Two separately inlined public-wrapper refinements
passed; these hand-written controls are not actual prototype results.

Before measurement, a metadata-only use exposed a real prototype bug.
`Function::uses()` missed an indirect call's `!callees` reference. Retyping
that function left the indirect caller scalar and changed its metadata to
`distinct !{null}`; LLVM verification still succeeded. The native regression
returned **46** instead of the expected scalar sum, while its migrated direct
caller remained correct.

The final tool rejects `isUsedByMetadata()` targets before selection. Original
and corrected direct/indirect calls agree; the failed binary, valid input,
output and native observations remain in `metadata-native-regression.json`.
This implements the declared complete-use contract, not a dropped caller or
fallback wrapper. A separate LLVM 22 `TypeSize` reporting conversion was fixed,
and output is closed/checked before success coverage is emitted.

### Native semantics and proof normalization

The actual prototype passed **45 transformation/boundary conditions** and
**20 native executions**, covering original plus seeds 0/1/42 under direct
llc O0/O2/O3 and clang O0/O3. The independent oracle made **1,276,040 checks**:
mixed narrow widths, signed/unsigned parameters and returns, high-bit inputs,
wrapping ranges, multiple callers/sites, preserved escaped/alias identities,
whole-target musttail exclusion, repeated loops, complete buffers/guards and
external state. Nonhost layouts are verifier-only.

Eighteen initial wrapper comparisons failed because ordinary analysis-only
inlining erased original scalar-call `noundef` preconditions. For example,
`pa_mixed(x=3,y=poison)` is undefined at its original scalar call; the inlined
reference instead returned poison, while target field-load `!noundef` still
raised UB. Comparing those copies was not an exact source normalization.

LLVM's generic knowledge-retention utility also omitted derived/narrow
operand premises. The corrected analysis helper explicitly retains each
**existing call operand** `noundef`, noundef-backed range, relevant pointer
premise and return contract before inlining actual local callees. It does
not strengthen public parameter attributes, disable undef/poison checks or
alter the measured candidate. Unsupported poison-only contract cases fail
closed rather than acquire invented assumptions.

With those contracts retained, **all eighteen actual-emission public-wrapper
refinements passed**, with zero incorrect, failed-to-prove or error verdicts.
The initial failed copies/results and both helper versions remain alongside
`proof-exact/results.json` and `proof-normalization-diagnosis.json`. These are
scoped, separately inlined wrapper checks, not a direct proof between changed
private signatures, native calling conventions or entire applications.

### Genuine work and measured cost

The unchanged Rust corpus migrates **one real `h0` leaf and all three callers**
in `h2`, `h3` and `f3`: three i64 arguments, one wrapping range, three 24-byte
bundles and 72 bytes of static payload across callers. `h1` remains scalar
because its address escapes through `H2`. Original normalized inputs match;
post-C1/downstream IR and pass counts are not falsely required identical.

Native `h0` gains three explicit memory reads in every candidate. Standalone,
S2 and A1+S2 callee bodies grow from 9 to 13 bytes; six-pass 20/18/45-byte
bodies become 28/26/53 bytes at seeds 0/1/42. Caller stack stores, pointer
transfers and frame instructions are retained separately in
`native-bundle-survival.json`; static payload is not process peak stack use.

All **24 profile/seed variants and stripped counterparts** passed
**73,728 native cases and 36,864 state transitions**. Initialized-ELF emulation
passed **96 conditions, 36,864 cases and 18,432 transitions**. These are
same-LLVM22/PIC/shared-link reconstructions, not a new production Cargo feature.

The 960-sample runtime matrix used CPU 2, five ABBA pairs, 10,000 rounds and
ten ratios/family/profile/seed, with exact independent checksums and no
build/proof/analysis overlap. Median-ratio ranges across seeds:

| Candidate / matched reference | f0 | f1 | f2 control | f3 |
|---|---:|---:|---:|---:|
| Aggregate / no-pass | 1.008–1.043× | 1.049–1.052× | 1.005–1.015× | 1.064–1.067× |
| Aggregate+S2 / S2 | 1.028–1.043× | 0.973–0.976× | 1.003–1.010× | 0.947–0.950× |
| Aggregate+A1+S2 / A1+S2 | 1.005–1.021× | 1.030–1.037× | 0.993–0.998× | 1.095–1.097× |
| Seven-pass / six-pass | 1.016–1.088× | 1.081–1.100× | 0.998–1.056× | 0.936–1.011× |

**All 48 comparisons passed** the 1.15× first-three-pair and 1.10× seven/six
ceilings. The rounded 1.100× cell includes **1.0999304855×** at seed 42:
narrowly below its ceiling, not robust headroom, and not selectively rerun.
All growth gates passed: `.text` added 48–80 bytes and stripped sizes were
unchanged. No exact microarchitectural cause is inferred from these ratios.

### Anonymous bundle normalization and decision

The observer receives only randomized stripped ELF artifacts and public
four-export ABI/domains. Thirty-two deterministic probes/export discover real
call destinations, incoming registers, bounded live memory and return values.
Indirect jumps are counted separately, never automatically treated as calls.
No source, IR, private names, layouts, seeds, profile labels, expected pairings
or oracle enter `bundle_match.py`.

Bounded Capstone lifting handles scalar instructions and fixed-offset loads
from incoming pointers. Lifted expressions must agree with actual returns and
captured entry memory. Candidate field bindings are compared with independently
discovered scalar-register expressions; only **UNSAT** bitvector inequality
queries receive proof credit. A separate controller joins labels/inventories
after the anonymous output is complete.

**All twelve candidate bundle instances matched their true scalar counterpart**,
including S2, A1+S2 and six-pass compositions at every seed. Each matched pair
has two equivalent bindings because multiplication is commutative; exactly one
also matches the sampled original register/field values. All **2,493 observed
invocations per matched pair** have matching field values and results. No solver
query was unknown. The 288 cross-artifact proof bindings are not 288 distinct
source algorithms or independently protected functions.

This proves the recovered scalar leaf expression under the bindings and checks
sampled caller correspondence; it is not universal caller-packing, LLVM
provenance or whole-public-program recovery. Nevertheless, ordinary bundle
unpacking removes the **added** layer on every tested real opportunity.
Passing cost gates does not justify calling that an incremental recovery barrier.

No fresh model trial, scale build, production integration/full-suite rerun or
native cross-platform execution followed the matching failure. Keep the design,
decision, full verification/cost/growth reports, ABI pilot, metadata regression,
proof-normalization history, native bundle evidence, `matching-protocol.json`,
`bundle-matching.json`, `matching-grade.json`, anonymous packet/controller map
and `corpus/commands.json`. Production source/test hashes remain unchanged.
C2 was assessed next; its separate results follow.

## Register-lane calling-convention experiment (C2)

Evidence: `target/custom-cc.jElu2e/`. **Reject this bounded candidate on
runtime cost.** Production remains six opt-in passes, without `--register-cc`.

### Honest target and ABI scope

The experiment combines a seeded permutation of existing scalar parameters
with LLVM 22's **PreserveNone register bank**, not a new convention ID or
arbitrary physical-register/return-register assignment. It inserts no dummy
parameters, thunks, inline-assembly transformation or backend patch.

[Clang's documented contract](https://clang.llvm.org/docs/AttributeReference.html#preserve-none)
preserves only RSP/RBP on x86-64 and makes additional general registers
available for arguments. The ABI is explicitly **unstable across versions**.
This candidate requires the pinned LLVM 22.1.8 x86-64 Linux ELF target and
matching little-endian 64-bit default pointer/index layouts; other targets
are rejected, not silently treated as host-compatible.

The pilot observed scalar inputs in `RDI/RSI/RDX`, preset inputs in
`R12/R13/R14`, and the permuted logical arguments in `R13/R14/R12`, with RAX
still carrying the result. All six direct-llc-O2/clang-O3 pilot artifacts
passed 1,024 value checks each, including values live across the call.
Four analysis-only public-wrapper refinements passed. The simple caller
grew from 2 to 33 bytes and the live-values caller from 60 to 95 bytes:
these are native size observations, not performance measurements.

### Complete scalar ABI cutover

Eligible targets are internal/private `noinline nounwind memory(none)`
integer leaves, with 2–6 `noundef` i8/i16/i32/i64 parameters and an integer
result, originally C/fast/cold. Pure integer intrinsics have a whitelist.
The original return, memory/nosync and applicable function/call attributes
remain. Parameter types, values and attributes—including sign/zero extension
and **independent callee/callsite ranges**—move together to their new slots.

Both definition and every migrated call use `preserve_nonecc`; rewritten calls
are `notail` from the outset rather than relying on cross-convention tail
compatibility. Any `musttail`, escaped pointer, alias, metadata-only use,
unsupported caller/ABI/metadata shape or frame/EH/GC/coroutine observer rejects
the whole target. Uncalled definitions receive no credit. Original name,
module order and body are preserved; old definitions disappear only after
all-use closure. Budgets remain 128 instructions/16 blocks, eight calls per
target and sixteen selected targets/module.

No IR alloca, memory operation, global, constructor, call, barrier, freeze or
floating rewrite is added. **Backend register saves/spills can still grow**;
zero new IR storage is not zero stack cost. Existing pass order is preserved,
with C2 first so S2 sees the new call contract. No S2 production change was
required: final IR confirms all three migrated CC21 calls become indirect
when S2 is requested, alongside its actual coverage report.

### Native register preservation and semantic evidence

The real unchanged corpus migrates `h0` and all three callers, preserving its
wrapping range while leaving escaped `h1` scalar. The actual logical mapping
is observed in native code, not inferred merely from ID 21:

| Seed | Original a | Original b | Original c |
|---|---|---|---|
| 0 | R13 | R14 | R12 |
| 1 | R14 | R12 | R13 |
| 42 | R12 | R14 | R13 |

Different logical permutations do not guarantee different callee bytes:
commutative multiplication can make mappings look identical in the body.
Native instructions, register operands and caller frame/save/spill candidates
are retained in `native-register-survival.json`; register-access lists are
not themselves ABI clobber masks.

**69 transformation/verification conditions passed**, including target,
layout, attribute, metadata-identity, call and resource boundaries.
The initial native fixture was corrupted by evaluation-tool percent/bang
preprocessing (`__omp_magic`/`__omp_shell` appeared as literal IR). Only those
literal instructions were restored; the original invalid input, failed
verifier output and tool incident remain in `fixture-generation-incident.json`.
No prototype, real corpus, semantic expectation or gate was changed.

The focused original/seed-0/1/42 matrix passed **20 native executions** under
llc O0/O2/O3 and clang O0/O3:

- **573,440 scalar-result checks**.
- **573,440 assembly ABI-canary checks**.
- **81,920 full-state checks**, including loops and live values.
- **163,840 excluded-contract checks**.

The tests-only assembly harness saves its own state, assigns distinct
RBX/RBP/R12–R15 sentinels, calls the real public SysV wrapper, and checks those
registers, RSP and the returned value before restoration. An independent
saved-RSP anchor avoids trusting a damaged test frame. Nine sensitivity
controls correctly detect deliberate individual register, stack-pointer and
return-value violations. The harness is non-reentrant, used sequentially;
severe corruption may crash instead of returning a failure mask.

All **24 profile/seed variants and stripped counterparts** passed the standard
**73,728 native cases and 36,864 state transitions**. Additional assembly-canary
validation called the **24 actual stripped corpus artifacts**, including every
S2/A1/six-pass composition, and passed **36,864 cases and 18,432 transitions**
with exact registers, RSP, returns and full buffers. These are native checks,
not the separate initialized-emulator matrix.

Exact-contract analysis-only inlining retained the C1 proof safeguards.
Alive2 proved **21 of 24 wrapper comparisons**, with zero incorrect results;
the three `rc_live` seed variants timed out at the 30-second solver bound.
Separate verified copies reorder only contiguous
`assume(true)["noundef"(V)]` conjunctions, retaining every premise and moving
nothing across another instruction. All three wrappers then have **identical
LLVM instructions/types/attributes, intrinsic declarations and data layout**.

That structural identity is recorded separately, **not counted as three SMT
successes**. Seed 0 still timed out after canonicalization; no timeout extension,
disabled check or repeated identical solver query was used. Keep
`focused-proof-results.json`, `proof-summary.json` and
`proof-canonical/identity-certificate.json`. These scoped semantic checks do
not establish hardware ABI behavior or universal whole-program correctness.

### Cost gate and decision

The 960-sample matrix used CPU 2, five ABBA pairs, 10,000 rounds and ten ratios
per family/profile/seed. Full-result checksums matched independent expectations;
loader/process time was separate and no build/proof/analysis job overlapped
timing. Across-seed median-ratio ranges:

| Candidate / matched reference | f0 | f1 | f2 control | f3 |
|---|---:|---:|---:|---:|
| Register lanes / no-pass | 1.000–1.013× | 0.965–0.986× | 0.996–1.011× | 1.059–1.079× |
| Register lanes+S2 / S2 | 1.015–1.075× | 0.942–0.978× | 0.995–1.005× | 0.938–1.005× |
| Register lanes+A1+S2 / A1+S2 | 1.005–1.031× | 0.994–1.017× | 0.990–0.994× | 1.052–1.061× |
| Seven-pass / six-pass | 1.014–1.048× | 0.983–1.101× | 0.996–1.003× | 0.959–1.147× |

**Two mandatory seven/six comparisons failed** the 1.10× ceiling:
text work at seed 0, **1.1473458789×**, and loop work at seed 1,
**1.1013420400×**. The other 46 comparisons passed their applicable 1.15×
or 1.10× caps. Neither failing row was selectively retried. The near-threshold
loop result and larger text regression remain observations; no exact
microarchitectural cause is claimed.

Every growth gate passed: `.text` added **32–64 bytes**, and stripped file size
was unchanged. Real ABI variation and modest file growth do not waive the
runtime gate or establish stronger recovery resistance.

The early cost failure stopped full-corpus emulation, binary-only recovery,
new model trials, scale builds and production integration/full-suite reruns.
No native non-Linux/non-x86-64 or stable-ABI claim follows. Keep the design,
decision and verification reports, prototype/source seal, ABI pilot, canary
sensitivity/native/corpus results, proof history, native register evidence,
cost/growth reports and `corpus/commands.json`. Production source/test hashes
remain unchanged.
The next candidate was D2; its completed decision follows.

## Constant-reconstruction experiment (D2)

**Decision: reject the defined-carrier modular reconstruction candidate on
runtime cost.** No `--constant-lift` production flag was added. The existing
six-pass pipeline and conservative MBA eligibility remain unchanged.

Local evidence is retained under `target/constant-lift.3xkJGX/`. The matched
corpus uses the same unchanged Rust source, original LLVM IR, inputs, oracle
and frozen shipped tool as C2, without copying old results. LLVM/Clang
**22.1.8**, x86-64 Linux and seeds **0/1/42** are the exercised scope.
The final prototype binary SHA-256 is
`d6a409e2b51b8d60d664aafc1b5ccb7e49f4e32732f58378245441ad95caad44`;
`corpus/prototype-tool.json` seals both source and binary before matched builds.

### Bounded representation and contracts

Only original scalar i32/i64 `add/sub/mul/xor/and/or` instructions with one
wide, nontrivial literal and one nonconstant operand were eligible. Minimum
signed literal width had to exceed half the operand width; zero, one, all
ones, powers of two and their complements were excluded.

For literal `C`, width `w`, explicit same-width `noundef` formal `x`,
seeded odd `A` in 3–255 and nontrivial mask `M`, the replacement was:

```text
R = A*S(x,M) - A*x + (C - A*M)       modulo 2^w
S0 = (x xor M) + ((x and M) << 1)
S1 = (x or M) + (x and M)
S2 = ((x or M) << 1) - (x xor M)
```

Each existing `mba_add` identity gives `S=x+M`, so `R=C`. This is arithmetic
representation, not encryption or runtime secrecy. No new production recipe
family was introduced; a retained design would have shared existing MBA/seed
machinery rather than duplicated it.

The original operand was preferred as carrier only when it was a qualifying
formal; otherwise the first qualifying formal was used. No inferred producer,
new `freeze`, duplicated unknown operand or new input promise was allowed.
The original consumer object, opcode, other operand, flags and metadata stayed
intact. Decoder operations were unflagged, with fixed shift one and no added
memory, call, block, alloca, global, constructor, pointer or ABI effect.
Existing EH/coroutine exclusions and supported integral default layouts applied.
Selection was planned before insertion, capped at **8 uses/function, 64/module**.

D2 ran **last**, after the chosen shipped prefix. All twelve paired prefixes
were byte-identical, preserving earlier-pass RNG decisions. The genuine source
opportunities were `h2`'s add of `3335678366873096957` and `f0`'s xor with
`-6884282663029611473`. Across twelve candidate variants, **33** prefix operand
uses were transformed: **19 exact original consumers** and **14 prior-pass
generated/changed consumers**, all in `h2/f0`. Generated copies were not counted
as additional original Rust opportunities; helpers were not padded.

### Native, algebra and refinement evidence

The final prototype passed:

- **30 transformation conditions plus two fail-closed conditions**, including
  width/layout/function exclusions, preferred/unrelated carriers, both literal
  positions, opcode coverage, flags/metadata, budgets, invalid input and failed
  output writes. Exact emitted decoder DAGs and constant-only mutations were
  independently checked. All three recipes were observed at both widths.
- **20 native executables** across direct `llc -O0/-O2/-O3` and
  `clang -O0/-O3`, original plus three seeds: **6,438,000 exact full-value/buffer
  checks**. Edge/random carriers, flag-safe source domains, unknown-other-
  operand concrete inputs, excluded widths, sentinels and state transitions
  were exercised.
- **48 corpus executions**, original and stripped forms of all 24 variants:
  **73,728 exact cases and 36,864 state transitions**.
- **Eight Z3 4.15.4 algebra queries**, all UNSAT: three finite-bitvector sum
  identities at each width plus modular ring cancellation at each width.
  These are algebra results, not complete LLVM or native-backend proofs.

The **13 seed0 actual-emission boundary comparisons** produced **six Alive2
proofs and seven timeouts**, with undef/poison checks enabled and no changed
signatures or semantic contracts. Timeouts were `xor64l`, `unrelated32`,
`flags32`, `flags64`, `state32`, `state64` and `multiple`. No counterexample
was reported, but **full LLVM refinement is not established**.

The initial `flags32` attempt stopped on unsupported custom `d2.keep`
metadata. Continuation removed only the three nonsemantic attachments from
each analysis module; actual emitter preservation was checked separately.
Consumer flags, attributes, memory and input promises were unchanged.
All five continuation comparisons timed out; normalization is not an SMT
success. The remaining prepared 123-comparison matrix was not completed.

Two implementation/fixture corrections are retained, not hidden as passes:

- Initial width32 mask construction asserted because LLVM22's `APInt`
  constructor requires explicit `implicitTrunc=true`. The one-line fix
  implements the already specified truncation without changing RNG draws.
  `focused-transform-results.json` retains the failed-before attempt.
- Before execution, a naked-function exclusion fixture was repaired to avoid
  an LLVM-invalid formal-argument reference. Its nonconstant operand now comes
  from register-output inline asm; exclusion counts and gates did not change.

Hand-generated pilot checks are separate: 63,840 native comparisons passed,
but only three of six Alive2 checks proved; the other three timed out.
The pilot's variant1 folded to byte-identical reference bodies under
`clang -O3` at both widths. Other pilot variants retained work. Actual corpus
`llc -O2` disassembly contains decoder arithmetic; partial pilot folding is
not a claim that every actual reconstruction disappeared.

### Cost gate and decision

Every growth gate passed: **80–256 added `.text` bytes**, against 256-byte
ceilings, and **zero stripped-file growth**. Padding slack is not free code.
The native matrix used CPU2, five ABBA pairs, 10,000 rounds and all four
families, producing **960 samples and 48 comparisons**. Proof/build/recovery
work did not overlap timing. Full-result checksums were exact.

| Comparison | Seed | Family | Median ratio | Cap |
|---|---:|---|---:|---:|
| Seven passes / six passes | 0 | `f0` branching | 1.121538720548793 | 1.10 |
| D2 + MBA / MBA | 1 | `f0` branching | 1.2050537822517602 | 1.15 |
| Seven passes / six passes | 1 | `f0` branching | 1.1765501655525723 | 1.10 |
| Seven passes / six passes | 42 | `f3` string processing | 1.1425582593030272 | 1.10 |

The remaining **44 comparisons passed**. `f3` is a mandatory unchanged-IR
control, not a decoder-execution result; its failure remains in the decision.
No microarchitectural/noise cause was isolated, no selective retry was made,
and no ceiling was relaxed. Three failures also affect transformed branching
work. Correct native outputs and surviving arithmetic do not justify the cost.

This early rejection stopped full-corpus emulation, binary-only constant
recovery, new model trials, real-workspace scale, integration and production
full-suite reruns. No recovery or cross-platform claim follows, and solver
timeouts are not resistance. All **21 snapshotted source/test/Cargo files**
remain unchanged.

Keep `design.json`, `decision.json`, `verification-summary.json`,
`proof-summary.json`, initial failure records, algebra/pilot evidence,
`native-inspection.json`, `cost-gates.json`, `size-gates.json`, source/tool
seals and full corpus reports. `commands.json`, `run_focused.py`,
`continue_proof.py`, `run_corpus.py` and `corpus/commands.json` retain execution
recipes; successful and failed outputs are separate and must not be overwritten.
The next evaluated candidate was D3; its feasibility decision follows.

## Quadratic-residue predicate experiment (D3)

**Decision: reject the quadratic-nonresidue family at the predeclared
native-probe simplification gate.** No automatic S1 rewriter, new predicate
flag, cache or BCF pass was built. Production remains the six shipped passes.
This is a component-feasibility result, not full-program or S1 CFG recovery.

Evidence root: `target/opaque-residue.BVKdDB/`. `design.json` declared the
staged experiment before emission or execution. `derive_decision.py` derives
the rejection from retained LLVM/native/analysis records, rather than counting
extra instructions as protection. The exercised scope is **LLVM/Clang 22.1.8,
x86-64 Linux, seeds 0/1/42**.

### Candidate and S1 boundary

For defined same-width i32/i64 carriers, the proposed family was:

```text
Q = x*x - D*y*y                     modulo 2^w
D in {2,10,18,26,34,42,50,58}
C mod 8 in {3,5}
E' = E AND (Q != C)                 or E OR (Q == C)
```

Squares modulo eight are 0, 1 or 4. With `D mod 8 = 2`, `Q mod 8` belongs to
`{0,1,2,4,6,7}`, so equality to `C` is impossible. Reduction modulo `2^w`
preserves the low three bits at both supported widths. This does not rely on
unbounded signed arithmetic or the false historical `C=1` predicate.

Both carriers were direct, distinct, same-width `noundef` formals in source
and target. The original `E` was consumed once; no freeze, inferred producer,
new input promise or poison-generating arithmetic flag was allowed. New work
was pure modular arithmetic, not a load, call, table, block or ABI change.

The intended consumer was an existing S1 unconditional biased-index select.
Its volatile key load, equality comparison, offsets, table contents, real
destination list and PHIs would have remained intact. This was neither entry
caching nor volatile-target dispatch. The automatic recognizer and matched
S1/S1+MBA/S1+S2/six-pass corpus were explicitly conditional on surviving the
initial native simplification gate; **they were not implemented or run**.

### Actual emitted probes and correctness

Twenty positive recipes covered both widths, both boolean composition modes,
seeded coefficients/full-width constants/carrier ordering, and fixed
`D=34,C=3/5` boundaries. Each emitted a predicate function and a biased-index
selector function: **40 positive exports**. The selector's independent i32
flag deliberately lacked `noundef`, and represented arbitrary original `E`
through comparison with zero. Expected indices were 29/11; they were not
derived by re-evaluating the candidate arithmetic in the positive native oracle.

Four additional nonconstant sensitivity exports used the known invalid `C=1`
case at both widths. Their source and target intentionally kept that
nonconstant computation. They test false constant/selector classification,
not a newly discovered production bug.

Observed checks:

- Both generated LLVM modules passed verification.
- **40/40 direct actual-emission Alive2 refinements passed**, with undef/poison
  enabled, unchanged signatures and no analysis normalization. Zero incorrect,
  failed-to-prove or tool-error results.
- **Eight compositional bitvector obligations were UNSAT** in Z3 4.15.4:
  equality projection, modular multiplication/subtraction projection, the
  coefficient-family constraint and nonresidue exclusion at both widths.
  Symbolic high bits stayed unconstrained. Six expected SAT sensitivity
  witnesses were recorded separately, not counted as equivalence proofs.
- Direct `llc -O0/-O2/-O3` and `clang -O0/-O3` produced **ten reference/target
  libraries**, plus five stripped target copies. Fifteen native validations
  passed **625,680 full-return comparisons** over low-bit pairs, width
  boundaries, high bits, random words and zero/nonzero flag values. Every
  sensitivity export exposed both outcomes.

None of the **200 positive function/backend pairs** had reference-identical
machine bytes. Every positive target body retained native multiplication.
This demonstrated surviving arithmetic, not incremental analysis difficulty.
Five backend configurations produced four distinct stripped binaries:
direct `llc -O2/-O3` outputs were identical. The instance counts are not counts
of distinct algorithms or independent resistance trials.

### Binary-only simplification gate

`analyze_native.py` received only stripped `observer/n00.so`–`n04.so` and the
public numeric-export scalar ABI. It read no source, LLVM IR, coefficients,
expected outcomes, controller roles or reference/pair labels.
Observed output values supplied candidate constants and selector arms.

The analyzer lifted actual scalar instructions, comparisons, condition flags,
conditional moves and bounded acyclic paths. Entry registers, unused argument
high bits, flags and uninitialized stack bytes remained unconstrained in the
model. Unsupported behavior failed closed. No hand-selected low-bit theorem
or source-formula rewrite was supplied to the native solver.

All **74,910 observations** agreed between real native calls, Unicorn and the
lifted expression. The completed classification was:

| Probe/backend instances | Result |
|---|---|
| 100 positive predicates | Constant outcome proved from native code |
| 100 positive selectors | Original flag-zero test and both output values recovered |
| 20 nonconstant controls | Both outcomes observed; no constant/simple-selector identity proved |

The **1,500 solver queries** returned **420 UNSAT and 1,080 SAT**, with zero
UNKNOWN results or tool gaps. UNSAT comprises **200 semantic normalization
proofs plus 220 return-path coverage proofs**, not 420 distinct recovered
algorithms. SAT records include rejected hypotheses and distinct-return
reachability at validated observations.

This removes the tested added component under the declared binary-only
analysis model. It is not whole-program recovery, a blinded model score or a
general claim about all opaque predicates. Local tool time includes sampling
and controller work and is not an attack-effort or target-runtime metric.

### Decision and retained evidence

The early gate failed despite correct surviving arithmetic. No automatic pass,
full-corpus size/runtime comparison, full-program recovery, new model trial,
real-workspace scale or production integration/full-suite rerun followed.
The experiment therefore makes **no product cost or scale acceptance claim**.
All **21 snapshotted source/test/Cargo files** remain unchanged.

Keep `design.json`, `scope.json`, generated IR/controller records,
`build-results.json`, `native-summary.json`, `native-arithmetic-survival.json`,
`proof-results.json`, `algebra-proof.json`, the public observer packet and its
input seal, `analysis-results.json`, `analysis-summary.json`,
`verification-summary.json` and `decision.json`. `commands.json` records
execution recipes; `capture_evidence.py` reproduced the retained metadata
exactly, and `derive_decision.py` checked the gate and production identities.
Fresh reruns must not overwrite this evidence.

The next evaluated candidate was the bounded D1 scheduler below.

## Bounded pipeline-order experiment (D1)

**Decision: do not promote the shipped/late-shuffle scheduler.** Its scoped
correctness and cost gates passed, but incremental recovery benefit was not
established. This is not a correctness failure or a rejection of every possible
dependency-constrained order. Production keeps its fixed six-pass order.

Evidence root: `target/pipeline-order.8XtfeM/`. `design.json` and
`dependencies.json` predeclared the candidate and constraints. Rust **1.98.1**,
LLVM/Clang **22.1.8**, x86-64 Linux and seeds **0/1/42** were exercised.
Only the isolated copy's `src/main.rs` changed; pass implementations, seed
helper, native shim, dependencies and tests remained byte-identical.
The candidate executable SHA-256 is
`b4a30d8cf2fd0a6f4de4a19f5c5f7f4b149d217965aef4b2f6509c874f8ae9ca`.

### Two orders, not arbitrary permutations

```text
shipped:     string → A1 → A2 → S1 → S2 → MBA
late-shuffle:string → A1 → S1 → S2 → MBA → A2
```

String generation stayed ahead of consumers of its helpers. A1 stayed ahead
of S1 because its eligibility excludes address-taken blocks and `indirectbr`;
MBA-first could also inflate its original-region instruction threshold.
Holding A1 before shuffle preserved its selected original region. S1/S2/MBA
relative order was retained as a scoped convention, not asserted as a universal
semantic dependency.

The isolated driver supported forced endpoints and a seeded choice using one
boolean draw from `seed::for_pass("pipeline_order", module, source)`.
It did not alter the master seed or consume another pass's stream.
Every enabled pass ran once on one parsed LLVM module, with input/stage
verification and optional fresh snapshots—no pass-by-pass reparse.
The experimental environment controls were not added to the production CLI.

Both endpoints were mandatory acceptance cases; a selected shipped order
could not excuse a failing late endpoint. The same seed did not imply the
same choice for different module identities: the focused fixture selected
shipped/shipped/late for 0/1/42, while the unchanged corpus selected
shipped/late/shipped. Both choices were observed without expanding the
predeclared seed range.

### Correctness and genuine native effects

Observed verification:

- **12 existing Cargo tests** passed in the isolated crate.
- All **192 seed/flag conditions**—64 subsets at three seeds—passed frozen
  reference identity, forced endpoint and seeded-resolution checks.
  There were **781 successful transform invocations**, including deterministic
  repeats, default/no-shuffle/duplicate-flag and trace-off controls.
  Nine invalid-policy/seed/input/output/retained-trace requests failed closed.
- **125 native executions**, across direct `llc -O0/-O2/-O3` and
  `clang -O0/-O3`, checked 21,000 repeated integer/state transitions,
  420,000 buffer/sentinel words and 7,000 narrow-ABI/excluded-function rows,
  plus strings and constructor ordering. The **12,000 floating comparisons**
  were bitwise comparisons to the original IR's native outputs, not an
  independent floating-point proof.
- All **24 corpus roles**, original and stripped: **73,728 exact cases and
  36,864 state transitions**. All **96 emulator conditions** additionally
  passed **36,864 cases and 18,432 transitions**.

The common input/string/A1 prefix was byte-identical. S1/S2/A1/A2 coverage
remained matched; MBA round decisions and generated-operation totals were
retained rather than forced to incidental expected counts.

Controller-only canonical copies established that A2 changed only block-list
order. They preserved instructions, operands, PHIs, globals, attributes,
metadata and source identity; only block order and ModuleID were normalized.
The helper's sensitivity smoke detected changed arithmetic, metadata and
source identity, rejected unsupported anonymous unreachable ordering, and
passed 24 native checks. Canonical copies never entered measured builds.

**Final IR block order matched in all twelve endpoint pairs.** Other native
differences came from existing dispatch/MBA choices and placement, not a new
algorithm or additional CFG layer. Symbol-corresponded native comparison
preserved arithmetic immediates, registers and instruction order while
normalizing control/RIP placement only. It is inspection, not anonymous
semantic recovery. Shuffle+MBA at seeds 0 and 1 produced identical whole and
stripped ELF files; they cannot be credited as additional variation.

### Cost and matched recovery

All **48 runtime comparisons** passed over **960 CPU2 ABBA samples**, five
pairs and 10,000 rounds, with exact full-result checksums and no overlapping
build/proof/analysis work. Caps were 1.15× for three reduced profiles and
1.10× for all six passes. The closest median was six-pass loop work at seed1:
**1.0970054215436122×**. `.text` changes ranged from **−112 to +64 bytes**;
stripped growth was zero. Every paired growth check passed.

A fresh recovery batch selected **seed1**, the first fixed matrix seed that
actually selected late shuffle for this module. It covered both endpoints,
all four profiles and all four families: **32 anonymous conditions**, one
stateless trial each, twelve decisions and 64 chosen inputs per condition.
No recovery outcome informed selection.

All 32 numeric-entry IDA conditions were available. Fresh observer-copy
readiness checked 12,288 additional native/emulator/oracle cases and 6,144
transitions; these overlap corpus conditions and are not extra independent
workloads. Source, order/profile labels, coefficients, oracle, hidden sets and
cross-condition history stayed outside analyst prompts.

| Profile | Fixed-order exact recovery | Late-shuffle exact recovery |
|---|---:|---:|
| A2 + MBA | 4/4 | 4/4 |
| A2 + S1 | 2/3 completed; one unavailable | 4/4 |
| A2 + S1 + S2 + MBA | 1/4 | 2/4 |
| Six passes | 3/4 | 1/4 |

Exact recovery required every remaining held-out return and full buffer to
match, plus all stateful transitions where applicable. There were 256
held-out cases per submitted condition and 32 stateful sequences where
applicable; query-overlapping inputs/sequences would have been excluded.

The aggregate was **11/16 candidate conditions** versus **10/15 completed
reference conditions**. One reference loop trial lost its model connection.
Of **307 attempts and 306 responses**, 99 responses violated the format/size
contract. Eighteen tool requests failed: eleven exceeded the output bound
and seven requested unmapped addresses. These are not proof of unavailable
analysis capability or obfuscation resistance. No selective trial retry or
budget relaxation followed.

The documented completion interface exposes only a route alias, not exact
model identity, settings or usage. Together with one trial per condition and
protocol errors, this prevents treating the six-pass difference as reliable
hardening evidence. The result is **unestablished incremental benefit**, not
a success claim or an invented all-recovered outcome.

### Schema-parsed recovery protocol revision

Evidence root: **`target/recovery-schema.3pbKNv/`**. This is a separate,
predeclared full **32-condition** trial, not selective replacement of failed
historical conditions. It reused the same seed1 endpoints, four profiles,
four families, binaries, oracle, hidden inputs and IDA reports. Native tools
and the bounded interpreter were byte-identical; readiness reuse checked
their hashes and the retained input/artifact identities. No analyst histories
or scores were copied.

The documented `completion(schema=...)` interface returned parsed action
objects. The controller validated their fields, types and bounds before
dispatch. Emulation operands `a` and `b` traveled as canonical decimal strings,
then became exact Python integers; JSON-number rounding could not narrow the
u64 input domain. Limits remained **12 external decisions, 64 accepted cases,
8 cases per emulation request**, with unchanged native execution, interpreter,
prompt and tool-output bounds. The 24,000-character response bound now applies
to canonical JSON of the **parsed object**, not unavailable raw provider text.
Every invalid action would consume a decision; transport failures terminate
their condition without a selective retry.

Transport validation rejected **24 invalid actions** and compared native,
Unicorn and DSL execution on four independent XOR cases. A real schema API
response then preserved **2^64−1 and 2^53+1** and passed two further
native/emulator/DSL comparisons. Isolated fault-injection checks exercised
accepted-query accounting across a tool timeout/reload, the 64-case and
12-decision boundaries, and terminal launch/collection failures without
replay. Those checks are not analyst trial results.

| Profile | Fixed-order exact recovery | Late-shuffle exact recovery |
|---|---:|---:|
| A2 + MBA | 4/4 | 4/4 |
| A2 + S1 | 4/4 | 4/4 |
| A2 + S1 + S2 + MBA | 3/4 | 3/4 |
| Six passes | 4/4 | 4/4 |

All **32 conditions submitted and were scored** after **272 external API
calls and 272 parsed responses**. No API failure, invalid parsed action or
controller error was observed. There were **240 tool requests**, including
**22 failures**: nine exceeded the output bound and thirteen requested
unmapped memory. All 152 accepted queries counted toward their conditions'
budgets, including requests whose output failed. These failures are not
resistance or proof that the underlying analysis capability was unavailable.

Scoring retained **256 held-out cases per condition** and **32 sequences /
768 transitions per stateful condition**; no query overlap required exclusion.
The only nonexact submissions were the late-shuffle string-processing case
at **227/256**, and the fixed-order stateful case at **236/256 and 0/32
sequences**, both in A2+S1+S2+MBA. Aggregate recovery was **15/16 at each
endpoint**, including all six-pass conditions. Finite exact recovery is not an
equivalence proof; the two nonexact submissions do not establish hardening.

Exact model identity, settings, token usage, raw provider output and internal
parsing/repair attempts remain unavailable. Zero invalid **parsed actions**
does not demonstrate zero raw-format errors. Do not pool this trial with the
historical raw-text trial or attribute score differences solely to transport.
The scheduler remains **not promoted: incremental benefit unestablished**.

`recovery/trials/` retains every prompt and parsed response. `audit.py`
reconciles histories, remaining budgets, exact-u64 queries, CPU input echoes
and held-out exclusions without rescoring or repairing submissions.
`recovery/trial-audit.json`, `readiness-reuse.json`, `transport-checks.json`,
`session-checks.json`, `verification-summary.json` and `decision.json` retain
the resulting evidence. All **31 previously sealed D1 evidence/decision
artifacts** and **21 production source/test/Cargo files** matched their saved
hashes. No new candidate runtime, scale or production full-suite run is claimed.

### Disposition and evidence

The candidate was not promoted and did not proceed to real-workspace scale.
All **21 snapshotted production source/test/Cargo files** remain unchanged.
No order-wide LLVM equivalence proof, native non-Linux verification or
production full-suite rerun is claimed.

Keep the design/dependency record, isolated source/tool seal, complete focused
command logs, canonical sensitivity results, corpus stages/coverage/native
forms, `cost-gates.json`, emulator/readiness results and every raw recovery
exchange. `recovery-summary.json` separates errors from scores;
`summarize.py` derives `verification-summary.json` and `decision.json`.
Do not overwrite completed trials or count their errors as protection.
The function-layout candidate was evaluated next; its decision follows.

## Function-layout experiment (Y2)

**Decision: reject the seeded final definition permutation as this bounded
obfuscation candidate.** Actual native placement changed, but every tested
function body remained anonymously matchable. No layout flag was added.
This says nothing about profile-guided locality optimization or every possible
layout scheme.

Evidence root: `target/function-layout.SJYalF/`. The prototype ran last after
the **frozen shipped pipeline**, not the unpromoted D1 scheduler. Its final
binary SHA-256 is
`be2a61e9b54ebe481f83b4aaa80282068d77cdbac4a56fe34fc3e2dc63a6efd3`.
LLVM/Clang **22.1.8**, x86-64 Linux, default GNU bfd linking and seeds
**0/1/42** are the exercised scope.

### Layout-only contract

SplitMix64/Fisher–Yates permuted existing function-definition objects among
their original definition slots; declarations retained their full-list slots.
No function was cloned, renamed or deleted. Bodies, blocks, operands, calling
conventions, attributes, metadata, globals, aliases, blockaddresses and
constructor/destructor arrays stayed intact. No padding, new section,
`noinline` attribute or runtime transform forced native survival.

Whole-module guards covered missing/unsupported target/layout/address spaces,
module/inline assembly, ifuncs, unnamed functions, explicit function sections,
naked/presplit functions, prefix/prologue data and known adjacency contracts.
Nonempty data layouts used LLVM's documented default pointer width when p0
was omitted, as existing S2 does—not a host-width fallback.
Logical function-pointer identity was preserved; absolute numeric addresses
and relative placement intentionally were not. Arbitrary fixed-code-address,
custom linker-script or section-adjacency assumptions are outside the contract.

Analysis-only canonical function-name ordering verified complete module
identity apart from function-list order and ModuleID. Those copies never
entered measured builds. Separate LLVMContexts avoided identified-structure
name collisions during serialization checks. Default-layout interpretation
and a named-type fixture were corrected during pre-execution review, not in
response to a failed measured gate.

### Native verification and realized placement

- **108 boundary/coverage conditions** passed, with complete guarded/no-op/
  moved accounting, exact seeded repeats, declaration-slot preservation,
  canonical positive/negative controls and parse/verifier/output failures.
- **20 focused native executions** at direct `llc -O0/-O2/-O3` and
  `clang -O0/-O3` passed **7,680 stateful full-buffer calls**, runtime
  pointer/alias identity, blockaddress dispatch, constructor/destructor order,
  initialized data and loop/PHI checks. Personality/invoke metadata stayed
  intact and no-throw paths ran; **executed unwinding was not tested**.
- Four profiles—none, S2, A1+S2 and all six passes—at three seeds produced
  **24 native roles**. Original and stripped validations passed **73,728
  exact cases and 36,864 state transitions**.

The final linked definition order exactly matched each requested permutation
in **all twelve pairs**. **183 definition slots moved**; **188 native entry
addresses changed**, because even a fixed slot can move when preceding
functions have different lengths. All growth checks passed: `.text` changed
by **−8 to +10 bytes** and stripped size did not grow.

Raw bodies are not all byte-identical. In the no-pass profile at seeds0/42,
`h2`'s tail jump widened from two to five bytes after its target moved farther
away. Other size changes included interfunction alignment. Such address/
encoding changes do not imply different arithmetic or a hidden prologue.

### Anonymous body matching

The observer received 24 stripped numeric-name ELF files and public ABI
information only—no source, IR, private symbols, profile/seed labels, expected
bindings or pairings. It discovered **384 extents** from public symbols,
unwind/FDE metadata and bounded real control flow, then examined all **276**
unordered artifact pairs.

Fingerprints preserved mnemonics, registers, widths, ordinary immediates,
instruction order and direct/address graph relationships. Only evidenced
placement was normalized. Referenced data and relocation records remained
explicit; a uniquely represented code pointer plus an observed immediate
could describe an additively biased relocation. Arbitrary numeric operands
were not erased.

After observer output froze, controller-only function truth validated:

- **192/192 true-pair function bodies correctly matched**, including all 188
  moved entry addresses.
- Zero wrong, ambiguous, unmatched or unsupported true-pair body matches.
- An independent sensitivity packet passed 80 native checks, matched all five
  layout-only functions, and rejected changed arithmetic, referenced data and
  an ordinary address-shaped constant. Biased-pointer target relationships
  were also checked.

FDEs are not a universal function oracle. The retained report records its
extent claims, indirect-target limitations and unclaimed executable gaps.
This is exact placement-normalized body correspondence on these artifacts,
**not whole-program/data semantic equivalence or a blinded model score**.

A controller-map serialization bug initially overwrote anonymous filenames
with source artifact paths. The parallel public/map append order and every
stripped SHA-256 reconstructed the binding unambiguously. Original and
corrected maps remain; no binary, public packet or observer output changed,
and analysis was not rerun.

### Disposition

The predeclared early matching gate failed, so runtime/locality benchmarks,
new model trials, real-workspace scale and production integration/full-suite
reruns were not run. Small size changes are not a performance result.
All **21 snapshotted production source/test/Cargo files** remain unchanged.

Keep `design.json`, `preflight.json`, source/tool seal, focused/corpus results,
`matching.json`, `matching-validation.json`, encoding-size evidence,
sensitivity results and both controller maps. `validate_matching.py` reproduces
the truth join without touching observer inputs; `summarize.py` derives the
verification summary and decision. The bounded S3 candidate was evaluated next.

## Bounded register-VM experiment (S3)

**Decision: reject this automatic pure-leaf, late-placement register VM on
composed native text growth.** It is a complete executable prototype for the
declared subset, not a production VM API or a general virtualizer. This does
not reject every VM architecture or a separately specified cold-code use case.

Evidence root: `target/scalar-vm.WKIfnb/`. The frozen shipped pipeline was the
base; none of the unpromoted D1/Y2/D2/D3 candidates was included. LLVM/Clang
**22.1.8**, x86-64 Linux and seeds **0/1/42** were exercised.
The corrected prototype SHA-256 is
`2e4a582a52633eb09501c4b8ea00ba4d19e6cf3a159eb9593e7c8ad27f60d6ec`.

### Executable subset and representation

Selection required exact internal/private C/Fast/Cold functions with one
basic block, one to six explicit `noundef` same-width i32/i64 arguments,
matching return width and two to 64 non-return instructions. At most four
functions per module and 128 registers per function were planned before any
helper generation.

The implemented subset included unflagged word arithmetic/bitwise operations,
constant in-range shifts, all ten integer comparisons, word selects, boolean
and/or/xor and explicit word/boolean conversions. Unsupported widths,
undefined/poison origins, poison-generating flags, source memory, atomics,
volatile operations, calls/intrinsics, branches/PHIs/loops, exceptions and
special ABI/contracts were accounted exclusions—not mocked operations or
runtime fallbacks.

Immutable private programs contained 16-byte control/payload records.
Typed per-width interpreters used seeded dense opcode IDs and emitted **only
the handlers actually used**. Each handler loaded only its consumed registers;
arguments initialized their slots, and validated SSA ordering initialized all
other consumed slots before use. There was no heap, mutable VM global state,
unnecessary frame zero-fill, new volatile access or runtime fuel cutoff.
The interpreter's dispatch loop was real; it was not a reconstructed native
expression presented as a VM.

Original Function objects, names, linkage, calling conventions, parameter/
return attributes and callers remained. Their selected bodies became exact-
size local register frames, argument stores, one typed interpreter call and
return. Existing S2 calls still targeted those same original functions.
The interpreter was private, shared, `noinline` and `nounwind`.

The genuine corpus target was **`h0` computing `b*a+c`**, with three existing
callers. It is hot in the branching/loop families and was not relabeled cold.
Programs occupied **48–208 bytes** and local register arrays **40–120 bytes**;
these are logical frame allocation sizes, not claims about complete native
stack-frame size.

### Correctness and actual VM survival

- **70 private/internal leaves across 18 modules** exercised all **28 handler
  kinds at both widths**, plus 48 negative modules and budget/contract cases.
  The checker compared actual emitted global words, operand/register ordering,
  definitions-before-use, terminal returns and used-handler unions with the
  original LLVM. That structural check was not treated as native proof.
- **20 native executables**—original plus three seeds at direct
  `llc -O0/-O2/-O3` and `clang -O0/-O3`—passed **19,353,600 full-return and
  seven-word-buffer calls** and **1,400 function/alias identity checks**.
  Independent unsigned C semantics covered signed ordering/sign fill,
  unequal operands, high bits, wraparound and constrained range contracts.
- **Nine actual-engine Alive2 comparisons passed**: 32-bit subtraction,
  64-bit arithmetic shift and a range-constrained leaf at three seeds.
  Analysis copies recursively inlined the real wrappers/interpreters using
  the existing exact call-contract preservation utility. Public promises and
  undef/poison checks stayed intact. Target unrolling was bounded by actual
  record count plus two; this is not a proof of arbitrary source loops or
  every VM/ABI behavior.
- All **24 corpus roles**, original and stripped, passed **73,728 exact cases
  and 36,864 state transitions**. Original caller bodies, ABI contracts,
  globals, aliases and constructor data stayed unchanged outside the declared
  selected bodies and generated VM artifacts.

Twelve concrete initialized-ELF/Unicorn witnesses independently matched the
source oracle and observed every emitted control record read in order.
This established actual bytecode/dispatch execution in the native candidates,
not anonymous recovery or performance. Private LLVM helpers need not appear
in static symbol tables; their absence is not evidence that the VM disappeared.

### Growth gate

| Matched profile | Added `.text` bytes across seeds | Allowed |
|---|---:|---:|
| No passes + VM / no passes | 142 | 256 |
| S2 + VM / S2 | 153 | 256 |
| MBA + VM / MBA | 407–519 | 256 |
| Six passes + VM / six passes | 404–528 | 256 |

**Six of twelve text-growth comparisons failed**. All stripped-size changes
were zero, reflecting layout slack rather than zero code/bytecode cost.
The shared interpreter was already limited to used handlers; no extra source
functions were invented to amortize its cost.

The declared early failure stopped hot runtime/locality benchmarks, cold-code
amortization claims, anonymous bytecode recovery, new model trials, scale and
production integration. The VM was not moved before MBA, the MBA composition
was not dropped, and caps were not relaxed to rescue the measured design.
The repository supplies no real cold/sensitive selection policy or frequency
model; adding unrelated work or claiming `h0` was cold would not establish
application acceptance.

### Retained implementation and controller corrections

The first generated input had an internal alias/public-wrapper name collision
and failed before transformation. Internal aliases now have distinct names.
Coverage keys also initially referenced a destroyed local StringMap; owning
the key strings fixed corrupted reason output. Both failures remain archived.

The intended Rust leaf carried `"probe-stack"="inline-asm"`, which the initial
attribute filter had not modeled. That exact policy is now preserved on the
original function and propagated to its shared interpreter; custom probe
symbols and custom probe sizes remain excluded. Positive focused leaves carry
the ordinary Rust policy. No source promise or protection was stripped.
See LLVM's [stack-probe attribute contract](https://llvm.org/docs/LangRef.html#function-attributes).
A verifier-only naked fixture was also repaired before execution to avoid
forbidden formal-argument references.

Controller projection initially confused attribute-group renumbering, then
predecessor-comment ordering, with caller changes. Inspection now expands
attributes and uses verified parse/print copies; it drops no semantic tokens,
and those copies never enter native builds. The first nine completed corpus
pairs were retained; only the three unexecuted six-pass pairs continued in a
fresh report. This was not a selective timing or model retry.

`initial-attempt/`, `integration-corrections.json`, `projection-correction.json`
and `corpus-continuation.json` preserve that history. `commands.json` records
the corrected execution recipe. Both retained-continuation and clean-full-
matrix evidence paths were exercised without compiler/proof/model/timing
repeats. `summarize.py` derives the decision and verification summary.
All **21 snapshotted production source/test/Cargo files** remain unchanged.

### Status after the inventory

Every listed technique now has an implementation or a bounded experiment
decision; this does **not** establish product acceptance. Production still has
six opt-in passes and the fixed order. There is no VM flag or
`#[lyra::vm_protect]` API.

Further work needs a new evidence-backed benefit case rather than automatic
promotion: reproducibly identified model/protocol evaluation, realistic
selection/workload contracts if revisiting VM, and the relevant native platform,
cost and real-workspace gates for any retained change. Do not interpret this
bounded VM rejection as a claim that all virtualization is infeasible.

## Ordered implementation plan

### 1. Floating-point MBA — completed on Linux

All nine unsound variants and their callers were removed. The retained Lyra-path
reproduction and native regression failed before and passed after the fix; see
[the results above](#resolved-floating-point-blocker). Relevant individual and
combined Linux runtime checks pass. Floating-point instructions are now outside
MBA's transformation subset. Do not restore real-number rewrites without an
explicit supported semantic contract and proof.

Native macOS and Windows confirmation is still required before claiming the
same runtime coverage on those platforms.

### 2. Integer MBA — scoped audit completed on Linux

The undef regression is fixed by conservative eligibility, with permanent native
tests and retained refinement/lowering evidence. See
[the supported subset and gaps](#integer-mba-eligibility-audit).
Do not reintroduce freeze-based expansion without resolving the retained
LLVM 22.1.8 backend failure. Broader eligibility requires both LLVM refinement
and native codegen checks; proof timeouts and untested constructs remain gaps.

### 3. Existing-pass measurement — initial Linux baselines completed

The [measured baseline above](#measured-existing-pass-baseline) records repeated
timings, size/coverage, seed repeatability, retained artifacts and scoped IDA
observations. The harness is reusable; the observations are not universal
effectiveness or cross-platform claims.
The [real-workspace baseline](#real-workspace-scale-baseline) adds selected
multi-crate scale, resource, archive-replacement and native behavior evidence.
The [blinded pilot](#blinded-semantic-recovery-pilot) establishes a usable method
but recovered both conditions. Broader workloads, pinned-model evaluation and
product cost/recovery gates remain open.

### 4. Select improvements only from measured gaps — next

Improve an existing pass when evidence shows a useful benefit within the agreed
correctness and performance budgets. Consider a new candidate only after those
baselines justify it. No new pass or advanced MBA expansion is the default next
task. Native Linux success must still be followed by the relevant existing macOS
and Windows checks before claiming cross-platform correctness.
Entry-only predicate caching, target caching, and seeded volatile-target dispatch
have failed this process; all runtime changes were rolled back. Consult their
paired, PMU and recovery evidence before repeating those designs or assuming a
simpler dispatch sequence is faster or equally difficult to analyze.
Good performance, >300k-LoC real-workspace validation, and blinded tool-assisted
LLM resistance must be measured together before claiming product acceptance.
Do not substitute generated source padding for scale evidence or model/tool
failures for resistance. Keep private inputs and license material local unless
external use is explicitly authorized.

## Evidence to keep between sessions

For each failure or retained comparison, store the source revision and local
patch state, tool versions, target, flags, seed, inputs, original/transformed IR,
binaries, expected/actual observable results, commands, exit codes, timeouts, and
analysis reports/settings. Keep working artifacts under `target/` or an external
experiment directory and preserve important evidence before cleaning builds.

The repository ignores `*.ll` and `*.bc` globally. If a reduced IR case earns a
permanent regression, give it an intentional tracked location and ensure the
relevant ignore exception is addressed; an ignored local fixture will not reach
the next machine or CI. Do not commit bulk binaries, decompiler databases, private
application code, or license material. Share only authorised fixtures with model
providers or external analysis services.

## Suggested prompt for the next session

> Read docs/DEVELOPMENT_HANDOFF.md and ROADMAP.md, including the acceptance pillars
> and all three rejected dispatch experiments. Preserve the shipped branch
> lowering, conservative MBA policy, and both scale-preparation fixes. Read the
> rust-analyzer baseline and complete four-family recovery comparison. Reuse
> their verified corpus/controller instead of repeating setup. Investigate the
> observed semantic-recovery weaknesses before proposing another lowering change;
> instruction count, retained indirect jumps and broken pseudocode are not proof
> of efficient protection. Predeclare cost/coverage/recovery gates and obtain a
> reproducibly identified model for strong quantitative claims. Keep all twelve
> Cargo tests and 78 native cases passing. Preserve the opt-in S2 subset and its
> recorded size exception, plus A1's bounded region/ABI contract and same-LLVM
> C++ build requirement. Read the A1 cost, no-op timing controls and limited
> recovery results, plus rejected A3 wrappers, A4 specialisation, the B1/C3
> return-contract proposal and B2 switch lowering. B1/C3 passed focused LLVM
> refinement checks but exposed a native lowering failure; do not bypass it or
> trust unknown freezes. B2's late placement passed scoped cost checks and hid
> static case mappings, yet every blinded condition was fully recovered.
> Extra calls, distinct code and static target hiding are not semantic secrecy.
> B3's pointer-dependent offset tables passed focused native checks but failed
> every text-growth gate; unchanged stripped size was alignment slack, not free
> protection. B4's byte aliases only partially recombined and also failed every
> text-growth gate, despite native and actual-emission refinement checks.
> C1's caller-owned bundles passed scoped correctness/cost gates, but binary-only
> lifting matched every tested bundle leaf back to its scalar computation.
> Preserve the metadata-only identity regression and exact call-contract proof
> normalization; generic inlining can erase source UB premises. C2's bounded
> PreserveNone register-lane permutation passed native ABI checks but failed
> two composition runtime gates. It is not arbitrary register assignment or a
> stable LLVM-version ABI; keep solver timeouts separate from structural proofs.
> D2's defined-carrier constant reconstruction passed native and size checks,
> but four runtime gates failed and seven actual-function proofs timed out.
> No constant-lift flag or recovery/scale claim followed. D3's quadratic family
> passed all40 probe refinements and native checks, but binary-only lifting
> normalized all200 positive predicate/selector instances; nonconstant controls
> remained nonconstant. No S1 rewriter, full-corpus cost or scale trial followed.
> D1's two-order scheduler passed correctness and all cost gates. Its original
> raw-text trial remains intact; a complete schema-parsed rerun recovered15/16
> conditions at each endpoint, including all eight six-pass conditions. It
> remains unpromoted. Exact model identity and raw provider/internal repair
> details remain unavailable; neither tool errors nor nonexact scores establish
> protection. Y2's final function permutation moved native entries, but anonymous
> matching recovered all192 true-pair bodies, including every moved entry. It
> stopped before runtime/locality, model or scale claims. S3's real scalar VM
> passed scoped native and nine bounded actual-engine refinements, but all six
> MBA/six-pass text-growth comparisons failed at404–528B against256B. No hot
> runtime, cold-amortization, recovery or scale claim followed. The inventory now
> has bounded decisions, not product acceptance. Choose further work from a new
> benefit case and reproducible model/workload evidence, not weaker gates or
> automatic pass additions. There is no VM annotation API or BCF upgrade.
> Preserve conservative eligibility and floating exclusion; do not lower gates,
> revive rejected designs, claim untested platforms/resistance, or add passes
> by default.
