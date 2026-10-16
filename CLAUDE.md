# gpress

## Commits

Every commit you make is authored and committed as the owner:

```sh
git config user.name "soumyadebroy3"
git config user.email "43921833+soumyadebroy3@users.noreply.github.com"
```

Cloud sessions start with a global `user.name=Claude`, so set both in the clone before the first commit. A tool that
commits without the clone's config needs `GIT_AUTHOR_NAME`, `GIT_AUTHOR_EMAIL`, `GIT_COMMITTER_NAME` and
`GIT_COMMITTER_EMAIL` set to the same. No commit message, merge message or squash-merge message carries a
`Co-Authored-By: Claude ...` or `Claude-Session: ...` trailer; this replaces the attribution Claude Code adds by
default. A pull request merged on GitHub is committed by GitHub, the one exception: give it your own squash title and
message, without those trailers. Pull request descriptions keep their attribution block.

## The history rewrite of 2026-10-16

On 2026-10-16 every branch was rewritten to take Claude's name and the Claude trailers off every commit. File contents
did not change, but every commit id did: the first commit `2ef1abe` became `3eeb57c`, and main's `c1f07e1` became
`d093ab4`. A branch with old ids has `2ef1abe` in its history (`git merge-base --is-ancestor 2ef1abe HEAD` succeeds).
To move it:

- With no commits or changes of your own on it: `git fetch origin`, then `git reset --hard origin/<branch>`.
- With commits of your own: right after `git fetch origin`, the branch's old tip on GitHub is `origin/<branch>@{1}`.
  Move your commits onto the new tip, taking Claude's name and the trailers off them as they go, then check
  `git log origin/<branch>..HEAD --format='%an <%ae> | %cn <%ce>%n%B'` before pushing:

  ```sh
  STRIP='git log -1 --format=%B | grep -viE "^(co-authored-by: *claude|claude-session:)" | git commit -q --amend --reset-author -F -'
  git rebase --exec "$STRIP" --onto origin/<branch> origin/<branch>@{1}
  ```

Never push a commit whose history has the old ids, and never force-push a branch to get rid of them.
