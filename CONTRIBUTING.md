# Contributing

Work on a topic branch and open a pull request. Keep it up to date with `main`,
resolve review conversations, and wait for required CI checks to pass before
squash-merging. Approval is not required; maintainers may merge their own PRs
after checks pass. The squash commit uses the PR title and body, and GitHub
deletes the merged branch.

These protections also apply to administrators. `main` requires linear history;
direct pushes, force pushes, branch deletion, and merging with pending or failed
required checks are not allowed.

Every commit subject and PR title must use the format `type(scope): description`,
with a lowercase type, a nonempty scope without whitespace or parentheses, and a
short, concrete description. Choose a type and scope appropriate to the change.
This includes documentation and dependency changes. Examples:
`fix(ci): validate LLVM SDK versions` and `docs(contributing): clarify commit format`.

Keep public text concise, neutral, and focused on behavior. Do not include
private paths, downstream application names, private inputs, or secrets in
files, commits, reports, PRs, or comments. Review logs before attaching them.

For bugs, provide a small reproducible input, exact commands, tool versions,
platform, expected behavior, and actual result. For features, describe the use
case, proposed behavior, and limits. No prescribed title format is needed for
issues.

Keep changes scoped and add regressions for behavior changes. Follow
[AGENTS.md](AGENTS.md) for invariants and verification commands. Report checks
actually run, their platform and SDK, and any checks not run. A build or an IR
verification pass does not establish runtime correctness or support for other
platforms. Do not weaken validation to make a check pass.
