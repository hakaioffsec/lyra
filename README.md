# lyra

Lyra is an LLVM IR obfuscator for Rust. It runs against an unmodified
rustc and cargo, requires no changes to the target project's source code,
and works on Windows, macOS, and Linux.

## Features

- String encryption (`--string-enc`): XORs constant byte arrays with
  per-byte random keys and decrypts them in memory before `main` via a
  constructor in `llvm.global_ctors`.
- Shuffle blocks (`--shuffle-blocks`): permutes the textual order of
  basic blocks inside each function. Breaks byte-pattern signatures.
- Indirect branch (`--indirect-branch`): rewrites direct `br`
  terminators so destinations are loaded at runtime from a per-function
  block-address table. Decompilers fail to recover the control flow
  graph of transformed functions.
- Mixed Boolean-Arithmetic (`--mba`): replaces integer and float
  arithmetic with algebraically equivalent expressions. 15 integer
  identities plus 9 float identities, chosen at random per instruction.
- Per-build seed (`--seed <u64>`): deterministic obfuscation when set,
  fresh OS entropy and a different binary on every build when unset.

## Requirements

- Rust, stable or nightly.
- LLVM 22: `llc`, `clang`, and `llvm-ar` must be discoverable. Lyra
  searches `LYRA_LLVM_BIN`, `LLVM_SYS_221_PREFIX`, an in-tree
  `llvm-project-22/build/bin`, and common package manager locations.

## Setup

macOS:

```bash
brew install llvm@22
export LLVM_SYS_221_PREFIX="$(brew --prefix llvm@22)"
```

Debian/Ubuntu (requires the apt.llvm.org repository):

```bash
apt install llvm-22 clang-22
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

## Build

```bash
cargo build --release
```

This produces three binaries in `target/release`: `lyra`, `lyra_wrapper`,
and `lyra_linker`. All three must stay in the same directory.

## Usage

```bash
lyra --project ./test_project \
     --output ./test_obf \
     --target x86_64-pc-windows-msvc \
     --string-enc --shuffle-blocks --indirect-branch --mba
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
| `--obfuscate-crate <CRATE>` | Crate to intercept. Repeatable. |
| `--string-enc` | Enable string encryption. |
| `--shuffle-blocks` | Enable basic-block shuffling. |
| `--indirect-branch` | Enable indirect-branch obfuscation. |
| `--mba` | Enable Mixed Boolean-Arithmetic substitution. |
| `--seed <U64>` | Master RNG seed for reproducible output. |
| `--keep-temps` | Keep intermediate files for debugging. |

## How it works

Cargo invokes rustc once per crate. Lyra places itself in front of that
process twice. As `RUSTC_WRAPPER` it intercepts each rustc call for the
crates selected for obfuscation, asks rustc to also emit textual LLVM
IR, transforms that IR out of process, and compiles it back to an
object file with `llc` and `clang`. For library crates the obfuscated
object is swapped into the `.rlib` archive with `llvm-ar`. For binaries
and shared libraries rustc runs a second time with `-C linker` pointed
at `lyra_linker`, which substitutes the obfuscated object before
invoking the real linker.

The transformation happens after monomorphization and after the LLVM
optimizer, so the passes survive to the final binary. LTO and
codegen-units are forced off and to one respectively so that each crate
has exactly one main object to substitute.

## Roadmap

See ROADMAP.md for planned passes, ranked by reverse-engineering impact
with effort and LOC estimates.

## License

GNU General Public License v3.0. See LICENSE.
