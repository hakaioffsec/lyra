# Lyra

Lyra applies opt-in LLVM IR obfuscation to selected Rust crates through Cargo,
without changing their source. It transforms compiler output, lowers it to
objects, and replaces the selected linker inputs or library archive members.
Obfuscation changes representation; it does not guarantee resistance to analysis
or keep runtime plaintext secret.

## Passes

All six passes default to off and run in this order:

| Flag | Transformation |
|---|---|
| `--string-enc` | Encrypt eligible byte-array globals; decrypt them at startup. TLS and marked pre-initialization dependencies remain plaintext. |
| `--block-outline` | Move at most one eligible acyclic block per function into an internal helper. |
| `--shuffle-blocks` | Shuffle non-entry blocks; final layout depends on code generation. |
| `--indirect-branch` | Replace eligible direct branches with block-address dispatch; leave switches unchanged. |
| `--indirect-call` | Route eligible private/internal calls through encoded pointer entries, preserving their ABI. |
| `--mba` | Rewrite scalar integer arithmetic with proven-defined operands; leave floating-point arithmetic unchanged. |

Unsupported constructs remain unchanged. `--seed <u64>` makes transformation
choices repeatable for fixed module inputs, not necessarily final artifact bytes.

## Build and install

Use **Rust 1.100.0** and patched **LLVM/Clang 23.1.6**. Stock LLVM needs the
CodeGenPrepare fix in `patches/` to avoid a rewrite cycle with static allocas
and indirect branches. Do not disable optimization passes to work around it.

Install rustup, Python 3.12+, Git, CMake, Ninja, and a native C++ compiler and
platform SDK. Prefer an exact stock LLVM/Clang 23.1.6 release SDK with assertions
off, static libraries, and development headers; see the
[SDK prerequisites](AGENTS.md#toolchain-and-compiler-patch). Replace the prefixes
below with separate absolute directories. On macOS, first set
`export SDKROOT="$(xcrun --sdk macosx --show-sdk-path)"`.

```sh
rustup toolchain install 1.100.0 --profile minimal --component rustfmt
export RUSTUP_TOOLCHAIN=1.100.0
export LLVM_SYS_231_PREFIX="<private-prefix>"
python3 scripts/build_llvm.py --base-prefix "<stock-prefix>" --prefix "$LLVM_SYS_231_PREFIX"
export LYRA_LLVM_BIN="$LLVM_SYS_231_PREFIX/bin"
cargo build --release --locked --bins
```

On Unix, keep the stock SDK installed: unchanged files are linked.

This reuses the stock SDK and rebuilds affected code generation in `llc` and
the private static LLVM library, not the stock `clang` or `opt` executables.
Omit `--base-prefix` for a full-source build; use `--jobs N` to limit parallelism.
Use the same private prefix for LLVM libraries, headers, and tools. Keep `lyra`,
`lyra_wrapper`, and `lyra_linker` from the same build together. Install all three
with `cargo install --path . --locked`.

On Windows, use an x64 Visual Studio developer shell with the Windows SDK.
LLVM, Rust, and the C++ shim must use the static CRT; Rust also needs explicit
SDK library search paths. See [AGENTS.md](AGENTS.md#windows-build-environment)
for PowerShell setup and the CI setup tool.

## Use

```sh
target/release/lyra --project ./test_project --output ./test_obf \
  --string-enc --block-outline --shuffle-blocks \
  --indirect-branch --indirect-call --mba --seed 42
```

Use `--bin <name>` for a multi-binary project, `--crate-type cdylib` for a C-ABI
shared library, and repeat `--obfuscate-crate <name>` to select dependency crates.
Explicit crate selection replaces the default selection; hyphens normalize to
underscores. `--keep-temps` retains intermediates; `--help` lists all options.

Linked `bin`, `cdylib`, and `dylib` outputs and selected `rlib` dependencies have
interception paths. `staticlib` is accepted but its archive is **not obfuscated**.
The runtime fixtures focus on executables, C-ABI shared libraries, and mixed
`cdylib`/`rlib` workspaces, not every artifact type.

`--target` defaults to the host. Target handling covers `x86_64-pc-windows-msvc`,
`x86_64-pc-windows-gnu`, `x86_64-pc-windows-gnullvm`, `x86_64-apple-darwin`,
`aarch64-apple-darwin`, `x86_64-unknown-linux-gnu`, and `aarch64-unknown-linux-gnu`.
Cross-compilation needs the target's Rust standard library, linker, and SDK.
Target handling is not evidence of native runtime qualification.

## Verify

With the build environment above, choose a new absolute output directory
outside the checkout for each smoke run:

```sh
cargo test --release --locked --bins
python3 scripts/ci_smoke.py --suite full --lyra "$PWD/target/release/lyra" \
  --output "<new-absolute-output-directory>"
```

Checks compare real execution with plain references and explicit expected
results. Validate artifacts in their intended runtime before distributing them.
See [AGENTS.md](AGENTS.md) for startup checks and maintenance,
[CONTRIBUTING.md](CONTRIBUTING.md) for contribution rules, and
[ROADMAP.md](ROADMAP.md) for remaining work.

## License

[GPL-3.0-or-later](LICENSE), with a [generated-output exception](LICENSE-EXCEPTION.md)
allowing independent processed programs, including inserted runtime support, to
retain their own licenses. The exception does not exempt Lyra or its modifications.
