# gpress

## Commits

Every commit is authored and committed as the owner:

```sh
git config user.name "soumyadebroy3"
git config user.email "43921833+soumyadebroy3@users.noreply.github.com"
```

Cloud sessions start with a global `user.name=Claude`, so set both in the clone before the first commit, and set
`GIT_COMMITTER_NAME` and `GIT_COMMITTER_EMAIL` to the same when a tool commits for you. No commit message, merge
message or squash-merge message carries a `Co-Authored-By: Claude ...` or `Claude-Session: ...` trailer. This
replaces the attribution Claude Code adds by default. Pull request descriptions keep their attribution block.

History was rewritten on 2026-10-16 to apply this to every commit, so a clone or branch from before then holds old
commit ids. Fetch, then reset onto `origin`, or move your own commits with `git rebase --onto origin/<branch> <old tip>`.
Never push a commit whose history has the old ids.
