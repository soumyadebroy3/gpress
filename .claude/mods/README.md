# Claude Code mods for gpress

## Turning them on

Repo settings cannot load a mod by themselves, so each place that runs Claude Code needs one switch:

- **Cloud sessions (claude.ai/code and projects):** in the cloud environment, set
  `CLAUDE_CODE_PLUGIN_DIRS=/home/user/gpress/.claude/mods:/home/claude/gpress/.claude/mods`
  (both clone locations are listed; a path that does not exist is skipped). Every mod in this folder loads in
  sessions started after that, including mods added later, so a pull request that changes `.claude/mods/` changes
  what every cloud session runs and gets the same review as engine code.
- **Your own machine:** `.claude/settings.json` registers this folder as the `gpress-mods` marketplace and
  enables its mods; trust the folder when Claude Code asks, or run `/plugin install <mod>@gpress-mods`.

Checks for any mod: `claude plugin validate .claude/mods/<mod>` and `claude plugin test .claude/mods/<mod>`.

## cheap-route

Sends read-only subagent tasks (lookups, summaries, status checks, reading files or logs, Explore searches) to Haiku. Anything that writes (code, tests, docs, renames, typo fixes, version bumps, changelog edits, commits) or needs judgement (implementing, debugging, optimizing, benchmarking, reviewing, planning) stays on the session's model, and so does any task with no clear read-only signal, so code quality is unchanged.

It also switches up on its own. Each step of a cheap subagent is held until it is complete. If that step tries to edit a file or run a command that changes things, comes back empty or unsure, gets refused, or the task runs past 20 steps, the step is thrown away and re-run on the session's model, and the session's model keeps the rest of the task. Two failed tool calls switch it up too. It covers the Agent tool and workflow `agent()` calls. A task that already names a model, forks, resumes and prompts over 2000 characters are left alone. `/cheap-route` lists recent decisions; `/config` turns it off, switches the cheap model to Sonnet, or changes the length and step limits.

## pr-review

Claude is the only reviewer of gpress pull requests and merges them itself, so this mod makes the review a
fixed procedure and makes merging wait for it.

- `/review-pr <number>` sends the main model the review protocol: read the diff, the brief and
  `docs/tasks/README.md`; start five fresh reviewer subagents (correctness and no data loss, security, fmt,
  clippy and tests on both feature sets, the brief's acceptance numbers and file ownership, byte identity
  through `bench/benchmark.py compare` for a pure brief); verify each blocker with another fresh agent; post one
  comment whose first line is `gpress-review: PASS <head sha>` or `gpress-review: FINDINGS <head sha>`.
- The same rule sits in the main model's system prompt, so a session asked to merge reviews first without
  being told.
- A merge (the GitHub merge tool, any `gh pr merge` form, the REST or GraphQL merge routes) is
  refused until this session has posted a PASS for that pull request, and the merge must pin the head the
  review passed (`expectedHeadSha`, or `gh pr merge <n> --match-head-commit <sha>`), so a push after the review
  means a new review. Auto-merge, which cannot pin a head, and a direct push or file write to main are refused. `/config` can turn the gate off.
- It is a guard against skipping the review by mistake, not against a session set on getting around it.
- Reviewers run on the session's model: cheap-route keeps any task that mentions reviewing there.
