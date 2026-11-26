# ROADMAP — Software protection and correctness

Lyra transforms LLVM IR to increase the effort required to understand selected
application code. This roadmap separates implemented behaviour, available
verification, and proposed work. It does not promise confidentiality on an
analyst-controlled machine, universal decompiler failure, or detection evasion.

Correctness comes first, then compatibility and measured cost, then demonstrated
analysis impact. A longer instruction sequence or a different binary hash is not
by itself evidence of stronger protection. Historical pass IDs below identify
items from the old roadmap; their letters no longer represent effectiveness tiers.

## Product goal and acceptance pillars

Deliver a correct Rust obfuscation pipeline with **good performance**, reliable
operation on **real Rust projects exceeding 300,000 lines of Rust**, and passes
that make selected program semantics **very difficult to recover through
LLM-led reverse engineering even with CPU emulators, disassemblers and
decompilers**. These are acceptance goals, not demonstrated properties of the
current implementation. They must be achieved together; expensive complexity,
silent coverage loss, or a failed analysis-tool setup is not success.

### Good performance

- Declare workload-specific budgets for runtime/throughput, latency, startup,
  clean and incremental build time, peak memory, and file/section growth before
  evaluating a candidate. Compare with both controlled plain compilation and
  no-pass Lyra so pipeline overhead is not hidden.
- Exercise hot paths, cold paths, compute/control/data work and application-level
  workloads. Retain repeated samples, variability, machine/toolchain settings,
  and coverage. A speedup obtained only by silently skipping requested work does
  not meet the goal.
- The current public-fixture baseline is useful evidence, not proof of “good
  performance”: all-pass text work was 4.07–4.49× controlled plain, and no product
  performance budget has yet been satisfied across representative applications.

### Rust workspaces above 300k LoC

- Validate at least one authorized, real multi-crate Rust workspace above 300,000
  lines of Rust. Pin its revision and line-count tool/method; report first-party,
  generated and dependency code separately. Vendored/generated padding alone
  must not qualify a small application as a large-project validation.
- Count crates, LLVM modules/functions/blocks/globals, IR bytes and resulting
  artifacts as well as source lines. Rust generics and monomorphization can make
  similarly sized source projects very different compiler workloads.
- Exercise explicit selection of multiple workspace crates, final linking and
  real application/integration behavior. Record per-stage build time, peak
  memory, temporary-disk use, failures and transformed/skipped coverage.
  Generated stress cases supplement, rather than replace, real-project evidence.
- Completion requires correct linked execution within declared resource/cost
  budgets, with no hidden fallback to unobfuscated objects or omitted selected
  crates. Intentional eligibility exclusions remain visible. A first scoped
  Linux build/run now passes for ten selected libraries in a pinned 470,852-line
  rust-analyzer workspace, including exact archive-replacement checks. This is
  not all-crate, production-only-LoC, cross-platform, or comprehensive workload
  validation; see the [scale evidence](docs/DEVELOPMENT_HANDOFF.md#real-workspace-scale-baseline).

### Tool-assisted LLM resistance

- Run blinded, authorized semantic-recovery tasks against plain/no-pass artifacts
  and current pass profiles. Give the analyst model realistic disassembly,
  decompilation, CPU emulation, debugging and execution capabilities; do not
  restrict it to looking at noisy pseudo-C.
- Keep source, expected algorithms, symbol correspondence, hidden test inputs and
  scoring oracles outside the analyst environment. Pin model/version, prompts,
  sampling settings, binary/tool versions, seeds, time/token/tool-call budgets,
  and permitted observations. Check that emulators and analysis tools actually
  execute the baseline correctly before interpreting candidate failures.
- Score recovered implementations or semantic answers with held-out behavioral
  tests. Record complete/partial/incorrect recovery, success rate, elapsed effort,
  tool calls and model cost under predeclared gates. Model self-assessment, larger
  pseudo-C, and API-produced pseudocode are not correctness or difficulty scores.
- “Very difficult” must mean demonstrably lower successful recovery and/or
  substantially greater effort under a stated capable-analyst budget while
  meeting correctness, performance and scale goals. No universal resistance,
  impossible reverse engineering, or runtime plaintext secrecy is promised.
  The initial one-function pilot recovered both plain and all-pass semantics.
  The expanded four-family baseline recovered 30/32 profile/function conditions,
  including all four all-pass cases. A no-op string-encryption condition's model
  failure is not protection. Exact backend identity/settings remain unavailable,
  so these scoped outcomes are not a pinned-model resistance score.
- Use redistributable or explicitly authorized inputs. Do not send private code,
  proprietary artifacts or license material to external model services without
  authorization.

### Execution order

1. Maintain M1 correctness and explicit safe subsets.
2. Establish M2 cost/coverage evidence: public-fixture measurements first, then
   machine-level profiling, real large-workspace validation, and blinded
   tool-assisted LLM baselines.
3. Use those measurements for one bounded M3 improvement at a time. Predeclare
   gates, retain failed experiments, and reject changes that trade away a goal
   without an explicit decision.
4. Repeat on broader workloads and the relevant native platforms before wider
   claims. Consider new techniques only when these baselines justify them.


## Threat model and limits

Assume an analyst has the binary; a capable LLM with tool use; IDA with Hex-Rays,
Ghidra or Binary Ninja; CPU emulators; strings/object-file tools; a debugger; and
the ability to execute, instrument and inspect memory. Record exact model/tool
versions and capabilities in every evaluation.

Evaluate these properties separately:

| Property | Question |
|---|---|
| Static data visibility | Which selected literals and data references remain visible in the file? |
| Disassembly and control flow | Which instruction boundaries and branch/call targets are recovered correctly? |
| Decompilation | Is output correct, incomplete, or merely harder to read? |
| Cross-build matching | How much code can be matched despite layout or expression changes? |
| Runtime inspection | What plaintext, targets, and computation become observable during execution? |
| LLM-led semantic recovery | Can a tool-equipped model recover behavior or an equivalent implementation under a fixed budget and held-out tests? |

Modern disassemblers do not rely only on linear sweep. An indirect branch is not
necessarily an unresolved branch, and awkward pseudo-C is not necessarily wrong
pseudo-C. Function matching can use more than direct call edges or exact bytes.
MBA complexity alone does not establish resistance to algebraic simplification.

The current string pass decrypts selected globals in place at startup. Removing
plaintext from the file does not remove it from process memory. More generally,
obfuscation cannot guarantee that secrets or executed behaviour remain hidden
from an analyst who controls execution. Static-analysis results must not be
presented as evidence of resistance to debugging or instrumentation.

## Current implementation

Implemented means present in the source and callable, not universally correct or
proven effective against analysis tools. All six transformation flags are opt-in.

| Feature | Source / flag | Behaviour and limits |
|---|---|---|
| String encryption | `src/obfuscator/string_encryption.rs`, `--string-enc` | XOR-encodes eligible byte-array globals with per-byte keys; a constructor decrypts them in place. Coverage is limited to selected modules and eligible globals, not every string in a linked program. |
| Block outlining (A1) | `src/obfuscator/block_outline.rs` / `.cpp`, `--block-outline` | LLVM `CodeExtractor` moves at most one reachable, non-entry, acyclic call-free block per function into an internal `noinline` helper. At least eight operations; at most six live-ins/two live-outs; conservative ABI/frame/type exclusions. Helpers can feed S2; bounded cost and static effect are not semantic secrecy. |
| Block shuffle (A2) | `src/obfuscator/shuffle_blocks.rs`, `--shuffle-blocks` | Permutes non-entry blocks in eligible functions with at least three blocks. Preserves IR CFG semantics; does not inherently change instructions within blocks. Final layout depends on code generation and linking. |
| Indirect branch (S1) | `src/obfuscator/indirect_branch.rs`, `--indirect-branch` | Rewrites eligible direct `br` terminators using per-function block-address tables and runtime index computations. Leaves `switch` terminators alone. Do not describe the table as an encrypted byte array or assume its targets cannot be recovered. |
| Indirect call (S2) | `src/obfuscator/indirect_call.rs`, `--indirect-call` | Replaces eligible local callees with volatile-loaded, additively encoded function-pointer entries while preserving each call's ABI/attributes. Exact private/internal definitions only; conservative call/layout exclusions. Observed static target hiding is not semantic secrecy. |
| MBA (B1) | `src/obfuscator/mba.rs`, `--mba` | Implements scalar integer add/sub/xor/and/or substitutions for widths above one bit with proven-defined operands: three variants per opcode, two rounds, and a 60% selection probability per round. Unproven operands and all floating-point instructions are left unchanged. No new freeze instructions are introduced; see the scoped arithmetic audit below. |
| Seed control (part of D1) | `src/obfuscator/seed.rs`, `--seed <u64>` | Derives per-pass RNG streams from the seed and module identity when set; otherwise uses fresh entropy. Does not randomise pass order or guarantee a different final binary on every invocation. |

The shared eligibility helper in `src/obfuscator/util.rs` excludes declarations,
LLVM intrinsics, the Rust EH personality function, functions containing EH pads,
and functions with recognised direct calls to coroutine intrinsics. This is not
a claim that every other function is safe under every transformation.

The old `instruction_substitution`, `bogus_control_flow` (BCF), and
`control_flow_flattening` (CFF) modules were removed. There is no current
`--substitution` flag. Arithmetic extensions belong to the existing MBA work;
BCF, CFF, and a standalone block-splitting pass are not available dependencies.

### Runtime pass order, not development order

`src/main.rs` currently applies enabled transformations in this fixed order:

1. String encryption.
2. Block outlining.
3. Block shuffle.
4. Indirect branch.
5. Indirect call.
6. MBA.

Pipeline randomisation is not implemented. In particular, block shuffle is not
currently last. Changing this order requires composition evidence, not an
assumption that any permutation is safe. New transforms must document which IR
forms they consume, what they produce, and their required ordering constraints.

A fixed seed controls transformation choices for identical module inputs. A
byte-reproducible final artifact additionally depends on toolchain versions,
paths, build inputs, and linker behaviour; verify that separately. Likewise,
fresh entropy does not ensure visible changes when nothing eligible transforms
or downstream compilation produces the same code.

## Evidence available today

### Public verification harness

`scripts/ci_smoke.py` defines executable, shared-library, and two-crate workspace
scenarios using `test_project`, `test_dll_project`, and `test_workspace_project`.
It builds plain references, checks explicit
runtime output, compares transformed execution, verifies emitted IR with LLVM
`opt`, and checks absence of a specific full plaintext canary when string
encryption is enabled. The shared-library probe exercises typed ABI calls, a
loader-time greeting, and strict `f32`/`f64` arithmetic edge cases with
runtime-supplied operands. It compares non-NaN results bitwise (including signed
zero) and checks NaN classification without requiring a particular payload/sign.
The workspace regression additionally verifies default-bin, explicit multi-crate,
and library-only encryption with both hyphenated and normalized Rust crate names.
A reduced native CFG regression protects full LLVM local names at and beyond
1,024 bytes, including two labels that differ only after that shared prefix.
The CLI reader, backend reader, and manual verification use the same unlimited
local-name setting; no textual name shortening or skipped crate is substituted.

Native JIT regressions also cover defined-bit preservation with literal,
selected, and frozen undef, exhaustive small-width integer pairs, and larger-width
boundaries through i128. `scripts/export_ida.py` supplies a separately exercised
Linux IDA 9.1 disassembly/decompiler export path; it is not a correctness oracle
or proof that a transformation resists analysis.

The smoke suite covers no-pass and all-pass profiles at seed 0. The full suite
adds each pass individually and uses seeds 0, 1, and 42. Neither suite covers
every pass subset, every input, or every floating-point edge case.

`.github/workflows/ci.yml` configures native Linux x86-64, macOS ARM64, and Windows
x86-64 MSVC jobs. These are configured checks, not a record of a particular green
run, and do not establish runtime coverage for every advertised target or crate
type. See [README.md](README.md#ci-and-regression-checks) for prerequisites and
commands. Preserve run-specific reports and failing artifacts as evidence.

### Reproducible measurement harness

`scripts/measure_passes.py` extends the existing runner with controlled plain,
no-pass, individual, branch+MBA, call+MBA, outline+call, and all-pass comparisons. It exercises integer,
text, FFT, and shared-library fixtures; records repeated build/runtime samples,
file/section sizes, input/tool provenance, and per-pass coverage; and retains
successful stripped artifacts for the direct IDA exporter. Coverage collection
uses a separate matched build, outside timed builds. MBA round-zero coverage is
distinct from visits to generated instructions in later rounds.

The measurement protocol uses cold target artifacts but warm/offline downloads
and runtime warmups, common LTO/codegen settings, seeded profile ordering, and
recorded CPU affinity. Startup/process wall time is not conflated with native
workload time, and the Python cdylib probe is not an isolated ABI latency test.
IDA receives numeric entry addresses and stripped copies, not injected source
names/types; observed CFG/decompilation results are not correctness proofs.
See [measurement usage](README.md#measuring-existing-passes) and retain reports
before cleaning `target/`. No performance threshold or effectiveness claim follows
merely from making a report.

The first public Linux baseline completed 44 native profile/seed cases with
132 timed builds, 990 runtime samples and 36 separately matched coverage builds.
All native checks passed. On this corpus, indirect branching dominated hot-work
cost, conservative MBA skipped most original integer candidates, and some DLL
profiles were no-ops. IDA produced pseudocode for 24 of 25 selected entries, but
some outputs were incomplete and one entry was not discovered automatically.
These are scoped observations, not a universal recovery-failure claim. Exact
costs, variation, inputs and limits are in the
[measured handoff](docs/DEVELOPMENT_HANDOFF.md#measured-existing-pass-baseline).

### Bounded outlining evidence

The opt-in A1 pass has native SSA/ABI/memory regressions and a same-LLVM C++ shim.
The current Linux run passes 12 Cargo tests and 78 native CLI cases. One genuine
state-update block in the unchanged recovery corpus becomes a surviving helper;
S2 changes its resolved IDA call into an encoded computed call.

Matched state-transition runtime medians were 0.975–1.023× their respective
references across standalone/composed profiles and seeds, with 72–360 bytes of
stripped growth. The older public fixtures have zero A1 opportunities and
byte-identical no-pass artifacts; their timing variation is not transformation
cost or protection evidence. The six-pass large-workspace run extracted 93
regions from 203 eligible blocks across ten selected libraries, verified exact
archive replacement and passed scoped linked runtime checks within resource caps.

In one seven-condition blinded state-transition comparison, standalone outlining
was fully recovered. Outline+call and all-six submissions matched 202/256 held-out
cases and no full stateful sequences. Formatting/query errors consumed decisions,
and exact model identity remains unavailable: these are limited observations,
not proof of strong resistance. See the
[A1 handoff](docs/DEVELOPMENT_HANDOFF.md#block-outlining-pass-a1).

### Function-wrapper experiment (A3)

The scoped forwarding design was implemented and **rejected**, not added as a
flag. Original private function identities were retained while their bodies moved
into internal implementations; `noinline` and `notail` preserved real call/return
boundaries. Native ABI, memory and composition checks passed.

In the unchanged corpus, binary-only initialized-ELF analysis recognized and
resolved **all 24 added wrappers** across four candidate profiles and three seeds,
including A1/S2 composition. Standalone string-work runtime increased
**34.5–37.7%**, exceeding its 25% ceiling; 11/12 transformed-family comparisons
failed their declared runtime gates. More call layers did not earn retention.
No new model or large-workspace run followed these mandatory early failures.
See the [A3 experiment](docs/DEVELOPMENT_HANDOFF.md#function-wrapper-experiment-a3)
for matched controls, exact limits and retained evidence.

### Constant-specialisation experiment (A4)

The bounded candidate was implemented and **rejected as an obfuscation pass**.
It created four distinct native variants, removed six constant parameters and
preserved generic functions needed by other calls and function pointers. Native
ABI, identity, memory and composition checks passed.

Binary-only execution and scalar lifting matched **all 48 clone instances**
across four candidate profiles and three seeds to partial evaluations of their
true generic functions, with UNSAT bitvector inequality checks and no solver
timeouts. No source, private symbols or expected bindings entered that matcher.
Distinct code and reduced signatures did not establish an added matching obstacle.
Growth stayed within budget; directly transformed workload medians were
0.967–1.082× their references, with two near-threshold composition/control
measurements above the 1.10× ceiling. No production flag, new model trial or
large-workspace run followed the failed early gate. See the
[A4 experiment](docs/DEVELOPMENT_HANDOFF.md#constant-specialisation-experiment-a4).

### MBA return-contract experiment (B1/C3)

A bounded eligibility extension was **rejected as proposed**. It used explicit
scalar integer callsite-return `noundef` contracts as definedness origins, without
changing the calls, recipes, rounds, probability or existing poison guards.
Standalone round-zero eligibility increased from 24 to 31 operations.

Normal-data native/emulator checks passed, but an adversarial case exposed a
candidate-only LLVM 22.1.8 lowering failure: `Cannot select: intrinsic
%llvm.ssa.copy`. Original and shipped-pass controls succeeded; the candidate
failed at three seeds under llc O0/O2/O3 and clang O0. Alive2 proved the exact
failing emitted function a correct LLVM refinement, underscoring that semantic
proof does not establish backend compatibility.

The shipped conservative producer set is unchanged. No runtime benchmark,
new model trial or scale build followed the hard failure. A narrower policy
would need its own declared contract and native evidence, not a dropped
counterexample. See the
[return-contract experiment](docs/DEVELOPMENT_HANDOFF.md#mba-return-contract-experiment).

### Custom switch-lowering experiment (B2)

The bounded encoded-dispatch candidate was **not promoted**. It preserved all
cases/defaults and duplicate-edge PHIs, with constant nonvolatile tables and
original-width bounds before pointer conversion. Native correctness passed.

An early placement failed a composition/control runtime gate at 1.215×.
A separately declared late placement, after all six existing passes, preserved
their outputs exactly and passed all 48 runtime comparisons and growth gates.
The genuinely transformed state workload took 0.996–1.013× its matched controls.
The same ten selected libraries in the pinned 470,852-line rust-analyzer
workspace also passed exact archive-replacement and linked semantic checks;
2,412 B2-stage switches were transformed within the declared resource limits.

Matched IDA output lost the case mapping, but binary-only initialized-memory
analysis recovered every tested encoded dispatch. **All eight blinded semantic
conditions were fully recovered**, including all four B2 profiles: 256/256
held-out cases and 32/32 stateful sequences each. Single trials, protocol errors
and unpinned model identity do not support a reliable effort or resistance claim.
The static effect alone did not justify another production pass. See the
[B2 experiment](docs/DEVELOPMENT_HANDOFF.md#custom-switch-lowering-experiment-b2).

### Address-offset materialisation experiment (B3)

The bounded pointer-dependent offset-table candidate was **rejected**.
It preserved original GEP bases/flags and memory contracts, and passed focused
native memory/pointer checks plus the unchanged Rust corpus. The tables and
address calculations survived code generation.

All **twelve native text-growth gates failed**: 432–528 added bytes against
a 256-byte allowance. Standalone `.text` grew from 1,212 to 1,724 bytes.
Stripped file size stayed unchanged because layout slack absorbed the growth;
that is not zero code/table cost or a measured runtime result.
Per the declared early-stop policy, no timing, new recovery trial or scale build
followed. Production remains unchanged. See the
[B3 experiment](docs/DEVELOPMENT_HANDOFF.md#address-offset-materialisation-experiment-b3).

### Byte-alias access experiment (B4)

The defined-value byte-alias candidate was **rejected on native growth**.
It split selected ordinary integer loads/stores into same-storage byte accesses,
excluding volatile/atomic and unsupported memory contracts. Focused native
overlap/alignment/guard-page checks and 54 actual-emission LLVM refinements
passed, as did the unchanged four-family Rust corpus.

A small overlap pilot compiled identically to its reference, but the real
corpus only partially recombined: narrow stores survived. All **twelve `.text`
growth comparisons failed**, adding 272–474 bytes against a 256-byte allowance.
Unchanged stripped size reflects layout slack, not zero cost. No timing,
new recovery trial or scale build followed the mandatory early failure.
Production remains unchanged. See the
[B4 experiment](docs/DEVELOPMENT_HANDOFF.md#byte-alias-access-experiment-b4).

### Parameter-aggregation experiment (C1)

The caller-owned bundle candidate was **rejected on incremental effect**, not
cost. It completely migrated one private three-argument leaf and three callers
to seeded 24-byte stack bundles, preserving range/return contracts and caller
lifetimes. Address-taken and unsupported identities remained scalar.

Native correctness, eighteen contract-preserving wrapper refinements and all
48 runtime comparisons passed. `.text` grew 48–80 bytes within budget; the
closest runtime result, 1.0999304855×, narrowly passed its 1.10× ceiling.

Anonymous binary lifting and bitvector proofs matched **all twelve candidate
bundle instances** back to their scalar computations, including S2/A1
compositions. Exact sampled field/register and return correspondence covered
2,493 calls per matched pair; no solver result was unknown. This is bounded
leaf normalization, not whole-program recovery, but it removes the added layer.
No new production flag, model trial or scale build followed. See the
[C1 experiment](docs/DEVELOPMENT_HANDOFF.md#parameter-aggregation-experiment-c1).

### Register-lane calling-convention experiment (C2)

The bounded x86-64 candidate was **rejected on runtime cost**. It combined
seeded logical-argument permutations with LLVM22's existing PreserveNone
register bank; it did not implement arbitrary physical-register assignment
or a stable external ABI.

Real register mappings survived, all callers migrated, and native SysV
register/RSP canaries passed—including actual S2/A1/six-pass corpus artifacts.
Growth stayed within budget at 32–64 added `.text` bytes. However, two
seven/six medians exceeded the 1.10× runtime ceiling: **1.147346×** for text
work and **1.101342×** for loop work. No selective rerun or gate waiver followed.
Solver timeouts are retained separately from exact normalized-IR identities;
neither is a resistance score. No recovery or scale claim was made.
See the [C2 experiment](docs/DEVELOPMENT_HANDOFF.md#register-lane-calling-convention-experiment-c2).

### Constant-reconstruction experiment (D2)

The defined-carrier candidate was **rejected on runtime cost**. It replaced
selected wide i32/i64 literal operands using the existing three `mba_add`
identities and modular cancellation. Only explicit same-width `noundef`
parameters supplied carriers; consumer flags/contracts remained unchanged.
There were no added memory effects, tables, freezes or input promises.

Native checks passed and decoder arithmetic survived the direct `llc -O2`
path. All twelve growth gates passed at **80–256 added `.text` bytes**.
However, four of 48 runtime medians failed: branching work reached
**1.205054×** against the MBA control's 1.15× cap and **1.121539×/1.176550×**
against the six-pass control's 1.10× cap. An unchanged-IR string-work control
also failed at **1.142558×**; no selective rerun or cause claim followed.

Eight algebra queries were UNSAT, but actual-function LLVM refinement remains
incomplete: **six proved, seven timed out**. A custom-metadata translation
failure and its analysis-only normalization are retained separately.
No recovery/scale trial or production flag followed. This rejects the scoped
representation, not all constant transformations. See the
[D2 experiment](docs/DEVELOPMENT_HANDOFF.md#constant-reconstruction-experiment-d2).

### Quadratic-residue predicate experiment (D3)

The bounded nonlinear predicate family was **rejected at native simplification
feasibility**, before building an S1 rewriter. It preserved an arbitrary
existing selector condition using a proved quadratic nonresidue over defined
i32/i64 carriers; it did not replace S1's volatile key comparison or revive
either cache design.

All forty LLVM refinements and **625,680 native full-return comparisons**
passed. Native multiplication survived all five tested backend configurations,
but source-independent binary lifting proved **all 100 predicate instances**
constant and reduced **all 100 selector instances** to the original flag test.
All twenty nonconstant sensitivity-control instances remained nonconstant;
74,910 native/emulator/lift observations agreed, with no solver unknown or gap.
These are probe/backend instances, not distinct algorithms or whole-program
recovery results.

The declared early failure stopped automatic rewriting, full-corpus cost,
model and scale trials. Production S1 and the six-pass pipeline remain unchanged.
See the [D3 experiment](docs/DEVELOPMENT_HANDOFF.md#quadratic-residue-predicate-experiment-d3).

### Bounded pipeline-order experiment (remaining D1)

The shipped/late-shuffle scheduler was **not promoted**: scoped correctness
and cost passed, but incremental recovery benefit was not established.
Only the copied driver changed. String/A1 stayed first and S1/S2/MBA retained
their relative order; a separate seeded stream chose whether A2 ran before
S1 or last. Every pass still ran once in one LLVM module.

All 64 flag subsets at three seeds, 125 focused native executions and the
matched corpus passed. All 48 runtime comparisons and twelve paired growth
checks passed; the closest six-pass median was **1.097005×** against 1.10×.
Final IR block order was identical in every paired condition. Existing
dispatch/MBA choices changed, while two shuffle+MBA pairs were entirely
byte-identical, including stripped ELF output.

The original raw-text recovery trial produced **11/16 exact candidate
conditions** versus **10/15 completed reference conditions**, with one reference
connection failure, 99 malformed/oversized responses and 18 failed tool requests.
Those failures remain recorded and are not protection.

A separate complete **schema-parsed rerun** recovered **15/16 conditions at
each endpoint**, including **all eight six-pass conditions**. It retained the
same binaries, held-out data and external action/query budgets. All 272 API
calls returned valid parsed actions; 22 tool requests failed. Raw provider
text, internal repair attempts and exact model identity remain unavailable.
This does not establish incremental hardening or raw-format error reduction.
No selective retry, scale build or public pipeline-order API followed. See the
[D1 experiment](docs/DEVELOPMENT_HANDOFF.md#bounded-pipeline-order-experiment-d1).

### Function-layout experiment (formerly Y2)

The final definition-order permutation was **rejected on native body matching**.
It preserved function objects, bodies, ABI/metadata, aliases and constructor
arrays after the frozen shipped pipeline. All twelve requested orders survived
LLVM/bfd; 183 definition slots and 188 native entry addresses moved.

Scoped native checks passed, and growth stayed within budget at **−8 to +10
`.text` bytes**, with unchanged stripped size. Anonymous ELF/unwind/CFG analysis
then matched **all 192 true-pair function bodies**, including the moved entries,
without private symbols or supplied pairings. No true-pair body was wrong,
ambiguous, missing or unsupported. This is normalized instruction/body matching,
not raw-byte or whole-program/data equivalence.

The declared early matching failure stopped runtime/locality benchmarks, new
model trials, scale and integration. No layout flag was added. See the
[function-layout experiment](docs/DEVELOPMENT_HANDOFF.md#function-layout-experiment-y2).

### Bounded register-VM experiment (S3)

The automatic pure-integer-leaf VM candidate was **rejected on composed
native text growth**. It compiled real eligible LLVM operations into immutable
bytecode executed by shared typed interpreters, preserving original function
objects and ABIs. It emitted only used handlers and virtualized the genuine,
hot `h0` leaf—not fabricated cold work.

Seventy focused leaves exercised all 28 handlers at both widths. Twenty native
executables passed **19,353,600 full-return/buffer calls**, and nine selected
actual-engine LLVM refinement checks passed with explicit unrolling limits.
All corpus outputs matched and native bytecode dispatch survived.

None/S2 profiles added 142/153 `.text` bytes, but **all six MBA/six-pass size
comparisons failed**, adding **404–528 bytes** against 256-byte caps.
Stripped size stayed unchanged; that is layout slack, not free interpretation.
No runtime, cold-amortization, recovery or scale claim followed. No VM flag or
`#[lyra::vm_protect]` API was added. See the
[S3 experiment](docs/DEVELOPMENT_HANDOFF.md#bounded-register-vm-experiment-s3).

The listed inventory now has bounded experiment decisions, not product
acceptance. Further work needs a new benefit case, sound selection/contracts
and reproducible cost/recovery evidence—not automatic promotion or weaker gates.







### Historical internal report

The earlier roadmap reported a combined run on an internal Rust project:

- 9,501 strings encrypted.
- 14,418 blocks shuffled across 1,639 functions.
- 15,942 branches rewritten across 1,815 functions.
- Runtime behaviour preserved and tested business-logic plaintext absent from
  the final binary for that run.

These are historical reported results, not newly reproduced measurements. They
do not establish correctness for all inputs or quantify decompiler, binary
matching, or symbolic-execution performance. Any wider claim needs its own
reproducible corpus and results.

## Correctness gates before expanding coverage

### Arithmetic semantics

Prove integer identities at the supported bit widths and under LLVM semantics,
not just mathematical integers. Include wraparound, narrow widths, overflow
flags, poison/undef behaviour, shift bounds, and comparison signedness where
applicable. Do not generalise scalar support to vector operations without proof.

The integer audit reproduced a real undef failure: `or i8 undef, -1` must return
255, but an old MBA expansion returned 0. A freeze-based repair was valid at the
IR level yet exposed a separate LLVM 22.1.8 optimized-backend miscompilation.
The selected fix is conservative eligibility, not a compiler patch or reduced
comparison: only proven-defined operands are expanded, and uncertain operations
remain intact. Definedness starts from integer constants and explicit `noundef`
parameter/load contracts, then propagates through a restricted set of safe scalar
producers. It does not treat freeze of an unknown value as a safe origin.

The 15 existing modular identities remain. Original nowrap/disjoint flags are
not copied to potentially wrapping intermediates; this is LLVM refinement,
not a promise to preserve poison outcomes. Shift-by-one requires width above
one. Cyclic PHIs, unsupported producers, and unproven values reduce transformation
coverage. LLVM proof results and timeouts, the native lowering counterexample,
and the exact supported subset are recorded in the
[integer audit handoff](docs/DEVELOPMENT_HANDOFF.md#integer-mba-eligibility-audit).
Any eligibility expansion must repeat both IR refinement and native lowering
checks; successful IR verification alone missed both earlier failures.

Real-number algebra is not sufficient for IEEE 754 substitutions. The former
`fmba_add` variant `2*a + (b-a)` was reproduced through Lyra: with binary64
`a = 1e308` and `b = -1e308`, the original returned zero and the transformed
function returned NaN through overflowing intermediates. All nine float variants
had native counterexamples and were removed, including their target selection
and dispatch. MBA now leaves floating-point instructions unchanged, rather than
offering an unsound mode. The shared-library regression covers rounding,
cancellation, signed zero, subnormals, infinities, and NaNs; it failed before the
fix and passed afterward on Linux x86-64. See
[the development handoff](docs/DEVELOPMENT_HANDOFF.md#resolved-floating-point-blocker)
for evidence and remaining platform/proof gaps. Any future float rewrite needs
an explicit semantic contract and proof; absence of fast-math is not a proof.

The old opaque-predicate example `x*x - 34*y*y != 1` is not always true:
`35*35 - 34*6*6 == 1`. Any future predicate needs a proof for its actual operand
domain and machine arithmetic. The old constant example was also not an identity:
`0x1337 ^ 0xD3C9 == 0xC0FE`, not `0xC0DE`; an unconstrained added variable cannot
repair that claim. Future examples must state and verify their preconditions.

### Control flow, ABI, and platform behaviour

- Preserve PHIs, dominance, legal block-address uses, returns, unwind paths,
  exception metadata, and observable side effects. Report skipped constructs.
- For call/signature changes, specify calling conventions, parameter and return
  attributes, variadics, intrinsics, inline assembly, tail-call constraints,
  function-pointer uses, exported symbols, and foreign ABI boundaries.
- Preserve volatile/atomic semantics, alignment, address spaces, pointer validity,
  PIC/relocations, and constructor ordering where relevant.
- Validate final machine code and linked execution. LLVM IR validity alone does
  not prove semantic equivalence or that a transformation survives codegen.
- Retain crash diagnostics and supported platform protections. Changes to binary
  layout must account for unwind information and signing requirements.

Each pass needs an explicit supported subset. Unsupported constructs must not be
silently transformed under a universal compatibility claim.

## Proposed technique inventory

These are candidates for scoped experiments, not promised protection levels or
scheduled releases. Existing S1, S2, A1, A2, and seed support are described above. The old
B1/C3 arithmetic proposals are consolidated to avoid parallel substitution passes.

| Candidate | Intended scope | Required decision or evidence |
|---|---|---|
| Function wrappers (A3) | Add call boundaries around selected private functions. | Scoped forwarding candidate rejected: boundaries survived, but binary-only analysis contracted every added wrapper and runtime gates failed. No `--function-wrap` flag. Any different design needs a new incremental-effect case, not just more calls. |
| Cloning and constant specialisation (A4) | Produce selected semantically valid function variants. | Bounded constant-argument candidate rejected as obfuscation: genuine native clones survived, but binary-only partial-evaluation matching recovered every tested clone relationship. No `--constant-specialize` flag. Different code/signatures alone do not establish failed matching. |
| MBA extensions (B1/C3) | Extend the existing pass's supported operations or expression handling. | Explicit callsite-return `noundef` origin candidate rejected on LLVM 22.1.8 lowering compatibility despite focused Alive2 success. Existing producer set remains unchanged. Further bounded proposals require new semantic and native evidence; no floating-point or unknown-freeze expansion. |
| Custom switch lowering (B2) | Change the representation of selected switches. | Bounded encoded-table candidate not promoted: late placement passed scoped correctness/cost gates and hid static case mappings, but every blinded condition was fully recovered. No `--switch-lower` flag. A different design or future CFF/BCF consumer needs a separate contract and incremental-benefit case. |
| Address/offset materialisation (B3) | Change how selected addresses and offsets are expressed. | Bounded pointer-dependent offset-table candidate rejected: all twelve text-growth gates failed despite scoped native correctness. No `--address-offset` flag. Different designs still require GEP/provenance, PIC and relocation correctness; x86-64 `[rip+rax]` remains unencodable. |
| Alias-access transformations (B4) | Re-express selected memory accesses. | Defined-value byte-alias candidate rejected: native checks and focused LLVM refinements passed, but all twelve text-growth gates failed. Partial native recombination is not a resistance result. No `--alias-access` flag; different designs still need aliasing/alignment/atomic/volatile and data-recovery evidence. |
| Parameter aggregation (C1) | Group parameters for selected internal functions. | Caller-owned bundle candidate rejected on incremental effect: complete migration and scoped cost/native/refinement gates passed, but anonymous binary analysis normalized every tested bundle leaf to its scalar counterpart. No `--param-aggregate` flag. Preserve metadata-only identity exclusions and exact memory/tail/argument contracts in any different proposal. |
| Custom calling conventions (C2) | Target-specific internal call boundaries. | Bounded x86-64 PreserveNone-bank/argument-permutation candidate rejected: native ABI checks passed, but two composition runtime gates failed. No `--register-cc` flag. LLVM convention IDs still do not provide arbitrary physical-register assignment, and this ABI is version-unstable. |
| Constant transformations (D2) | Replace selected integer constants with equivalent computations. | Defined-carrier modular reconstruction candidate rejected: native/growth checks passed, but four runtime gates failed and full LLVM refinement remains incomplete. No `--constant-lift` flag. Different proposals must coordinate with existing MBA, prove bit-width/definedness semantics and measure native folding and incremental recovery. |
| Opaque predicates (D3) | Investigate predicates with formally established outcomes. | Quadratic-nonresidue family rejected at native-probe simplification: LLVM/native checks passed, but binary-only lifting removed every tested added predicate/selector component. No automatic S1 rewriter or new flag followed. Different candidates still need machine-semantic proofs and incremental simplification evidence; no current BCF pass exists to upgrade. |
| Pipeline variation (remaining D1) | Consider alternative orders only after composition is understood. | Bounded seeded shipped/late-shuffle scheduler not promoted: correctness/cost passed, but a complete schema-parsed rerun recovered15/16 conditions at each endpoint, including every six-pass condition. Exact model identity remains unavailable; incremental benefit is unestablished. Production remains fixed-order. Other orders need explicit dependencies, exact seeded reproduction and a separate benefit case; seed support alone is not pipeline randomization. |
| Function layout variation (formerly Y2) | Investigate module-to-machine layout effects. | Seeded final definition permutation rejected: requested placement survived and native/size checks passed, but anonymous matching recovered all192 true-pair function bodies. No layout flag or runtime/locality claim. Moving an entry changes addresses/encodings, not inherently its instruction structure or matching difficulty. |
| VM-based execution (S3) | Interpret a deliberately limited subset of selected application logic. | Automatic straight-line integer-leaf register VM rejected on composition text growth: real bytecode/native execution and scoped checks passed, but all six MBA/six-pass size gates failed. No VM API/flag. Cold/sensitive selection, broader memory/call/exception support and other architectures need separate contracts and evidence; this is not a rejection of every VM design. |

Block splitting, if a candidate needs it, must be scoped explicitly; it is not
implicitly supplied by the removed BCF. Likewise, development order need not
match runtime pass order: outlining can be implemented later yet run earlier if
a validated consumer requires that form.

VM selection should distinguish sensitive code from hot code. Hot functions
require particularly careful performance budgets. Interpreter-based execution
can raise analysis effort but does not make analysis categorically infeasible.

## Development milestones

### M1 — Establish a trustworthy baseline

Resolve known semantic blockers and document the supported subset of each current
pass. Extend existing runtime checks only where an observable boundary or
plausible failure warrants a regression case. Include arithmetic edge cases,
constructor behaviour, ABI boundaries, and relevant pass interactions.

**Exit evidence:** native runtime results linked to a revision, toolchain, target,
flags, and seeds; retained reproductions for fixed correctness bugs; explicit
coverage gaps. Do not label an entire target or pass verified from build success
or a single happy-path program.

### M2 — Measure performance, scale, and analysis impact

Initial public-fixture, real-workspace, and blinded-method Linux baselines are
recorded in the handoff. They do not complete M2: representative performance
budgets, broader scale/workload validation, demonstrated LLM resistance, and
native macOS/Windows confirmation remain open. The first blinded pilot recovered
both conditions rather than demonstrating resistance.

Use a representative, redistributable corpus with a plain baseline, no-pass
pipeline, individual passes, and selected combinations. Include compute-heavy,
branch-heavy, data-heavy, and shared-library cases. Choose workload-specific
performance and size budgets before assessing candidates.

Record:

- Runtime, startup time, binary/section size, and build time, with repeated runs
  and variability rather than a single timing.
- Eligible, transformed, and skipped coverage, so a no-op is not credited as
  effective protection or successful compatibility coverage.
- Final-code observations, not just transformed IR or changed hashes.
- Analysis-tool names, versions, settings, scripts/manual intervention, and
  timeouts; distinguish correct recovery, incomplete recovery, and wrong output.
- Runtime-observable plaintext and behaviour separately from static results.
- Seeded repeatability under pinned inputs separately from cross-build diversity.
- Real-workspace scale and resource use, plus blinded LLM-led recovery outcomes,
  as required by the acceptance pillars above.

**Exit evidence:** reproducible reports and example artifacts supporting every
published effectiveness claim. A failure in one tool/version is not universal
failure, and larger pseudo-C is not a proxy for analyst effort.

### M3 — Select the next bounded change

Two entry-hoisting variants for indirect-branch dispatch were measured and
rejected: both preserved correctness/coverage but failed predeclared runtime
gates. The original lowering remains shipped; fewer IR key loads were not a
native-speed improvement. See the
[rejected entry-cache experiments](docs/DEVELOPMENT_HANDOFF.md#rejected-entry-cache-experiments)
before revisiting predicate or target hoisting.

Choose a candidate only after M1/M2 identify a gap worth its compatibility and
performance cost. Write its supported subset, ordering constraints, exclusions,
correctness obligations, and measurable acceptance criteria before implementation.
Use a scoped prototype to resolve backend or binding uncertainty before estimating
a release. Do not infer completion effort from a reference project's line count.

**Exit evidence:** baseline-versus-candidate results meeting the predeclared
correctness and cost gates, plus demonstrated benefit on the intended workload.
Keep runtime ordering documented alongside the implementation. Defer candidates
that lack evidence; do not replace the old speculative sprint dates with new ones.

## Out of scope

The former YARA/EDR evasion sprint is removed. Import-table manipulation for
detection evasion, fake imports, metadata fingerprint randomisation, section-size
jitter, and malware-attribution avoidance are not product milestones. Code/layout
diversity is evaluated here as a software-maintenance and analysis property, not
a guarantee that security detections fail.

Anti-debug sabotage (former D4), deliberately incorrect output under debugging,
and counterfeit signing are also out of scope. Genuine signing and crash-report
compatibility are release-integration constraints, not evasion techniques. Bulk
packing/encryption is not part of this IR-pass roadmap.

For format correctness, do not confuse linker naming conventions with Windows
loader requirements: PE directory entries and section attributes carry critical
loading information; literal `.text`, `.CRT`, or `.pdata` names are not a blanket
loader requirement. Consult Microsoft's [PE format specification](https://learn.microsoft.com/en-us/windows/win32/debug/pe-format)
for any future format-related compatibility work. Such work must preserve loader
and signing contracts rather than assume cosmetic changes are harmless.
