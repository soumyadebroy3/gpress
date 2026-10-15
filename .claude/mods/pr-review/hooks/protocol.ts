// The review every gpress pull request gets before it merges. It is sent to
// the main model as a prompt (/review-pr) and kept in its system prompt, so a
// session asked to review or merge follows it without being told.

export const MARK = 'gpress-review:'

export const protocol = (pr: string) => `Review gpress pull request ${pr} with the protocol below. Do every step; do not skip one because the change looks small.

1. Read the pull request: its description, every changed file in the diff, and its head commit sha (\`gh pr view ${pr} --json headRefOid,headRefName,body,files\` and \`gh pr diff ${pr}\`). Find its brief: the docs/tasks/*.md file the branch name (opt-<name>) or the description names. Read docs/tasks/README.md (the rules every branch is held to, the procedure, and which files each session owns) and the brief's acceptance rule. A pull request with no brief (tooling, docs) is held to the README rules alone.

2. Start five reviewers at once, each a fresh general-purpose subagent that did not write the change. Do not give them a cheaper model. Each gets the pull request number, the head sha, the brief's path, and only its own part:
   a. Correctness: read the diff against the code around it. Wrong results, broken edge cases, lost data (every normal-PDF object kept, every archive restored byte for byte), output that depends on the thread count or machine, the engine's self-checks turned off.
   b. Security: no new unsafe code; no panic across the library boundary; memory and time bounded on hostile input, with threads too; nothing read from the environment except PDFE_DEBUG; new dependencies only under MIT, Apache-2.0, BSD, ISC, Zlib, Unicode or MPL-2.0 (run \`cargo deny check\` and \`python3 -I bench/check_dep_sources.py\` when Cargo.toml or Cargo.lock changed).
   c. Build hygiene, on a checkout of the head sha: \`cargo fmt --all --check\`, \`cargo clippy --workspace --all-targets -- -D warnings\` and the same with \`--features existing\`, \`cargo test --release --workspace\` and the same with \`--features existing\`. Each changed behaviour has a test. For a failure, check whether main fails the same way; a failure main shares is reported as such, not as this change's.
   d. The brief: every acceptance number the brief sets, checked against the \`benchmark.py compare\` output in the description (speed, size never larger on any file, peak memory, the thread rules: CPU at most 1.15x the one-thread CPU). The change touches only the files its session owns, or names each exception. Missing compare output, or a run made with missing tools (\`missing_tools\` in meta.json), is a finding.
   e. Byte identity, for a brief marked pure: the compare output must show every output byte-identical. When it does not say so, or the description lacks it, build base and head and run \`python3 -I bench/benchmark.py time\` on \`--set sample120 --limit 20 --efforts 0,50 --threads 1\` for both, then \`compare\`; any changed byte is a finding. For a trade, any output larger than before is a finding.
   Each reviewer reports findings as: severity (blocker or minor), file:line, what goes wrong, and the input or command that shows it. No finding means it says so.

3. Verify each blocker with one more fresh subagent told to disprove it. Keep only findings that hold.

4. Post one comment on the pull request (the GitHub add_issue_comment tool, or \`gh pr comment <n> --body "..."\`, not --body-file), its first line exactly
   \`${MARK} PASS <head sha>\` when no blocker holds, or \`${MARK} FINDINGS <head sha>\` otherwise,
   then one line per finding (severity, file:line, what goes wrong) and one line per check run (fmt, clippy, tests, compare). End it with the Claude Code attribution footer.

5. Merge only after a PASS for the current head sha, with CI green apart from checks that fail on main too, pinning that sha in the merge (\`expectedHeadSha\`, or \`gh pr merge <n> --match-head-commit <sha>\`; auto-merge cannot pin one and is refused). A push after the review means a new review. On FINDINGS, fix them (when it is your pull request) or report them, and review again after the fix.`

export const SYSTEM_SECTION = `# Reviewing gpress pull requests

Claude is the only reviewer of gpress pull requests and merges them itself. Before merging any of them, or when asked to review one, run the review in the pr-review mod's protocol (the /review-pr command sends it): fresh reviewer subagents for correctness, security, build hygiene, the brief's acceptance numbers and byte identity, each blocker verified, and one comment whose first line is \`${MARK} PASS <head sha>\` or \`${MARK} FINDINGS <head sha>\`. The mod refuses a merge until this session has posted a PASS for the pull request's current head; pass \`expectedHeadSha\` when merging.`
