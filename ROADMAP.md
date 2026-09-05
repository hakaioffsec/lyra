# ROADMAP — Software protection and correctness

Lyra transforms LLVM IR to increase the effort required to understand selected
application code. This roadmap separates implemented behaviour, available
verification, and proposed work. It does not promise confidentiality on an
analyst-controlled machine, universal decompiler failure, or detection evasion.

Correctness comes first, then compatibility and measured cost, then demonstrated
analysis impact. A longer instruction sequence or a different binary hash is not
by itself evidence of stronger protection. Historical pass IDs below identify
items from the old roadmap; their letters no longer represent effectiveness tiers.

## Threat model and limits

Assume an analyst has the binary, IDA with Hex-Rays, Ghidra, or Binary Ninja;
strings and object-file inspection tools; a debugger; and the ability to execute
and instrument the program. Record exact tool versions in any evaluation.

Evaluate these properties separately:

| Property | Question |
|---|---|
| Static data visibility | Which selected literals and data references remain visible in the file? |
| Disassembly and control flow | Which instruction boundaries and branch/call targets are recovered correctly? |
| Decompilation | Is output correct, incomplete, or merely harder to read? |
| Cross-build matching | How much code can be matched despite layout or expression changes? |
| Runtime inspection | What plaintext, targets, and computation become observable during execution? |

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
proven effective against analysis tools. All four transformation flags are opt-in.

| Feature | Source / flag | Behaviour and limits |
|---|---|---|
| String encryption | `src/obfuscator/string_encryption.rs`, `--string-enc` | XOR-encodes eligible byte-array globals with per-byte keys; a constructor decrypts them in place. Coverage is limited to selected modules and eligible globals, not every string in a linked program. |
| Block shuffle (A2) | `src/obfuscator/shuffle_blocks.rs`, `--shuffle-blocks` | Permutes non-entry blocks in eligible functions with at least three blocks. Preserves IR CFG semantics; does not inherently change instructions within blocks. Final layout depends on code generation and linking. |
| Indirect branch (S1) | `src/obfuscator/indirect_branch.rs`, `--indirect-branch` | Rewrites eligible direct `br` terminators using per-function block-address tables and runtime index computations. Leaves `switch` terminators alone. Do not describe the table as an encrypted byte array or assume its targets cannot be recovered. |
| MBA (B1) | `src/obfuscator/mba.rs`, `--mba` | Implements integer add/sub/xor/and/or and floating-point fadd/fsub/fmul substitutions, with three variants per opcode, two rounds, and a 60% selection probability per round. Floating-point semantic issues below are a correctness blocker, not an effectiveness feature. |
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
2. Block shuffle.
3. Indirect branch.
4. MBA.

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

`scripts/ci_smoke.py` defines executable and shared-library scenarios using
`test_project` and `test_dll_project`. It builds plain references, checks explicit
runtime output, compares transformed execution, verifies emitted IR with LLVM
`opt`, and checks absence of a specific full plaintext canary when string
encryption is enabled. The shared-library probe exercises typed ABI calls and a
loader-time greeting.

The smoke suite covers no-pass and all-pass profiles at seed 0. The full suite
adds each pass individually and uses seeds 0, 1, and 42. Neither suite covers
every pass subset, every input, or every floating-point edge case.

`.github/workflows/ci.yml` configures native Linux x86-64, macOS ARM64, and Windows
x86-64 MSVC jobs. These are configured checks, not a record of a particular green
run, and do not establish runtime coverage for every advertised target or crate
type. See [README.md](README.md#ci-and-regression-checks) for prerequisites and
commands. Preserve run-specific reports and failing artifacts as evidence.

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

Real-number algebra is not sufficient for IEEE 754 substitutions. The current
`fmba_add` variant `2*a + (b-a)` illustrates the problem: with binary64
`a = 1e308` and `b = -1e308`, `a+b` is zero, while the rewritten expression
produces NaN through overflowing intermediates. This arithmetic counterexample
is not a full Lyra runtime regression run, but it invalidates a blanket claim of
semantic equivalence. Resolve the float substitutions before recommending MBA
for arbitrary floating-point code. Cover rounding, cancellation, signed zero,
subnormals, infinities, and NaNs; absence of fast-math does not make a new rewrite
correct.

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
scheduled releases. Existing S1, A2, and seed support are described above. The old
B1/C3 arithmetic proposals are consolidated to avoid parallel substitution passes.

| Candidate | Intended scope | Required decision or evidence |
|---|---|---|
| Indirect calls (S2) | Selected direct calls through function pointers. | Define the supported call/ABI subset and measure target recovery; direct-edge removal does not erase callee identity. |
| Basic-block outlining (A1) | Extract eligible regions into private helpers. | Establish live-in/live-out and unwind correctness; measure call overhead and whether helpers remain after optimisation. LLVM's C++ `CodeExtractor` requires an explicit binding/shim decision, not an assumption that `llvm-sys` exposes it. |
| Function wrappers (A3) | Add call boundaries around selected private functions. | Demonstrate boundary survival and ABI preservation. Disabling linker-plugin LTO alone does not prevent inlining or tail-call folding. |
| Cloning and constant specialisation (A4) | Produce selected semantically valid function variants. | Preserve address-identity requirements; check compiler/linker merging and code-size cost. Different layout does not guarantee failed matching. |
| MBA extensions (B1/C3) | Extend the existing pass's supported operations or expression handling. | Resolve current correctness issues first. Prove new identities and bound growth before assessing simplification resistance. |
| Custom switch lowering (B2) | Change the representation of selected switches. | Preserve all cases and the default edge; measure emitted code and recovered structure. Any future CFF/BCF consumer would require a separate dependency contract. |
| Address/offset materialisation (B3) | Change how selected addresses and offsets are expressed. | Preserve target addressing rules, PIC, and relocation semantics. The old x86-64 `[rip+rax]` example is not encodable: RIP-relative addressing cannot also use a general-purpose index register. |
| Alias-access transformations (B4) | Re-express selected memory accesses. | Establish pointer validity, aliasing, alignment, and atomic/volatile correctness; measure whether data-model recovery actually changes. |
| Parameter aggregation (C1) | Group parameters for selected internal functions. | Migrate all relevant callers and function-pointer uses without changing external ABIs; measure allocation and access costs. |
| Custom calling conventions (C2) | Target-specific internal call boundaries. | Requires backend feasibility and complete caller/callee coordination. LLVM's predefined convention IDs do not implement arbitrary register permutations. No estimate before that scope is settled. |
| Constant transformations (D2) | Replace selected integer constants with equivalent computations. | Prove bit-width-correct equivalence and inspect final code for folding. Coordinate with MBA rather than duplicate its machinery. |
| Opaque predicates (D3) | Investigate predicates with formally established outcomes. | Prove outcomes under machine semantics and measure simplification. There is no current BCF pass to upgrade. |
| Pipeline variation (remaining D1) | Consider alternative orders only after composition is understood. | Specify dependency constraints and reproduce failures by seed and exact order. Seed support alone does not implement this. |
| Function layout variation (formerly Y2) | Investigate module-to-machine layout effects. | Establish backend/linker control and measure locality and matching. Moving an entry-point function does not inherently change its prologue bytes. |
| VM-based execution (S3) | Interpret a deliberately limited subset of selected application logic. | Define supported IR, memory model, external calls, and exceptions; measure interpreter overhead. Outlining is A1, not a nonexistent S4, and is not an assumed prerequisite. No `#[lyra::vm_protect]` API is implemented. |

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

### M2 — Measure existing passes

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

**Exit evidence:** reproducible reports and example artifacts supporting every
published effectiveness claim. A failure in one tool/version is not universal
failure, and larger pseudo-C is not a proxy for analyst effort.

### M3 — Select the next bounded change

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
