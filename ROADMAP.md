# Roadmap

Correctness comes before broader coverage, then compatibility and measured cost.
These are backlog items, not implemented features or support guarantees.

## Compatibility and correctness

- Track the upstream CodeGenPrepare fix and remove the local patch only after
  a pinned replacement SDK passes the same compiler and runtime regressions.
- Broaden native ABI, loader, TLS, and pre-constructor coverage beyond the
  executable and C-ABI shared-library fixtures.
- Qualify cross-target combinations and Rust `dylib` behavior separately from
  native executable tests.
- Add real `staticlib` archive interception with end-to-end runtime tests;
  it currently passes through without archive obfuscation.
- Expand pass eligibility only with LLVM-semantic and optimized native
  regressions for the newly supported constructs.

## Cost and analysis

- Measure runtime, startup, build time, memory, and section growth on broader
  redistributable workloads, including large multi-crate workspaces.
- Compare plain builds, no-pass Lyra, individual passes, and compositions with
  declared budgets, repeated samples, and transformed/skipped coverage.
- Investigate indirect-branch overhead without weakening correctness or silently
  reducing requested coverage.
- Evaluate analysis impact with reproducible tools and held-out behavioral
  checks. Changed hashes, larger disassembly, and tool failures are not proof
  of increased resistance.

New passes or ordering changes need a supported subset, composition tests,
measured costs, and a demonstrated benefit before becoming public options.
Runtime secrecy, universal analysis resistance, and detection evasion are not
promised. See [AGENTS.md](AGENTS.md) for verification and
[CONTRIBUTING.md](CONTRIBUTING.md) for contribution rules.
