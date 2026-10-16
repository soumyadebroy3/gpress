#!/usr/bin/env python3
"""Move the local branches of a gpress clone from before 2026-10-16 onto the rewritten history.

On 2026-10-16 every branch on GitHub was rewritten in one pass:
- "Co-Authored-By: Claude ..." and "Claude-Session: ..." lines were taken out of every commit message;
- Claude <noreply@anthropic.com> as author or committer became the owner;
- signatures were dropped.

Trees, dates, merges and every other identity stayed as they were. This script makes the same rewrite of the
branches, remote-tracking branches and detached worktree HEADs that still hold old commits. Each commit GitHub had
gets the id GitHub gave it. Commits GitHub never had are rewritten the same way and stay on top. The script writes
new commit objects and moves those refs, nothing else. The working tree, the index and stashes are left as they
are, and so is every worktree. Since every tree is unchanged, uncommitted and staged changes stay exactly what they
were. The old commits stay in each ref's reflog.

    git fetch origin main
    git show origin/main:.claude/history-rewrite-2026-10-16.py | python3 - [--dry-run]
    git fetch --prune origin

Run it before fetching the other branches: it rewrites the remote-tracking branches too, so it can tell the commits
GitHub had from the ones it never had, and `git pull --rebase` can move only those if GitHub's branch moved on since.
"""
import os
import re
import subprocess
import sys

OLD_ROOT = "2ef1abeff2f7457c7c0991b41cd7903b06950ddc"
# Old commits whose new ids are known: the rewrite here must give exactly these, or nothing is moved.
KNOWN = {
    OLD_ROOT: "3eeb57c828a2db78a149f4f8688be1845b33d47c",  # the first commit
    "c1f07e10f4bae0f5ebb4222511b9fced77172fe7": "d093ab45c4092aacb656b3c3b9929459546f71a1",  # main
}
OWNER_NAME = b"soumyadebroy3"
OWNER_EMAIL = b"43921833+soumyadebroy3@users.noreply.github.com"
CLAUDE_EMAIL = b"noreply@anthropic.com"
TRAILER = re.compile(rb"^[ \t]*(co-authored-by:[ \t]*claude\b.*|claude-session:.*)$", re.I)
GH_SEPARATOR = re.compile(rb"^-{5,}$")
IDENT = re.compile(rb"^(author|committer) (.*) <(.*)> (\d+ [+-]\d{4})$")
REFLOG_MESSAGE = "history rewrite of 2026-10-16: Claude trailers and identity off"


def git(*args, input=None, check=True):
    p = subprocess.run(["git", *args], input=input, capture_output=True)
    if check and p.returncode:
        sys.exit(f"git {' '.join(args)} failed: {p.stderr.decode(errors='replace').strip()}")
    return p.stdout if check else p


def is_blank(line):
    return line.strip() == b""


def clean_message(msg):
    """The message without Claude trailers, exactly as on GitHub."""
    body = msg[:-1] if msg.endswith(b"\n") else msg
    items = [None if TRAILER.match(line) else line for line in body.split(b"\n")]
    if None not in items:
        return msg
    # GitHub's squash message puts a "---------" line before its co-author trailers; once those trailers are gone
    # the separator introduces nothing.
    kept = [k for k, x in enumerate(items) if x is not None and not is_blank(x)]
    if kept and GH_SEPARATOR.match(items[kept[-1]]) and None in items[kept[-1] + 1:]:
        items[kept[-1]] = None
    out = []
    i = 0
    while i < len(items):
        if items[i] is not None and not is_blank(items[i]):
            out.append(items[i])
            i += 1
            continue
        j = i
        while j < len(items) and (items[j] is None or is_blank(items[j])):
            j += 1
        run = items[i:j]
        if None in run:
            if any(x is not None for x in run):
                out.append(b"")  # blank lines around a removed trailer: keep one
        else:
            out.extend(run)  # untouched blank lines stay exactly as they were
        i = j
    while out and is_blank(out[-1]):
        out.pop()
    return b"\n".join(out) + b"\n"


def rewrite(raw, new_id):
    """The rewritten commit object for the raw commit `raw`, its parents mapped by `new_id`."""
    head, sep, msg = raw.partition(b"\n\n")
    if not sep:
        head, msg = raw.rstrip(b"\n"), b""
    lines = []
    skipping = False
    for line in head.split(b"\n"):
        if line.startswith(b" "):
            if not skipping:
                lines.append(line)
            continue
        key = line.split(b" ", 1)[0]
        skipping = key in (b"gpgsig", b"gpgsig-sha256")
        if skipping:
            continue
        if key == b"parent":
            parent = line[7:].decode()
            line = b"parent " + new_id.get(parent, parent).encode()
        elif key in (b"author", b"committer"):
            m = IDENT.match(line)
            if not m:
                sys.exit(f"cannot read the {key.decode()} line of a commit: {line!r}")
            if m.group(3) == CLAUDE_EMAIL:
                line = b"%s %s <%s> %s" % (key, OWNER_NAME, OWNER_EMAIL, m.group(4))
        lines.append(line)
    return b"\n".join(lines) + b"\n\n" + clean_message(msg)


def main():
    args = sys.argv[1:]
    if args not in ([], ["--dry-run"]):
        sys.exit("usage: history-rewrite-2026-10-16.py [--dry-run]")
    dry_run = bool(args)

    if git("rev-parse", "--is-shallow-repository").strip() == b"true":
        sys.exit("This clone is shallow: run `git fetch --unshallow origin`, then run this again.")
    if git("cat-file", "-e", OLD_ROOT + "^{commit}", check=False).returncode:
        print("This clone has no commits from before the rewrite: nothing to move.")
        return

    # What to move: branches, remote-tracking branches and detached worktree HEADs that hold the old first commit.
    refs = {}
    for line in git("for-each-ref", "--contains", OLD_ROOT, "--format=%(objectname) %(refname) %(symref)",
                    "refs/heads", "refs/remotes").decode().splitlines():
        sha, ref, symref = line.split(" ")
        if not symref:  # a symbolic ref such as origin/HEAD moves with the ref it points to
            refs[ref] = sha
    heads = {}  # worktree path -> its detached HEAD
    worktrees = []
    block = {}
    for line in git("worktree", "list", "--porcelain").decode().splitlines() + [""]:
        if line:
            key, _, value = line.partition(" ")
            block[key] = value
            continue
        if block and "prunable" not in block:
            worktrees.append(block["worktree"])
            if "detached" in block and not git("merge-base", "--is-ancestor", OLD_ROOT, block["HEAD"],
                                                check=False).returncode:
                heads[block["worktree"]] = block["HEAD"]
        block = {}
    for path in worktrees:
        for state in ("rebase-merge", "rebase-apply", "MERGE_HEAD", "CHERRY_PICK_HEAD", "REVERT_HEAD", "BISECT_LOG"):
            if os.path.exists(os.path.join(path, git("-C", path, "rev-parse", "--git-path", state).decode().strip())):
                sys.exit(f"The worktree at {path} is in the middle of a rebase, merge, cherry-pick, revert or bisect: "
                         "finish or abort it, then run this again.")
    if not refs and not heads:
        print("No branch or detached HEAD holds commits from before the rewrite: nothing to move.")
        return

    # Commits already on the new history (every ref without the old first commit) stay as they are.
    keep = git("for-each-ref", "--no-contains", OLD_ROOT, "--format=%(objectname)",
               "refs/heads", "refs/remotes", "refs/tags").decode().split()
    tips = sorted(set(refs.values()) | set(heads.values()))
    todo = git("rev-list", "--topo-order", "--reverse", *tips, "--not", *keep, "--").decode().split()

    raws = {}
    batch = git("cat-file", "--batch", input="".join(s + "\n" for s in todo).encode())
    i = 0
    for sha in todo:
        nl = batch.index(b"\n", i)
        _, kind, size = batch[i:nl].split(b" ")
        if kind != b"commit":
            sys.exit(f"{sha} is a {kind.decode()}, not a commit")
        size = int(size)
        raws[sha] = batch[nl + 1:nl + 1 + size]
        i = nl + 1 + size + 1

    new_id = {}
    hash_args = ["hash-object", "-t", "commit", "--stdin"] + ([] if dry_run else ["-w"])
    for sha in todo:
        new_id[sha] = git(*hash_args, input=rewrite(raws[sha], new_id)).decode().strip()
    for old, new in KNOWN.items():
        if old in new_id and new_id[old] != new:
            sys.exit(f"The rewrite gave {old[:7]} the id {new_id[old][:7]}, but GitHub has {new[:7]}. Nothing was "
                     "moved; ask the owner.")
    if not dry_run:
        for ref, sha in list(refs.items()) + list(heads.items()):
            if git("rev-parse", sha + "^{tree}") != git("rev-parse", new_id[sha] + "^{tree}"):
                sys.exit(f"{ref}: the rewritten tip has another tree. Nothing was moved; ask the owner.")

    print(("Would move" if dry_run else "Moving") + f" {len(refs) + len(heads)} refs ({len(todo)} commits):")
    for ref, sha in sorted(refs.items()):
        print(f"  {ref}  {sha[:7]} -> {new_id[sha][:7]}")
    for path, sha in sorted(heads.items()):
        print(f"  HEAD of {path} (detached)  {sha[:7]} -> {new_id[sha][:7]}")
    if dry_run:
        return

    updates = "".join(f"update {ref} {new_id[sha]} {sha}\n" for ref, sha in refs.items())
    if updates:
        git("update-ref", "-m", REFLOG_MESSAGE, "--stdin", input=updates.encode())
    for path, sha in heads.items():
        git("-C", path, "update-ref", "--no-deref", "-m", REFLOG_MESSAGE, "HEAD", new_id[sha], sha)

    left = git("for-each-ref", "--contains", OLD_ROOT, "--format=%(refname)", "refs/heads", "refs/remotes").decode()
    if left.strip():
        sys.exit(f"Still on the old history: {' '.join(left.split())}. Ask the owner.")
    moved = [new_id[sha] for ref, sha in refs.items() if ref.startswith("refs/heads/")]
    moved += [new_id[sha] for sha in heads.values()]
    remotes = git("for-each-ref", "--format=%(objectname)", "refs/remotes").decode().split()
    own = git("log", "--format=  %h %an <%ae> | %cn <%ce> | %s", *moved, "--not", *remotes, "--").decode() \
        if moved else ""
    print("Done. Commits GitHub doesn't have:\n" + (own or "  none\n"), end="")
    merges = git("rev-list", "--merges", *moved, "--not", *remotes, "--").decode() if moved else ""
    print("Now run `git fetch --prune origin`. If a push is then refused because GitHub's branch moved on, "
          + ("ask the owner: these commits include a merge, which a rebase would flatten. "
             if merges else "`git pull --rebase` puts these commits on top of it. ")
          + "Never force-push.")


if __name__ == "__main__":
    main()
