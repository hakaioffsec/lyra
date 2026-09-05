# Development handoff — Linux baseline and MBA correctness

## Start here

**Fix existing-pass correctness before adding passes or increasing complexity.**
The first coding task is a targeted floating-point MBA regression and fix, followed
by an audit of the supported arithmetic semantics. IDA/Ghidra integration helps
inspect output; it is not the correctness oracle.

Read [ROADMAP.md](../ROADMAP.md) for scope and acceptance gates, and
[README.md](../README.md#ci-and-regression-checks) for the current build workflow.
This handoff records decisions and findings from the roadmap review. It does not
claim that the Linux tools are installed, MCP servers are connected, or native
verification has passed.

## What changed, and what did not

- `ROADMAP.md` now separates implemented features, historical evidence, proposed
  techniques, correctness gates, and measurement milestones. Speculative tiers,
  LOC estimates, sprint dates, and universal effectiveness claims were removed.
- Detection-evasion milestones and anti-debug sabotage were removed from scope.
- `README.md` describes the revised roadmap and links to this handoff.
- Runtime code was not changed. The arithmetic issue below remains unresolved.
- Documentation references and arithmetic counterexamples were checked. No
  compiler build, native runtime regression, IDA run, or Ghidra run was performed
  during the documentation work.

### Current implementation facts

| Item | Current state |
|---|---|
| Passes | `--string-enc`, `--shuffle-blocks`, `--indirect-branch`, `--mba`; all opt-in. |
| Execution order | String encryption → block shuffle → indirect branch → MBA, fixed in `src/main.rs`. |
| Seed | `--seed <u64>` controls per-pass RNG streams for the same module inputs; pass order is not randomised. Final artifact reproducibility needs separate verification. |
| Removed passes | Legacy instruction substitution, BCF, and CFF are absent. There is no `--substitution` flag or implicit BCF block-splitting dependency. |
| String lifecycle | Eligible globals are decrypted in place at startup. On-disk plaintext absence is not runtime confidentiality. |
| Branch coverage | Eligible `br` terminators are transformed; `switch` terminators remain. Shared eligibility rules skip EH-pad functions and other constructs. |
| MBA | Three variants per supported opcode, two rounds, 60% selection probability per round. Includes integer and floating-point operations. |

Source entry points: `src/obfuscator/mba.rs`, `src/obfuscator/indirect_branch.rs`,
`src/obfuscator/string_encryption.rs`, `src/obfuscator/shuffle_blocks.rs`,
`src/obfuscator/seed.rs`, and `src/obfuscator/util.rs`.

### Known arithmetic blocker

`fmba_add` in `src/obfuscator/mba.rs` includes the transformation:

```text
Original:     a + b
Replacement: 2*a + (b-a)
Inputs:      a = 1e308, b = -1e308, binary64
Original result:    0.0
Replacement result: NaN (overflowing intermediates)
```

This was evaluated as floating-point arithmetic. It is a counterexample to the
identity, **not yet a reproduction through Lyra**. The source emits this variant
using separate floating-point multiply, subtract, and add instructions.

Absence of fast-math prevents certain optimisations; it does not make a newly
introduced real-number identity valid under IEEE 754. Other float variants also
need review, not just this one. Do not fix the symptom with a special case for
these two inputs or weaken the result comparison.

Two incorrect examples from the old roadmap were also evaluated and corrected:
`35*35 - 34*6*6 == 1` disproves its claimed always-true predicate, and
`0x1337 ^ 0xD3C9 == 0xC0FE`, not `0xC0DE`. These were documentation errors, not
newly reproduced runtime bugs.

## Linux workstation preparation

Prefer native Linux x86-64 to match the configured Linux CI job. The CI matrix
also includes macOS ARM64 and Windows x86-64 MSVC; a Linux run does not verify
those platforms. Retain access to native runners for cross-platform confirmation.

### Required for the first coding task

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

Installations and client wiring still need to be verified on the Linux machine.
These tools complement the runtime harness; none makes LLVM structural validity,
a decompiler's output, or an LLM assessment a semantic proof.

| Tool | Purpose | Setup / limits |
|---|---|---|
| [Alive2 / alive-tv](https://github.com/AliveToolkit/alive2) | Check small before/after LLVM transformations under LLVM semantics. First target: arithmetic rewrites. | Pin a compatible LLVM/Alive2 pair. Upstream targets LLVM main; do not assume compatibility with Lyra's LLVM 22. Keep any separate analysis toolchain isolated from Lyra's PATH/prefix. Unsupported constructs, timeouts, and bounds are not a successful proof. Interprocedural transformations are explicitly unsupported. |
| [IDA Pro MCP](https://github.com/mrexodia/ida-pro-mcp) | Interactive inspection of instructions, decompilation, and references. | Community integration. Its README lists IDA Pro 8.3+, recommends 9, and excludes IDA Free. Have the appropriate licensed Hex-Rays decompiler. Current guidance prefers `idalib-mcp` over the older GUI plugin. Pin a reviewed version. |
| [Hex-Rays idalib](https://docs.hex-rays.com/core/idalib/getting-started) | Repeatable headless analysis and scripted exports. | Use IDAPython or the Domain API; follow activation/licensing instructions for the installed IDA version. A Lyra-specific exporter has not been implemented. |
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

## Ordered implementation plan

### 1. Reproduce and fix floating-point MBA

- Read the current `fmba_add`, `fmba_sub`, `fmba_mul`, target selection, and their
  callers before editing. Do not assume the source is unchanged after switching
  machines or branches.
- Exercise the original and transformed function with runtime-supplied inputs so
  rustc cannot constant-fold the entire example before Lyra sees it.
- Retain original/transformed IR and confirm the failing variant actually fired.
  Seeded probability means a passing untransformed case proves nothing; select
  and record a reproducible seed or use a focused pass-level fixture.
- Compare the actual result to an explicit semantic expectation and the original.
  Account deliberately for NaNs and signed zero; ordinary float `==` is not a
  complete oracle, and tolerance checks would hide strict-semantic changes.
- Keep a regression that fails before the fix and passes afterward. Replace or
  remove transformations that cannot preserve the supported semantics, rather
  than leave a known-unsound rewrite active. Audit all float variants and update
  affected documentation if coverage changes. Do not add a new CLI mode just to
  retain unsound behaviour.

**Done when:** an actual Lyra-path reproduction is fixed, the regression protects
the failure, supported float behaviour is accurately documented, and relevant
individual/combined runtime checks pass. A mathematical example alone does not
complete this task.

### 2. Audit integer MBA and relevant interactions

Check supported widths, modular arithmetic, narrow values, overflow flags,
poison/undef, and any applicable shift or comparison preconditions. Use Alive2
where supported and retain its exact version/options and verdict. Verify emitted
IR and native execution; do not claim vectors or unsupported constructs verified.
Exercise relevant combinations with indirect branches because MBA can transform
arithmetic introduced by that pass.

**Done when:** the supported subset has defensible semantics, counterexamples
have regression coverage, and timeouts/unsupported cases remain explicit gaps.

### 3. Establish a measured baseline for existing passes

Extend `scripts/ci_smoke.py` or use a focused companion experiment instead of
building a competing test framework. Compare plain compilation, Lyra no-pass,
individual passes, and selected combinations on representative workloads.

Measure runtime/startup, build time, file/section size, and transformed/skipped
coverage. Inspect final machine code. Preserve selected successful artifacts for
scripted IDA/Ghidra export using fresh databases and fixed analysis settings.
Record decompilation errors, timeouts, recovered structure, and manual
intervention separately. Bigger pseudo-C and different hashes are not success
criteria. Keep known fixture/source correspondence separately from the stripped
binary being evaluated so analysis does not receive unintended ground truth.

**Done when:** reports tie observations and costs to exact inputs, revisions,
seeds, targets, and tool versions. Scope any analysis claim to the tested engine
and settings; static observations do not establish runtime secrecy.

### 4. Select improvements only from measured gaps

Improve an existing pass when evidence shows a useful benefit within the agreed
correctness and performance budgets. Consider a new candidate only after those
baselines justify it. No new pass or advanced MBA expansion is the default next
task. Native Linux success must still be followed by the relevant existing macOS
and Windows checks before claiming cross-platform correctness.

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

> Read docs/DEVELOPMENT_HANDOFF.md and ROADMAP.md. Establish the native Linux
> baseline using the pinned toolchain, then reproduce and fix the floating-point
> MBA semantic failure in src/obfuscator/mba.rs. Keep a failing-before/passing-after
> regression, audit the other float variants, and preserve the relevant LLVM and
> native-runtime evidence. Do not add new obfuscation passes. Verify which MCP
> tools are actually connected before relying on IDA or Ghidra. Report known
> baseline failures separately and do not claim unexercised platforms verified.
