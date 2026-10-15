# Claude Code mods for gpress

## cheap-route

Sends read-only subagent tasks (lookups, summaries, status checks, reading files or logs, Explore searches) to Haiku. Anything that writes (code, tests, docs, renames, typo fixes, version bumps, changelog edits, commits) or needs judgement (implementing, debugging, optimizing, benchmarking, reviewing, planning) stays on the session's model, and so does any task with no clear read-only signal, so code quality is unchanged. It covers the Agent tool and workflow `agent()` calls. A task that already names a model, forks, resumes and prompts over 2000 characters are left alone. `/cheap-route` lists recent decisions; `/config` turns it off or switches the cheap model to Sonnet.

### Turning it on

Repo settings cannot load a mod by themselves, so each place that runs Claude Code needs one switch:

- **Cloud sessions (claude.ai/code and projects):** in the cloud environment, set
  `CLAUDE_CODE_PLUGIN_DIRS=/home/user/gpress/.claude/mods/cheap-route:/home/claude/gpress/.claude/mods/cheap-route`
  (both clone locations are listed; a path that does not exist is skipped). Sessions started after that load it.
- **Your own machine:** `.claude/settings.json` registers this folder as the `gpress-mods` marketplace and enables `cheap-route`; trust the folder when Claude Code asks, or run `/plugin install cheap-route@gpress-mods`.

### Checks

```
claude plugin validate .claude/mods/cheap-route
claude plugin test .claude/mods/cheap-route
```
