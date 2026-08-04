# ROADMAP — Pass priority ranking

This document ranks upcoming obfuscation passes by how much pain they
impose on a reverse engineer working in IDA Pro, Ghidra, or Binary Ninja
with their decompilers enabled. The ordering is deliberately aggressive:
we want to pick the passes that give the **worst experience** to an
analyst per hour of implementation work.

## Threat model

We assume the reverse engineer has:

- A commercial disassembler (IDA 9, Ghidra, Binary Ninja) with the
  decompiler / Hex-Rays.
- `strings`, `objdump`, `dumpbin`, `pefile`, plus any YARA-style
  signature tooling.
- A debugger (WinDbg, x64dbg, remote gdb).
- Access to run the binary in an instrumented environment (TTD, Time
  Travel Debugging, DynamoRIO, PIN).

Obfuscation impact is ranked across four axes:

| Axis | Examples |
|---|---|
| **Static disassembly** | Can the linear sweep recover correct instruction boundaries? Are basic blocks identifiable? |
| **Decompilation** | Does Hex-Rays / Ghidra's decompiler produce readable pseudo-C? Can it simplify arithmetic? |
| **Cross-reference / CFG** | Can the tool resolve call targets, branch destinations, data references? |
| **Signature evasion** | Do per-build binaries match YARA rules, function hashes, compiler fingerprints? |

A pass gets a higher tier when it breaks more axes simultaneously.

---

## Already shipped

| Pass | Status | RE pain axis broken | YARA impact |
|---|---|---|---|
| String Encryption | Shipped, default-on recommendation | Static (strings), signature | Defeats all literal-string / regex-over-string rules |
| Shuffle Blocks | Shipped (Sprint 1 / A2) | Signature, static layout | Defeats intra-function hex-pattern rules per build |
| Indirect Branch | Shipped (Sprint 1 / S1) | Static CFG, decompilation | Replaces direct-branch byte patterns with indirect-dispatch ones |
| Per-build seed | Shipped (Sprint 1 / D1, `--seed`) | Signature (per-build uniqueness) | Without `--seed`, every build's hex patterns differ |

Sprint 1 verified together on a large internal Rust codebase:
- 9501 strings encrypted
- 14418 blocks shuffled across 1639 functions
- 15942 branches rewritten via indirect-branch across 1815 functions
- Runtime behaviour preserved; plaintext business-logic strings absent
  from the final binary

The earlier pre-rewrite `instruction_substitution`, `bogus_control_flow`,
and `control_flow_flattening` passes were **deleted** as part of the
clean-slate refresh. They shipped known-broken behaviour — notably a
name-based skip list in CFF (`name.contains("::encode::")`), shared-module
opaque predicates in BCF (defeatable with a single SMT query), and an
instruction-substitution pass whose statistics counters double-counted
probability-gated inspections as if they had been applied.

The tiers below are the replacements, ordered by reverse-engineer pain.
Each item is a greenfield rewrite, not an incremental patch, so that the
quality bar stays where `string_encryption` set it (regression-tested on
a real 80-crate Rust project, runtime preserved, 9501 strings verifiably
absent from the output).

---

## Tier S — "your disassembler is lying to you"

These passes cause the disassembler / decompiler to produce *confidently
wrong* output, or to fail outright. They are the highest priority.

### S1. Indirect Branch Obfuscation

**What it does:** Every direct `br label %X` inside selected functions
becomes a computed jump through a per-function dispatch table. The
`label -> index` map lives as an encrypted byte array; the index is
decoded at runtime with MBA.

**What RE sees:** The CFG reconstruction in IDA/Ghidra breaks for every
transformed function. Basic blocks appear as orphans because the
disassembler can't prove which block follows which; cross-references
inside the function disappear from `Xrefs to`. Hex-Rays decompilation
degrades to a single giant basic block containing the whole function,
usually full of `goto jumptable[...]` patterns it can't simplify.

**Implementation effort:** Medium. LLVM already has `indirectbr`; the
work is (a) numbering all block labels, (b) building a `BlockAddress`
array, (c) inserting a dispatcher, (d) encoding block choices in the
predecessor. Amice has a complete reference implementation in
`src/aotu/indirect_branch/mod.rs`.

**Estimated LOC:** 400–600 for a first cut; 800–1200 with the dispatch
table encryption.

**Dependencies:** None. Runs cleanly on any function but benefits from
running **after** split-basic-block (currently implicit inside BCF).

---

### S2. Indirect Call Obfuscation

**What it does:** Replace direct `call @foo` sites with calls through
an encrypted function-pointer table. The index into the table is
computed via MBA so it isn't a compile-time constant.

**What RE sees:** The decompiler shows `(*fptr)(...)` everywhere with no
resolvable target. IDA's "Xrefs from" and "function calls" graphs lose
nearly all edges for the obfuscated crate. Signature-based function
identification (FLIRT, lumina, function hashing) fails because callers
no longer have direct references. Binary diffing / BinDiff is
neutralised for call-graph-based matching.

**Implementation effort:** Medium. Slightly harder than S1 because we
need to handle variadic calls, inline asm call sites, and intrinsics
carefully (most intrinsics must NOT be indirected — e.g. `llvm.memcpy`,
`llvm.lifetime.*`). Amice's `indirect_call/mod.rs` has the whitelist
logic we can adapt.

**Estimated LOC:** 500–800.

**Dependencies:** None. Compose well with S1.

---

### S3. VM-based flattening

**What it does:** Replace a function's body with a bytecode interpreter.
The actual logic becomes an opcode stream; each opcode is dispatched by
a dispatcher loop reading a register file.

**What RE sees:** Decompilation becomes useless; the pseudo-C is a big
switch over opcodes with no business-logic visible. Every function
appears to do the same thing (run an interpreter). This is how
VMProtect, Themida, Oreans make static analysis infeasible.

**Implementation effort:** **HIGH**. This is a month-scale project, not
a week. Requires designing a small RISC-like VM, a bytecode encoder
from LLVM IR, a register allocator for the VM, and a dispatcher. Amice
has `vm_flatten/mod.rs` as reference, but the amice version is also
large and incomplete.

**Estimated LOC:** 3000–5000.

**Dependencies:** Benefits massively from S4 (outlining) run first so
only hot / sensitive functions get virtualised.

---

## Tier A — "your decompiler is noisy and wrong"

These passes don't outright break the decompiler, but they fill it with
such junk that an analyst gives up and moves on.

### A1. Basic-Block Outlining

**What it does:** Pick a subset of basic blocks (by opcode count or by
probability) and extract each into a private helper function. The
original block becomes a single `call @outlined_N` instruction.

**What RE sees:** Functions are now fragmented across dozens of tiny
helpers. Control flow in the decompiler becomes `foo()` → `outlined_1()`
→ `outlined_47()` → `outlined_12()`. Inlining heuristics in Hex-Rays
don't handle this well — the analyst has to manually inline each helper
to see the real logic. Combined with S1/S2, the inliner has nothing to
work with.

**Implementation effort:** Medium. LLVM's `CodeExtractor` class does
most of the work but exposes a brittle C++-only API; via `llvm-sys` we
can call into it. Amice's `basic_block_outlining/mod.rs` has the
selection heuristics (hotness, size) we can copy.

**Estimated LOC:** 400–700.

**Dependencies:** Split-basic-block (inside BCF) should run first; MBA
(B1) is a nice amplifier.

---

### A2. Shuffle Blocks

**What it does:** Permute the textual order of basic blocks within each
function. Unconditional branches are inserted as needed so semantics are
preserved; the *layout* changes completely.

**What RE sees:** Byte-for-byte different binaries per build (different
seed). YARA and function-hash signatures that rely on byte sequences or
block orderings are invalidated immediately. IDA graph view still works
but every entry in the "functions" tab has an unfamiliar prologue/shape.

**Implementation effort:** **LOW**. A 50-line `BasicBlock::move_before`
loop with a seeded RNG is enough for a first cut. This is the cheapest
tier-A pass.

**Estimated LOC:** 100–200.

**Dependencies:** None. Should run **last** in the pipeline so earlier
passes don't re-order the shuffled blocks.

---

### A3. Function Wrapper

**What it does:** Every call to a selected function is rewritten to go
through a generated trampoline of the form:

```
fn wrapper(arg0, arg1, ...) {
    // optional: juggle args through some arithmetic
    real_function(arg0, arg1, ...)
}
```

Combined with `-C linker-plugin-lto=no` (which we already force) the
wrappers are not inlined.

**What RE sees:** Every call looks like it's going through a bouncer.
Static call-graph analysis explodes in size. Binary diffing tools
produce huge false-positive change reports because every function has
a new wrapper sibling.

**Implementation effort:** Low-medium. Amice's `function_wrapper/mod.rs`
is ~200 lines.

**Estimated LOC:** 200–400.

**Dependencies:** None.

---

### A4. Clone Function + Const Specialization

**What it does:** Pick N functions; for each, emit `K` copies with
slightly different instruction schedules, permuted constants, and
sometimes specialised on a known argument value. Each call site picks
a random clone.

**What RE sees:** Function-hash-based signatures (e.g. Rizin's zignature
database, Ghidra's BSim, IDA's lumina) produce different hashes for each
clone. Analysts who've reversed one copy can't propagate their notes to
the others because the decompiler output differs. Per-build diversity
is very high.

**Implementation effort:** Medium. Amice's `clone_function/mod.rs` is a
~300 line module.

**Estimated LOC:** 300–500.

**Dependencies:** None; multiplicative gains when combined with S1/S2.

---

## Tier B — "the analyst wastes hours on a single function"

### B1. Mixed Boolean-Arithmetic (MBA) — advanced

**What it does:** Replace integer arithmetic and comparisons with
equivalent expressions drawn from a library of MBA identities. We
already have a basic version under `--substitution` that covers six
opcodes with a small identity set; the advanced version works on
expression trees, not single ops, and has dozens of identity templates.

**What RE sees:** Simple arithmetic like `a + b` expands into things
like `(a ^ b) + 2*(a & b)` and nests several levels deep. Hex-Rays's
arithmetic simplifier (`y-optimize`) collapses simple MBA patterns but
the random-template approach defeats its fixed-rule engine. Analysts
have to run an SMT-assisted deobfuscator (msynth, qsynthesis) on every
expression they want to understand.

**Implementation effort:** Medium-high. Amice's `mba/mod.rs`,
`mba/expr.rs`, `mba/generator.rs` and `mba/binary_expr_mba.rs` together
are ~1500 lines. They implement an expression-tree rewriter with per-
op identity selection and depth budgeting.

**Estimated LOC:** 800–1500 for a first cut; more if we also do
constant-obfuscation MBA (amice's `constant_mba.rs`).

**Dependencies:** None.

---

### B2. Lower Switch (custom switch lowering)

**What it does:** Replace LLVM `switch` instructions with a custom
dispatcher that doesn't look like a jump table. Options: if-else chain
with per-branch opaque predicates, perfect hash, range tree.

**What RE sees:** IDA's switch-table recovery heuristics fail. The
decompiler can't produce a C `switch` statement; you get a cascade of
`if` / `else if` with arithmetic on the scrutinee that IDA can't prove
is a case dispatch.

**Implementation effort:** Medium. LLVM has `LowerSwitch` as a stock
transform; amice customises it in `lower_switch/mod.rs`.

**Estimated LOC:** 300–500.

**Dependencies:** Should run before CFF or BCF so those see the lowered
form.

---

### B3. Delayed Offset Loading (AMA — "arithmetic materialisation of addresses")

**What it does:** Replace compile-time-known constant offsets, global
addresses, and vtable indices with small arithmetic expressions that
evaluate to the same value at runtime. So instead of
`mov rax, [rip+0x1234]` you get
`mov rax, rbx; xor rax, 0xA9C0; sub rax, 0xBB34; mov rax, [rip+rax]`.

**What RE sees:** No `DATA XREF` lines in the disassembly for the
obfuscated constants. Global variables aren't listed under "strings"
or "data references". Function tables, enum dispatch tables, and
vtable entries stop being visible as clean tables.

**Implementation effort:** Medium. Amice has
`delay_offset_loading/mod.rs`. Requires careful handling to not break
position-independent code / relocations.

**Estimated LOC:** 400–700.

**Dependencies:** Interacts with A1 (outlining can hide the
materialisation); run late in the pipeline.

---

### B4. Alias-Access Obfuscation

**What it does:** Take loads/stores of a pointer P and re-express them
via aliases: allocate a second pointer Q that's derived from P by an
invertible transformation (xor with a constant, add-then-sub, pointer
arithmetic through a struct GEP) and route the access through Q. Vary
the alias per use.

**What RE sees:** Points-to analysis in Hex-Rays (which drives
struct-field recognition and array-index recognition) collapses. What
was a clean `p->field.sub[i]` becomes `*(unsigned __int64 *)(v3 +
v7 + 8)`.

**Implementation effort:** Medium. Amice's `alias_access/mod.rs` plus
`alias_access/pointer_chain.rs` is ~800 LOC.

**Estimated LOC:** 600–900.

**Dependencies:** None.

---

## Tier C — "small but compounding"

### C1. Parameter Aggregation

**What it does:** For functions taking several scalar args, pack them
into a heap / stack struct at the call site and unpack inside. The
calling convention (the `.text` and register usage pattern) changes.

**What RE sees:** Function signatures in the decompiler look wrong —
instead of `int f(int a, int b, int c)` you get `int f(struct_47 *)`.
Every call site looks like it's constructing an object. FLIRT-style
signatures fail because the function prologue changes.

**Implementation effort:** Medium. Amice's `param_aggregate/mod.rs` is
straightforward.

**Estimated LOC:** 300–500.

---

### C2. Custom Calling Convention

**What it does:** Replace the standard SysV / MSVC x64 calling
convention on selected functions with a shuffled one — swap argument
registers, pass some via stack that would normally go in registers,
etc. Needs coordinated changes at both call sites and callee.

**What RE sees:** Hex-Rays's function-argument recovery fails. The
pseudo-C shows the wrong number or type of arguments. Binary
differencing breaks because prologues / argument use differs across
builds.

**Implementation effort:** Medium-high. LLVM exposes calling
conventions (`CallingConv::*`) but custom ones need target-specific
tweaks. Amice marks this as ⏳ (in progress).

**Estimated LOC:** 500–800.

---

### C3. Instruction-Substitution — advanced

**What it does:** Extend the current `--substitution` to cover more
opcodes (shifts, comparisons, select), more templates per opcode, and
depth-budgeted nested substitutions.

**What RE sees:** Not a huge jump over the current pass, but closes the
gap so every scalar op in the binary looks weird. Also extends to
`icmp` so comparison operators become obfuscated.

**Implementation effort:** Low-medium. Incremental work on
`src/obfuscator/instruction_substitution.rs`.

**Estimated LOC:** 300–500 added.

---

## Tier D — "nice to have / per-build variety"

### D1. Per-build pipeline randomisation

**What it does:** Accept a `--seed <u64>` flag (or generate fresh each
build). Shuffle the pass order within safe dependency constraints. Vary
probabilities (e.g. which 30% of basic blocks get outlined) and key
material (XOR keys, alias pointers). Result: every build is a distinct
binary even if the source is unchanged.

**What RE sees:** Two consecutive builds produce binaries that look
unrelated. All byte-sequence signatures break; any IoC derived from one
build is worthless for another.

**Implementation effort:** Low. Most of it is plumbing a global `Seed`
through every pass.

**Estimated LOC:** 200 plumbing + wiring.

---

### D2. Constant obfuscation

**What it does:** Integer constants in hot paths get replaced with
arithmetic expressions that evaluate to the same value.

**What RE sees:** `mov eax, 0xC0DE` becomes
`mov eax, 0x1337; xor eax, 0xD3C9; add eax, some_var`.

**Implementation effort:** Low-medium. A subset of B1 applied only to
`ConstantInt` operands.

**Estimated LOC:** 200–400.

---

### D3. Opaque predicates (standalone)

**What it does:** Generate provably-true or provably-false predicates
that a symbolic executor needs non-trivial effort to solve. e.g.
`x*x - 34*y*y != 1` (Pell's equation) as a tautology.

**What RE sees:** Every branch has a non-trivial mathematical condition
the analyst has to prove. Typically used to guard the bogus-CF branches
we already have — so this is really an upgrade to BCF rather than a
standalone pass.

**Implementation effort:** Low. Extension of `bogus_control_flow.rs`.

**Estimated LOC:** 100–200.

---

### D4. Anti-debug stubs

**What it does:** Emit small helper functions that check for debugger
presence (`IsDebuggerPresent`, `NtQueryInformationProcess` with
`ProcessDebugPort`, timing checks across `rdtsc`) and scramble data or
divert control flow if they trip.

**What RE sees:** Binary refuses to run under a debugger, or produces
wrong output. Escalates from "annoying to read" to "annoying to run".
Has ethical / operational tradeoffs (breaks crash reporters, sometimes
trips AV heuristics falsely).

**Implementation effort:** Medium. Not an IR pass per se — more a set
of `#[link]` Rust helpers we inject into the obfuscated crate, plus a
pass that inserts calls.

**Estimated LOC:** 300–500.

---

## Recommended build order

This is a concrete execution order that front-loads the highest RE pain
per unit of implementation effort.

### Sprint 1 — "cheap and devastating" (~1.5 weeks)

1. **A2 Shuffle Blocks** (100–200 LOC) — trivial, instantly breaks byte
   signatures across builds.
2. **D1 Per-build seed + pipeline randomisation** (200 LOC) — amplifies
   every subsequent pass.
3. **S1 Indirect Branch** (400–600 LOC) — first pass that truly breaks
   decompilers.

### Sprint 2 — "fully break the CFG" (~2 weeks)

4. **S2 Indirect Call** (500–800 LOC).
5. **A3 Function Wrapper** (200–400 LOC) — feeds into S2 and makes it
   hurt more.
6. **A4 Clone Function + Diversify** (300–500 LOC) — combines with A2/D1
   to kill signature-based analysis.

### Sprint 3 — "kill the decompiler's pseudo-C" (~2 weeks)

7. **B1 Advanced MBA** (800–1500 LOC).
8. **B2 Lower Switch** (300–500 LOC).
9. **C3 Instruction-Substitution — advanced** (300–500 LOC), augmenting
   the existing pass.

### Sprint 4 — "hide the data model" (~2 weeks)

10. **B3 Delayed Offset Loading** (400–700 LOC).
11. **B4 Alias-Access** (600–900 LOC).
12. **D2 Constant obfuscation** (200–400 LOC).

### Sprint 5 — "secondary annoyance" (~1 week)

13. **A1 Basic-Block Outlining** (400–700 LOC) — done late so earlier
    passes have fired first.
14. **C1 Parameter Aggregation** (300–500 LOC).
15. **D3 Opaque Predicates (BCF upgrade)** (100–200 LOC).

### Far future — "VM tier" (1–2 months)

16. **S3 VM-based flattening** (3000–5000 LOC). Only on hand-selected
    hot/sensitive functions via a `#[lyra::vm_protect]` attribute.
17. **C2 Custom Calling Convention** (500–800 LOC).
18. **D4 Anti-debug stubs** (300–500 LOC) — separate flag, off by
    default, documented tradeoffs.

---

## What a fully-loaded obfuscation looks like (from the analyst's view)

After Sprints 1–4 are shipped, an analyst reverse engineering
`app_obf.exe` would see:

- `strings` output: no meaningful business logic (shipped).
- IDA function list: populated, but every function's graph view is a
  dense cloud of small blocks connected through indirect branches
  (S1). Xrefs to most functions are empty (S2).
- Double-clicking a function in Hex-Rays: the pseudo-C is 200–500
  lines of MBA-expanded arithmetic over pointer aliases (B1+B4), with
  `(*fptr)(...)` for every call, and no recognisable `switch` (B2),
  no recognisable string literal or data table (B3, shipped).
- Running `diaphora` / BinDiff against the previous build: almost 100%
  of functions marked "unmatched" (D1+A4).
- Trying `angr` / symbolic execution: the constraint solver chokes on
  MBA + indirect dispatch.
- Trying `ghidra` + scripts: same as IDA, plus more errors because
  Ghidra's CFG recovery is slightly weaker.

That's the target end state. This ROADMAP is the sequenced plan to get
there.

---

## YARA evasion alignment

YARA is a narrower threat model than "a human in IDA" and deserves its
own audit. YARA rules match concrete artefacts in the file image, so
what hurts YARA is different from what hurts a decompiler.

### What YARA rules actually look at

| Rule artefact | Example YARA feature |
|---|---|
| Literal text strings | `$s = "CreateRemoteThread"` |
| Hex byte patterns | `$h = { 48 8B 05 ?? ?? ?? ?? FF D0 }` |
| Regex over strings | `$r = /https?:\/\/[a-z0-9\.]+\/admin\.php/` |
| PE header fields | `pe.timestamp == 0x60000000`, `pe.number_of_sections > 6` |
| Import hash | `pe.imphash() == "abcd..."` |
| Import name match | `pe.imports("ws2_32.dll", "WSASocketA")` |
| Rich header (MSVC) | `pe.rich_signature.toolid(...)` |
| Section name | `pe.section[0].name == ".text"` with specific entropy |
| Entry-point bytes | `$ep = { 55 8B EC ... } at pe.entry_point` |
| Resources | `pe.resources[0].type == RT_MANIFEST` |
| File-global statistics | `math.entropy(0, filesize) > 7.0`, `filesize < 5MB` |

### How current / planned passes map

| Rule artefact | Defeated by |
|---|---|
| Literal strings | **String Encryption** (shipped) |
| Hex byte patterns (intra-fn) | **Shuffle Blocks** (shipped), **Indirect Branch** (shipped), **MBA B1**, **Instruction-Sub C3**, **Clone Function A4** |
| Hex patterns (block-boundary) | **Shuffle Blocks** (shipped), **Per-build Seed D1** (shipped) |
| Regex over strings | **String Encryption** (shipped) |
| Per-build determinism break | **Per-build Seed D1** (shipped, use `--seed` omission) |
| Call-site byte sequences | **Indirect Call S2**, **Function Wrapper A3** |
| Imports / imphash | **Y3 Import Obfuscation** (new — see below) |
| Import name strings | Already defeated by string-enc? **No** — import names live in PE import directory, not `.rodata`. Needs **Y3**. |
| Rich header (MSVC fingerprint) | **Y1 PE Header Polymorphism** (new — see below) |
| Section layout / names | **Y1 PE Header Polymorphism**, **Y2 Shuffle Functions** |
| Entry-point bytes | **Y2 Shuffle Functions** (reorders `.text`, shifts EP prologue bytes), **Y1** (timestamp / characteristics jitter) |
| File size bucketing | **Y4 Section Size Jitter** (new) |
| Global entropy rules | **no change needed** — our string encryption preserves per-section entropy within normal bounds (it XOR-encodes strings, doesn't bulk-encrypt the section). |

### Coverage gaps

The existing tiered list is solid for **string-based** and
**code-pattern-based** YARA rules, but misses three categories of
rules common in production YARA feeds:

1. **Imphash / import-name rules** — YARA's `pe.imphash()` is an MD5 of
   the normalised import table. It's a staple of malware family
   clustering (VT/MISP). Nothing in the current pipeline touches
   imports.
2. **Rich header / PE metadata rules** — the Rich header is an MSVC
   linker fingerprint that uniquely identifies which compiler and
   linker built the binary. Copying one of those across builds makes
   Mandiant/CrowdStrike/Mandiant-style linker-based clustering trivial.
   We don't touch it.
3. **Function-boundary / entry-point rules** — rules that anchor at
   `pe.entry_point` or at known function start offsets aren't fully
   defeated by shuffling basic blocks within a function because the
   function's prologue still appears at a predictable offset.

### New passes — YARA tier (Y)

Inserting a new tier above D (higher priority than D) because YARA
evasion is usually the first artefact a defender reaches for.

#### Y1. PE Header Polymorphism

**What it does:** Post-link step that rewrites cosmetic fields of the
PE header to randomise the fingerprint without changing code behaviour:
- Zeros or jitters the `IMAGE_FILE_HEADER.TimeDateStamp`.
- Strips or randomises the Rich header (the embedded MSVC compiler
  version table between DOS stub and PE header).
- Randomises the `IMAGE_OPTIONAL_HEADER.MajorLinkerVersion`,
  `MinorLinkerVersion` within plausible ranges.
- Randomises section names (e.g. `.text` → kept, but `.rdata` →
  `.r$rand`, `.data` → `.d$rand`). Some section names must stay — the
  loader treats `.text`, `.CRT`, `.pdata` specially on Windows.

**What YARA sees:** Rich header is the single highest-signal field for
malware attribution; zeroing it removes it from the `pe.rich_signature`
YARA module entirely. Timestamp rules (`pe.timestamp == X`) fail.
Section-name rules (`pe.section[".rdata"].entropy > 7`) fail.

**Implementation effort:** Medium-low. Not an IR pass — runs in lyra
*after* `link.exe` finishes, before the final `.exe` is copied to
`--output`. Pure byte-level rewriting of a well-documented header
structure.

**Estimated LOC:** 300–500.

**Dependencies:** None. Runs as the final pipeline step.

#### Y2. Shuffle Functions (module-level)

**What it does:** Sibling to the shipped `shuffle_blocks`, but at the
module level. Permutes the textual order of functions within each
obfuscated crate's `.o`, so `.text` section layout differs per build.

**What YARA sees:** Rules anchored at `pe.entry_point` see different
prologue bytes across builds. Rules of the form
`$b1 at pe.entry_point` and `$b2 at pe.entry_point + 0x4A` fail
because the offset relationships across functions change. Function
hashing (FLIRT, lumina, Ghidra BSim) still identifies individual
functions but attribution based on layout breaks.

Important: LLVM IR does not have a "function order" operator the way
it has block order. We instead sort `Module::get_functions()` into a
desired order and re-emit. Alternatively, shuffle at the `.ll` text
level (simpler but has to be reparseable IR).

**Implementation effort:** Low-medium. Amice doesn't have an exact
equivalent.

**Estimated LOC:** 150–300.

**Dependencies:** None. Runs late (after block-shuffle so fn ordering
isn't disturbed by subsequent passes).

#### Y3. Import Table Obfuscation (promoted from planned tier D)

**What it does:** Defeats `pe.imphash()` and `pe.imports()` rules. Two
modes:
- **Hash-based imports**: replace `#[link(...)] extern "system" fn`
  static imports with a per-call runtime resolver. At init time we
  walk `kernel32!LoadLibrary` / `GetModuleHandle` + `GetProcAddress`
  by DJB2-hashed name so the import table no longer lists the real
  API names. Common technique in packers.
- **Obfuscated IAT**: keep the normal IAT but insert `k` bogus imports
  per build chosen from a pool of 200+ plausible APIs.

**What YARA sees:** `pe.imphash()` changes every build (mode 1 removes
most imports entirely; mode 2 adds fake imports). Rules that match
literal API names (`pe.imports("ws2_32.dll", "WSASocketA")`) fail in
mode 1.

**Implementation effort:** Medium-high. Mode 1 requires a resolver
helper in Rust, code-gen to rewrite every call site, and careful
handling of calling convention / SEH. Mode 2 is only a post-link
header rewrite (~200 LOC).

**Estimated LOC:** 600–1200 for mode 1; 200 for mode 2 alone.

**Dependencies:** For best effect, run after S2 Indirect Call so API
calls are already going through a pointer table.

#### Y4. Section-Size Jitter

**What it does:** Inject a private section of random length (50 KB –
2 MB) into the final binary. The section is never referenced but
occupies space, so rules that key on file size buckets
(`filesize < 5MB`, `filesize in (512KB..2MB)`) misbucket per build.

**What YARA sees:** File-size-based pre-filtering misses the file, or
worse (for the defender) triggers the wrong rule chain.

**Implementation effort:** Low. Post-link: append a random-content
section with benign-looking entropy (e.g. compressed zeros) to the PE.

**Estimated LOC:** 100–200.

**Dependencies:** None. Runs as the final pipeline step together
with Y1.

### Updated build order with YARA tier

The YARA passes (Y1–Y4) are lightweight compared to Tier A/B and high
value for the YARA-specific defender. I'd slot them **between Sprint 1
and Sprint 2**, bumping them ahead of broader CFG work because they
address a very real operational risk — YARA hits are usually the first
signal an EDR or a threat-hunter produces.

Revised sprint plan:

- **Sprint 1 (shipped)** — Shuffle Blocks, Per-build seed, Indirect
  Branch.
- **Sprint 1.5 — YARA tier (~1 week, new)** —
  - Y1 PE Header Polymorphism
  - Y2 Shuffle Functions
  - Y4 Section-Size Jitter
  - Y3 Import Table Obfuscation, mode 2 (fake imports) — defer mode 1
    until after Sprint 2 so S2 Indirect Call has landed.
- **Sprint 2** — Indirect Call, Function Wrapper, Clone Function.
- **Sprint 2.5** — Y3 mode 1 (hash-based imports).
- **Sprint 3+** — unchanged from above (MBA, Lower Switch, Delayed
  Offset Loading, etc.).

### Explicit non-goals for YARA

Two techniques intentionally not on the roadmap:
- **Bulk section encryption / packing** (UPX-style). Breaks entropy
  rules trivially but **raises** suspicion — `math.entropy > 7.5`
  triggers a lot of generic "packed binary" rules. Our goal is to
  *not look packed*, so this is counterproductive.
- **Code signing / counterfeit certificates**. Defenders pivot on
  certificate authorities, and forged ones are clear red flags.
  Genuine signing is out of scope (it's an ops concern, not a code
  concern).

