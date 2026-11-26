# Maintainer guide

## Project map

- `src/main.rs`: CLI, Cargo orchestration, and fixed pass pipeline.
- `src/bin/lyra_wrapper.rs`: target-crate interception and archive replacement.
- `src/bin/lyra_linker.rs`: transformation at the real linker boundary.
- `src/obfuscator/`: six passes, eligibility helpers, seeds, and native unit tests.
- `src/obfuscator/block_outline.cpp`, `build.rs`: LLVM C++ outlining bridge.
- `scripts/build_llvm.py`, `patches/`, `tests/llvm23_*`: pinned SDK and compiler regressions.
- `scripts/setup_ci.py`, `.github/workflows/ci.yml`: native CI environments and jobs.
- `scripts/ci_smoke.py`: executable, shared-library, workspace, and ABI regressions.
- `scripts/check_startup_policy.py`, `tests/string_tls.c`, `tests/string_preinit.c`: startup safety.
- `test_project/`, `test_dll_project/`, `test_workspace_project/`: portable Cargo fixtures.
- `scripts/measure_passes.py`: optional cost/coverage measurements, not a correctness substitute.

## Toolchain and compiler patch

Use Rust **1.100.0**, Python **3.12+**, and the prepared LLVM **23.1.6** SDK.
Mixed LLVM versions are unsupported. `LLVM_SYS_231_PREFIX` selects
libraries and headers; `LYRA_LLVM_BIN` must be that same prefix's `bin` directory.
The outlining bridge must use those C++ headers and an ABI-compatible compiler.
Keep all three Lyra executables together and from the same build.

Stock LLVM 23.1.6 can cycle in CodeGenPrepare while unmerging static-alloca GEPs
across `indirectbr`. The supplied patch rejects that unprofitable case, retaining
non-static-base unmerging. Keep the reduced witness and non-static control in
`tests/llvm23_*`. Do not bypass a failure by disabling CodeGenPrepare, lowering
optimization, removing a pass, or substituting an unqualified stock toolchain.
SDK changes require compiler regressions and native Linux, macOS, and Windows
runtime checks; IR verification or a successful Rust build alone is insufficient.

Prefer `--base-prefix` with an exact stock LLVM/Clang 23.1.6 release SDK built
with assertions off. Include installed/generated headers, static LLVM archives,
`llvm-config`, `llvm-ar`, and `llvm-readobj`. Install the system libraries reported
by `llvm-config --link-static --system-libs` and use a native C++ compiler.
The builder prepares a separate private SDK and rebuilds affected code
generation in `llc` and the static LLVMCodeGen library. Consumers must relink
statically; Lyra does so. Stock shared libraries remain for `clang` and
`opt`, whose code generation is not patched. The builder checks that patched
`llc` has no shared LLVM dependency.

Omit `--base-prefix` for a full-source build, which also requires CMake and Ninja.
Use `--source-archive` for a local source archive; the builder verifies it.
Never patch a shared stock installation in place.

## Build and checks

Install rustup, Python 3.12+, Git, CMake, Ninja, and native C++/platform SDK tools.
Replace the placeholders with absolute SDK prefixes and a new absolute output
directory outside the checkout. On macOS, first set
`export SDKROOT="$(xcrun --sdk macosx --show-sdk-path)"`. From the repository root:

```sh
rustup toolchain install 1.100.0 --profile minimal --component rustfmt
export RUSTUP_TOOLCHAIN=1.100.0
export LLVM_SYS_231_PREFIX="<private-prefix>"
python3 scripts/build_llvm.py --base-prefix "<stock-prefix>" \
  --prefix "$LLVM_SYS_231_PREFIX" --jobs 4
export LYRA_LLVM_BIN="$LLVM_SYS_231_PREFIX/bin"
cargo fmt --all -- --check
python3 -m unittest discover -s scripts -p 'test_*.py'
cargo build --release --locked --bins
cargo test --release --locked --bins
python3 scripts/ci_smoke.py --suite smoke --lyra "$PWD/target/release/lyra" \
  --output "<new-absolute-output-directory>"
```

For release qualification, use `--suite full` in a fresh output directory.
Smoke covers no-pass and combined profiles; full adds individual passes and
seeds 0, 1, and 42. Run native jobs for Linux x86-64, macOS ARM64, and Windows
x86-64 MSVC. A result on one host does not qualify another target or artifact type.
Preserve `report.json`, `junit.xml`, logs, and failing intermediates for diagnosis;
review retained artifacts before publishing them for paths or private inputs.

Startup-policy checks additionally require Zig and Docker with Linux containers:

```sh
python3 scripts/check_startup_policy.py --lyra "$PWD/target/release/lyra" \
  --output "<new-absolute-startup-directory>"
```

These are Linux fresh-thread and pre-constructor probes, including all six passes,
not native Windows or macOS loader qualification. To verify retained textual IR:

```sh
"$LYRA_LLVM_BIN/opt" --non-global-value-max-name-size=-1 \
  -passes=verify -disable-output input.ll
```

## Windows build environment

Use an x64 Visual Studio developer PowerShell with native build tools and the
Windows SDK; `LIB` must contain its real library directories. Prepare the SDK with
`python scripts/build_llvm.py --base-prefix "<stock-prefix>" --prefix "<private-prefix>"`.
Before Cargo commands:

```powershell
$env:RUSTUP_TOOLCHAIN = "1.100.0"
$env:LLVM_SYS_231_PREFIX = "<private-prefix>"
$env:LYRA_LLVM_BIN = "$env:LLVM_SYS_231_PREFIX\bin"
$flags = @("-C", "target-feature=+crt-static", "-Lnative=$env:LLVM_SYS_231_PREFIX\lib")
$flags += ($env:LIB -split ";" | Where-Object { $_ } | ForEach-Object { "-Lnative=$_" })
$env:CARGO_ENCODED_RUSTFLAGS = $flags -join [char]0x1f
```

LLVM, Rust, and the outlining shim must agree on the static MSVC CRT (`/MT`).
Explicit native search paths let llvm-sys find SDK import libraries; encoded
flags preserve spaces. Use `python`, `.exe` paths, and PowerShell path syntax for
the checks above. `scripts/setup_ci.py` configures the corresponding GitHub runner
environment; it is not a general local installer.

## Invariants

- Pass order is string encryption, outlining, block shuffle, indirect branch,
  indirect call, then MBA. All are opt-in. Ordering changes need composition tests.
- Preserve PHIs, dominance, ABI attributes, unwind behavior, volatile/atomic
  effects, pointer semantics, and constructor ordering. Unsupported IR stays intact.
- MBA requires proven-defined scalar integer operands wider than one bit. Do not
  add floating-point rewrites or use `freeze` to broaden eligibility without proof
  and optimized native regressions. Preserve defined behavior, not poison outcomes.
- Transform only explicitly targeted rustc invocations. Host build dependencies
  must stay host code even when the same crate is selected for the deployment target.
- Preserve explicit `-C linker` selection and last-option precedence; use
  `LYRA_REAL_LINKER` only as fallback. Retain target/sysroot/deployment-floor flags.
  Pass Unix arguments directly to the driver; retain Windows response-file support.
- Emit objects directly, preserving separate function/data sections. Keep Cargo's
  `gnullvm` target selection while using LLVM's Windows GNU ABI spelling for lowering.
- Keep LTO off and one codegen unit for the main-object substitution contract.
  Mixed `cdylib`/`rlib` outputs need both replacements. `staticlib` is pass-through.
- Preserve full LLVM local names, including names sharing a 1,024-byte prefix.
- String encryption excludes TLS templates and marked pre-constructor dependencies.
  Annotate each cross-module pre-constructor entry definition with
  `#[unsafe(link_section = ".text.lyra.preinit")]` and `#[inline(never)]` so its
  boundary survives. Preserve transitive helpers/data; ordinary strings stay eligible.
  Coverage distinguishes `thread_local` and `preinit_dependency` exclusions and
  reports `initialization_policy: 1` for this policy.
- `LYRA_AUDIT_SCRIPT`/`LYRA_AUDIT_PYTHON` is optional inspection of original inputs.
  Do not make normal standalone operation depend on an audit controller or Python.

## Test expectations

Add a portable regression for each behavior change. Execute transformed code and
compare plain references with explicit expected results; a build or IR verifier
pass is not semantic verification. Check ABI, constructor/TLS behavior, selected
archive replacement, and plaintext canaries where relevant. Floating results must
match non-NaN bits, including signed zero; compare NaNs by classification.
Exercise individual and composed passes at fixed seeds. Do not weaken assertions,
silently skip selected crates, or count unchanged functions as transformed coverage.
New eligibility needs both LLVM-semantic reasoning and optimized backend tests.
Measure final code and cost separately from claims about analysis resistance.

## Contribution workflow

Follow [CONTRIBUTING.md](CONTRIBUTING.md) for branch, title, review, and merge rules.
The policy job runs `python scripts/check_contribution.py` with the GitHub event
environment and a full-history checkout. It checks PR titles and every new commit
subject; push events check the pushed head commit.

After merging, inspect active PRs and worktrees before deleting merged branches.
Never remove a branch still used by an open PR or active worktree.
