# lyra

Lyra is an LLVM IR obfuscator for Rust. It runs against an unmodified
rustc and cargo, requires no changes to the target project's source code,
and works on Windows, macOS, and Linux.

## LLVM compatibility

LLVM **22** remains the supported pipeline for all six opt-in passes.
The LLVM **23** API port is an experimental compatibility effort:
it builds and passes 16 unit tests and 6/6 startup probes,
but legal indirect-branch IR triggers an infinite CodeGenPrepare rewrite cycle
in stock LLVM 23.1.6. It is **not fully supported or qualified**.
See the [LLVM 23 experiment and portable reproducer](docs/DEVELOPMENT_HANDOFF.md#llvm-23-compatibility-experiment).
The experimental port is retained separately; this branch uses LLVM 22.

## Build and validate

Keep `lyra`, `lyra_wrapper`, and `lyra_linker` from the same source revision
and LLVM toolchain. Mixing binaries from different builds is unsupported.

From this checkout on macOS:

```sh
export RUSTUP_TOOLCHAIN=1.98.1
export LLVM_SYS_221_PREFIX="$(brew --prefix llvm@22)"
export LYRA_LLVM_BIN="$LLVM_SYS_221_PREFIX/bin"
export LYRA_EXE="$PWD/target/release/lyra"
cargo build --release --locked --bins
cargo test --release --locked --bins
```

Keep `lyra`, `lyra_wrapper`, and `lyra_linker` together in `target/release`.
Six passes are available and remain opt-in. With string coverage enabled,
`initialization_policy: 1` identifies the startup-safe policy: TLS initializers
and marked pre-constructor dependencies stay plaintext; other eligible
strings remain encrypted.

Before deployment, run the startup regression from this directory
(LLVM 22, Zig and Docker required; choose a new output directory):

```sh
python3 scripts/check_startup_policy.py --output target/startup-preflight
```

This executes real Linux fresh-thread and pre-constructor callback probes,
including all six passes. It does not establish compatibility with every
loader, host application, or platform; validate the resulting artifacts in
their intended runtime before distribution.

Historical measurements in the development handoff describe their recorded
inputs and environments, not a guarantee for other workloads or platforms.


## Project goals

The target is correct obfuscation with good runtime/build/resource performance,
reliable support for real Rust workspaces above **300,000 lines of Rust**, and
strong resistance to **LLM-led semantic recovery even with CPU emulators,
disassemblers and decompilers**. These are goals, not established capabilities.
Linux validation now includes ten selected libraries from a public **470,852-line
Rust workspace**, plus blinded recovery cases for branching, loops, mutable state,
and string processing. These scoped checks do not establish general scalability
or resistance. The opt-in indirect-call pass obscures selected static call targets,
but all four standalone cases were semantically recovered in the current
call-rich evaluation. Runtime plaintext secrecy and universal reverse-engineering
failure are not promised.
See the [acceptance pillars and execution order](ROADMAP.md#product-goal-and-acceptance-pillars).

## Features

- String encryption (`--string-enc`): XORs constant byte arrays with
  per-byte random keys and decrypts them in memory before `main` via a
  constructor in `llvm.global_ctors`. Thread-local initializers are excluded:
  a process constructor cannot decrypt the template for every future thread.
- Block outlining (`--block-outline`): moves at most one eligible acyclic,
  non-entry block per function into an internal `noinline` helper. Call-free
  regions need at least eight operations, at most six live-ins and two live-outs.
  LLVM handles PHIs and output slots; unsupported ABI/frame/control-flow shapes
  stay unchanged. Runs before indirect-call, which can obscure the new boundary.
  See the [subset and measured limits](docs/DEVELOPMENT_HANDOFF.md#block-outlining-pass-a1).
- Shuffle blocks (`--shuffle-blocks`): permutes the textual order of
  basic blocks inside eligible functions. Final layout and byte changes
  depend on LLVM's backend; a skipped function gains no protection.
- Indirect branch (`--indirect-branch`): rewrites direct `br`
  terminators so destinations are loaded at runtime from a per-function
  block-address table. `switch` is unchanged. Analysis impact depends on
  the workload, tool, and settings; it is not a universal CFG-recovery failure.
- Indirect call (`--indirect-call`): routes eligible calls to defined private/internal
  functions through seeded, encoded integer-table entries. Preserves the original
  call ABI and attributes; excludes unsupported call shapes and pointer layouts.
  This can obscure static call-graph edges, not prevent target recovery through
  memory inspection or execution. See the
  [supported subset and measurements](docs/DEVELOPMENT_HANDOFF.md#indirect-call-pass-s2).
- Mixed Boolean-Arithmetic (`--mba`): replaces scalar integer
  `add/sub/xor/and/or` (widths above one bit) using 15 identities,
  chosen at random per instruction **only when operands are proven defined**.
  Uncertain operands are left unchanged; no `freeze` workaround is introduced.
  Floating-point arithmetic is unchanged. See the
  [arithmetic contract](docs/DEVELOPMENT_HANDOFF.md#integer-mba-eligibility-audit).
- Per-build seed (`--seed <u64>`): deterministic per-pass RNG streams for
  fixed seed/module inputs, fresh OS entropy when unset. This does not
  guarantee artifact uniqueness on every build.

## Requirements

- A Rust toolchain that emits LLVM 22-compatible IR; this checkout is validated with **Rust 1.98.1**.
- LLVM 22: `llc` and `llvm-ar` must be discoverable. Lyra
  searches `LYRA_LLVM_BIN`, `LLVM_SYS_221_PREFIX`, an in-tree
  `llvm-project-22/build/bin`, and common package manager locations.
  Native regression probes additionally use LLVM 22 `clang`.
- Building Lyra additionally requires LLVM 22 C++ development headers/libraries,
  `llvm-config`, and an ABI-compatible C++17 compiler (`CXX` can select it).
  `build.rs` uses the exact LLVM installation selected by `llvm-sys` for the
  outlining shim; a C-only LLVM SDK is insufficient.

## Setup

macOS:

```bash
brew install llvm@22
export LLVM_SYS_221_PREFIX="$(brew --prefix llvm@22)"
```

Debian/Ubuntu (requires the apt.llvm.org repository):

```bash
apt install llvm-22-dev clang-22 build-essential
export LLVM_SYS_221_PREFIX=/usr/lib/llvm-22
```

Arch Linux:

```bash
pacman -S llvm
export LLVM_SYS_221_PREFIX=/usr
```

Windows: download the official LLVM 22.1.4 prebuilt bundle
(`clang+llvm-22.1.4-x86_64-pc-windows-msvc.tar.xz`) from the
llvm-project releases page, extract it to `llvm-project-22`, and place
an empty `xml2s.lib` in `llvm-project-22/build/lib`. Then:

```powershell
$env:LLVM_SYS_221_PREFIX = "$(Get-Location)\llvm-project-22\build"
```

This bundle uses the static MSVC runtime (`/MT`). The C++ outlining shim
and the repository's Rust target configuration use the same runtime;
mixing it with `/MD` produces `LNK2038` runtime mismatches and duplicate
C/C++ runtime symbols.

## Build

```bash
RUSTUP_TOOLCHAIN=1.98.1 cargo build --release --locked --bins
```

This produces three binaries in `target/release`: `lyra`, `lyra_wrapper`,
and `lyra_linker`. All three must stay in the same directory.

## Usage

```bash
lyra --project ./test_project \
     --output ./test_obf \
     --target x86_64-pc-windows-msvc \
     --string-enc --block-outline --shuffle-blocks --indirect-branch --indirect-call --mba
```

`--target` defaults to the host triple. Supported targets:
`x86_64-pc-windows-msvc`, `x86_64-pc-windows-gnu`,
`x86_64-pc-windows-gnullvm`, `x86_64-apple-darwin`,
`aarch64-apple-darwin`, `x86_64-unknown-linux-gnu`,
`aarch64-unknown-linux-gnu`.

| Flag | Effect |
|---|---|
| `--project <DIR>` | Cargo project to build. |
| `--output <FILE>` | Where to write the obfuscated binary. |
| `--target <TRIPLE>` | Target triple. Default: host. |
| `--crate-type <KIND>` | `bin`, `cdylib`, `dylib`, or `staticlib`. Default `bin`. |
| `--bin <NAME>` | Which `[[bin]]` to obfuscate in multi-bin projects. |
| `--obfuscate-crate <CRATE>` | Select Rust crates to intercept (repeatable). Hyphens normalize to underscores. An explicit list replaces the default bin/cdylib selection. |
| `--string-enc` | Enable string encryption. |
| `--block-outline` | Extract one eligible acyclic region per function into an internal helper. Opt-in, with bounded live values and conservative exclusions. |
| `--shuffle-blocks` | Enable basic-block shuffling. |
| `--indirect-branch` | Enable indirect-branch obfuscation. |
| `--indirect-call` | Encode selected local call targets and dispatch indirectly. Opt-in; external/interposable and unsupported calls remain unchanged. |
| `--mba` | Enable Mixed Boolean-Arithmetic substitution. |
| `--seed <U64>` | Master RNG seed for reproducible output. |
| `--keep-temps` | Keep intermediate files for debugging. |

## How it works

Cargo invokes rustc once per crate. As `RUSTC_WRAPPER`, Lyra asks selected
target crates to emit textual LLVM IR alongside their normal outputs. Library
objects are transformed, lowered directly to object files by `llc`, then swapped into
their `.rlib` archives with `llvm-ar`. For binaries and shared libraries,
`lyra_linker` triggers transformation after rustc emits IR but before the
real linker runs. It substitutes the generated object without compiling the
crate a second time. Mixed `cdylib`/`rlib` crates have both outputs replaced.
Host build dependencies are not intercepted: the rustc invocation must carry
the explicit deployment target, even when the same crate is used on both sides.

Transformation happens after monomorphization and the Rust LLVM optimiser, but
later code generation/linking can still simplify or merge changes. Verify final
machine-code effects rather than assuming every IR change survives. LTO is off
and codegen units are one so each crate has one main object to substitute.

When embedded in an existing Cargo pipeline, the wrapper preserves rustc's
explicit `-C linker` selection, including cargo-zigbuild's target/sysroot driver.
Repeated codegen options follow rustc's last-option precedence.
`LYRA_REAL_LINKER` is the fallback when Cargo supplied no explicit linker;
substituting a host linker must not discard a deployment floor.
On Unix hosts, arguments reach the original driver directly. Wrapping them in
a new response file would bypass cargo-zigbuild's export-definition adaptation.
Native Windows retains response files for its command-line length limit.
For Windows GNU/LLVM builds, the Rust target name `x86_64-pc-windows-gnullvm`
is translated to LLVM's `x86_64-pc-windows-gnu` ABI spelling during
object generation. Cargo still selects the original Rust target and linker.
Object lowering also preserves separate function/data sections so the final
linker's garbage collection can discard unreachable code and literals.
Without both, unrelated release data can survive even when runtime outputs match.
Direct object emission avoids an assembly round trip: LLVM's COFF assembly
printer can leak an imported-global symbol prefix into a section directive that
the assembler rejects. The regression includes that imported-data case.

Enabled passes run in this order: string encryption → block outlining → block
shuffle → indirect branch → indirect call → MBA. All flags default to off.

The optional `LYRA_AUDIT_SCRIPT`/`LYRA_AUDIT_PYTHON` hook lets an external
controller inspect original compiler archives and linker inputs before
replacement. A controller can retain them for independent validation without
disabling string encryption. The hook contains no application-specific
policy, and ordinary standalone Lyra runs do not require Python.

Functions that can run before global constructors must carry
`#[unsafe(link_section = ".text.lyra.preinit")]`. String encryption preserves
the transitive data dependencies of these roots, following defined helpers,
global pointer tables and constant expressions within each LLVM module.
Annotate each cross-module pre-constructor entry on its definition; use
`#[inline(never)]` on that boundary so callers do not erase its annotation.
Unmarked post-initialization data remains eligible for encryption.
Coverage reports `thread_local` and `preinit_dependency` exclusions separately.
The native runtime fixtures `tests/string_tls.c` and `tests/string_preinit.c`
check fresh-thread data and access through a callback before decryptor startup.

## CI and regression checks

GitHub Actions builds and exercises Lyra natively on Linux x86-64, macOS
ARM64, and Windows x86-64 MSVC. Pull requests and pushes to `main` run the
smoke suite; scheduled and manually dispatched runs exercise individual
passes and the combined pipeline at multiple fixed seeds.

CI pins Rust **1.98.1** and LLVM **22**. Rust **1.99** and newer nightly
compilers can emit LLVM 23 IR, which this LLVM 22 pipeline does not support.
All three sibling binaries are
built together. Formatting and Cargo tests run before the runtime checks.
The integer MBA unit regressions execute transformed IR through LLVM's native
JIT: undef/selected/frozen-undef cases, exhaustive pairs at widths 1/2/3/4/8,
and boundary pairs at widths 16/32/64/128. Width 1 remains untransformed.
The indirect-branch JIT regression covers source volatile memory updates, loop
PHIs, repeated calls, recursion, a bare entry branch, and `musttail` adjacency.
Indirect-call native tests cover C/fastcc conventions, narrow integer extension,
floating arguments/results, aggregate/sret/byval behavior, caller-owned memory,
recursion, normal tail calls, and conservative exclusions. The CLI fixture
executes the same ABI-sensitive path at seeds 0/1/42.
Outlining adds native PHI/live-in/live-out, integer/FP, volatile/atomic memory,
byval isolation, recursion, poison masking, cycle and frame-sensitive exclusions.
Its CLI fixture requires four real helper extractions and, in composed profiles,
four encoded helper calls. Linux verification passes **16 Cargo tests** and
**78 native CLI cases**; Apple Silicon macOS verification passes **15 Cargo
tests** and the same **78 native CLI cases**. Executable JIT fixtures use the
native target triple and data layout, including Mach-O symbol mangling.
These workstation results do not establish native Windows correctness.
The backend and CLI regressions also execute a CFG whose two block names share a
1,024-byte prefix. Lyra preserves full local IR names in its reader and `llc`;
manual LLVM text-IR verification should likewise use
`opt --non-global-value-max-name-size=-1 -passes=verify -disable-output input.ll`.

The runtime checks copy `test_project`, `test_dll_project`, and
`test_workspace_project` into isolated directories, build plain references, and
compare actual execution against explicit expected results and obfuscated
artifacts. They verify LLVM
IR, encrypted-string absence on disk, constructor-time decryption, and
typed calls across the shared-library boundary. The shared-library probe
checks runtime-supplied `f32`/`f64` addition, subtraction, and multiplication
at overflow, cancellation, rounding, signed-zero, subnormal, infinity, and
NaN boundaries. Non-NaN results must match exact bits; NaNs are checked by
classification, not payload/sign. The two-crate workspace exercises a mixed
`cdylib`/`rlib` library also used as a host build dependency, hyphenated target
names, explicit hyphen/underscore selections, and library-only encryption:
selected canaries disappear from disk while unselected canaries and all runtime
output remain intact. Build success alone is not a passing runtime check.

Run the same checks locally after installing the prerequisites:

```sh
rustup toolchain install 1.98.1 --profile minimal --component rustfmt
export RUSTUP_TOOLCHAIN=1.98.1
# Set LLVM_SYS_221_PREFIX to your LLVM 22 installation.
cargo fmt --all -- --check
cargo build --release --locked --bins
cargo test --release --locked --bins
# Keep copied Cargo fixtures outside this checkout's workspace.
# Choose a new absolute output directory for each run.
python3 scripts/ci_smoke.py --suite full --lyra "$PWD/target/release/lyra" \
  --output /tmp/lyra-native-check
```

On Windows use `python` and PowerShell environment syntax
(`$env:RUSTUP_TOOLCHAIN = "1.98.1"`). Use `--suite full` for each pass
alone and all passes together at seeds 0, 1, and 42. The output directory
must be new. `report.json`, `junit.xml`, command logs, and failed build
intermediates support diagnosis; hosted jobs upload bounded failure
evidence and reports even when a check fails. Local execution verifies
only the host platform, not the other CI targets.

## Measuring existing passes

`scripts/measure_passes.py` reuses the native CI runner, public fixtures, and
correctness oracles. Its default matrix covers plain compilation, no-pass Lyra,
each pass, indirect-branch + MBA, indirect-call + MBA, outline + indirect-call,
and all six passes at seeds 0/1/42. Plain is unseeded. Older retained “all-pass”
reports used four or five flags; their recorded flag sets, not the current
profile label, define those artifacts.
It retains binaries, original/transformed IR, stripped copies, section sizes,
command logs, source snapshots/hashes, and raw repeated samples in `report.json`
under `measurement`; `junit.xml` records execution failures.

Build an optimized Lyra for meaningful build-cost measurements:

```sh
export RUSTUP_TOOLCHAIN=1.98.1
# Set LLVM_SYS_221_PREFIX or LYRA_LLVM_BIN to LLVM 22.
cargo build --locked --release --bins
python3 scripts/measure_passes.py --output "$(pwd)/target/measurement-local"
```

LLVM `llvm-size`, `llvm-nm`, and `llvm-strip` are also required. Output must be
new. Defaults are three cold-target builds per profile, two runtime warmups,
and nine measured samples. Cargo downloads stay warm/offline; OS caches are not
flushed. Profile order is reproducibly shuffled. Runtime processes use one
available CPU where affinity is supported; build affinity is restored.
All profiles use LTO off and one codegen unit, so “plain” means controlled
no-Lyra compilation, **not** the project's stock fat-LTO configuration.

The executable fixture supplies wrapping integer/control-flow, public-text
scanning, and length-1024 FFT workloads. Runtime operands and independent Python
checksums prevent a passing unexecuted or constant-folded example. Native
`work_ns` and end-to-end process wall time are separate; startup includes
spawning, loading, parsing, and output. The shared-library probe's wall time also
includes Python and ABI checks, not isolated load/call latency. The FFT benchmark
explicitly canonicalizes signed zero for hashing; the strict float regression
oracle is unchanged. No tolerance is used for nonzero FFT results.

Timed builds disable coverage collection. A separate untimed build collects
`LYRA_COVERAGE=1` JSON records and must produce identical transformed IR for the
same inputs/seed. Coverage units are pass-specific:

| Pass | Coverage unit and limits |
|---|---|
| String encryption | Globals in the initial snapshot; one transformed global even if several struct fields are encrypted. Extra array/field/byte counts are retained. |
| Block outlining | Original basic blocks at pass entry; at most one eligible region per original function is selected. Generated extraction blocks/helpers are excluded from its denominator; unselected eligible blocks report `selection_budget`. |
| Block shuffle | Functions with bodies; an eligible identity permutation is skipped, not credited as transformed. |
| Indirect branch | Direct `br` terminators only; `switch` and other terminators are outside the denominator. |
| Indirect call | Ordinary `call` instructions only; `invoke`/`callbr` are outside the denominator. Defined private/internal targets with supported integral pointer layouts are eligible; ABI/target/layout exclusions are separate skip reasons. |
| MBA | Visits to add/sub/xor/and/or instructions. Use `rounds[0]` for original opportunities at MBA entry; totals also count surviving/generated instructions revisited in round two. Unproven operands, i1, vectors, exclusions, and random nonselection are separate reasons. |

For automated IDA inspection, add `--ida-python /path/to/venv/bin/python
--ida-dir /path/to/ida-pro-9.1` after setting up the [local exporter](#local-ida-inspection-on-linux).
Selected seed-0/plain artifacts are inspected **after** timing, using fresh
stripped copies and numeric entry addresses. Source correspondence stays in a
separate file; dynamic-library ABI export names may remain in the binary.
The report records actual CFG blocks/edges, pseudocode availability, errors,
settings, and tool versions. These are scoped observations, not protection scores.
`--profiles`, `--fixtures`, `--seeds`, and sample-count options allow focused runs.

## Local IDA inspection on Linux

`scripts/export_ida.py` uses the installed IDA 9 **direct idalib API**, not an
MCP bridge. It exports a function list, assembly, recovered CFG blocks/edges, and
selected Hex-Rays pseudocode to `analysis.json` and `analysis.txt`. Tool versions,
input/configuration hashes, analysis settings, errors, and the fixed 600-second
worker limit are recorded.

Use a licensed installation with previously accepted terms and a Python version
compatible with its IDAPython build. IDA 9.1 / Python 3.11.16 was exercised:

```sh
IDA_DIR=/path/to/ida-pro-9.1
uv venv --python python3.11 target/ida-python
uv pip install --python target/ida-python/bin/python "$IDA_DIR/idalib/python"
target/ida-python/bin/python scripts/export_ida.py /path/to/fixture.so \
  --ida-dir "$IDA_DIR" --output target/ida-fresh --function function_name
```

The output directory must be new; repeat `--function` to select more functions.
It accepts names or hexadecimal entry addresses such as `--function 0x1230`;
numeric selection supports stripped inputs without injecting source names/types.
Inputs are copied for disposable analysis. An existing `ida.reg` is copied into
a private temporary profile to inherit prior terms acceptance, then deleted;
license files and user settings are not modified. Use `--ida-user-dir` if the
accepted profile is not at `IDAUSR` or `~/.idapro`. Raw native console output is
not exported because it may include license metadata. Analysis failure or timeout
returns nonzero. Decompilation is inspection evidence, not semantic verification.

## Roadmap

See ROADMAP.md for current pass status, correctness gates, proposed techniques,
and evidence-based development milestones.

The [B2 switch-lowering experiment](docs/DEVELOPMENT_HANDOFF.md#custom-switch-lowering-experiment-b2)
was not promoted: its late placement passed scoped cost checks, but all eight
blinded semantic-recovery conditions were fully recovered. Production still
has six opt-in passes; there is no `--switch-lower` flag.

The [B3 address-offset experiment](docs/DEVELOPMENT_HANDOFF.md#address-offset-materialisation-experiment-b3)
was rejected after all twelve native text-growth gates failed. Its scoped
correctness checks passed; no `--address-offset` flag was added.

The [B4 byte-alias experiment](docs/DEVELOPMENT_HANDOFF.md#byte-alias-access-experiment-b4)
also failed all twelve text-growth gates despite scoped native and LLVM
refinement checks. No `--alias-access` flag was added.

The [C1 parameter-bundle experiment](docs/DEVELOPMENT_HANDOFF.md#parameter-aggregation-experiment-c1)
passed scoped correctness and cost gates, but binary-only analysis recovered
every tested added bundle relationship. No `--param-aggregate` flag was added.

The [C2 register-lane experiment](docs/DEVELOPMENT_HANDOFF.md#register-lane-calling-convention-experiment-c2)
passed native register/stack checks but failed two composition runtime gates.
No `--register-cc` flag was added; the LLVM register bank is version-unstable.

The [D2 constant-reconstruction experiment](docs/DEVELOPMENT_HANDOFF.md#constant-reconstruction-experiment-d2)
passed native checks and size gates but failed four runtime gates. LLVM
refinement also remains incomplete; solver timeouts are not protection.
No `--constant-lift` flag was added.

The [D3 quadratic-predicate experiment](docs/DEVELOPMENT_HANDOFF.md#quadratic-residue-predicate-experiment-d3)
passed scoped native and LLVM checks, but binary-only analysis simplified every
tested added predicate and selector. It stopped before an S1 rewriter or new
production flag; this was not a whole-program recovery trial.

The [D1 pipeline-order experiment](docs/DEVELOPMENT_HANDOFF.md#bounded-pipeline-order-experiment-d1)
passed scoped correctness and cost gates. A complete schema-parsed recovery
rerun recovered 15/16 conditions at each endpoint, including every six-pass
condition; it did not establish an incremental benefit. Exact model identity
remains unavailable. Production keeps the fixed six-pass order; no
pipeline-randomization API was added.

The [function-layout experiment](docs/DEVELOPMENT_HANDOFF.md#function-layout-experiment-y2)
changed native placement, but anonymous analysis matched every tested function
body. It was rejected before runtime/locality or scale claims; no layout flag
was added.

The [S3 register-VM experiment](docs/DEVELOPMENT_HANDOFF.md#bounded-register-vm-experiment-s3)
executed real bytecode and passed scoped native/refinement checks, but failed
the composition text-growth gates. No VM flag or `#[lyra::vm_protect]` API was
added; hot runtime cost and cold-code amortization were not measured.

For the Linux workstation setup and next correctness work, see the
[development handoff](docs/DEVELOPMENT_HANDOFF.md).

## License

Lyra is licensed under the **GNU General Public License, version 3 or later**
(`GPL-3.0-or-later`). See [LICENSE](LICENSE) for the complete license text.

The [generated-output exception](LICENSE-EXCEPTION.md) allows independent
programs processed by Lyra to retain their own licenses, including runtime
support code emitted or inserted by Lyra. It does not exempt Lyra itself
or distributed modifications of Lyra from GPL requirements.
