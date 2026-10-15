# Claude Code mods for gpress

## cheap-route

Sends easy subagent tasks (lookups, summaries, status checks, reading files, mechanical edits, Explore searches) to Haiku, and keeps anything that implements, debugs, fixes, optimizes, benchmarks, refactors, reviews, writes tests or plans on the session's model. It covers the Agent tool and workflow `agent()` calls. A task that already names a model, forks, resumes and prompts over 2000 characters are left alone. `/cheap-route` lists recent decisions; `/config` turns it off or switches the cheap model to Sonnet.

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
