#!/usr/bin/env python3
"""Every dependency must come from crates.io, and every workspace crate must be one of ours.

cargo-deny skips crates whose manifest says `publish = false` (deny.toml `private.ignore`, meant for our own
workspace crates), so a path or git dependency marked that way would escape the licence check; and cargo makes
any path dependency under the repository root a workspace member on its own, so such a crate is "private
workspace" to cargo-deny without ever being named in Cargo.toml. This closes the gap: a package in
`cargo metadata` must be either a workspace member whose manifest is one of the `[workspace] members` paths of
the root Cargo.toml, or a crate whose source is the crates.io registry. Run from anywhere, under python3 -I:

  python3 -I bench/check_dep_sources.py            exit 0 when every package passes, 1 otherwise

`bench/security.py supply` runs the same check. The metadata is taken with every feature, as deny.toml
checks them."""
import json, os, re, subprocess, sys

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
REGISTRY = 'registry+https://github.com/rust-lang/crates.io-index'


def listed_members(repo):
    """The manifest paths the root Cargo.toml names under [workspace] members."""
    text = open(os.path.join(repo, 'Cargo.toml')).read()
    m = re.search(r'^\[workspace\].*?^members\s*=\s*\[(.*?)\]', text, re.S | re.M)
    if not m:
        raise RuntimeError('no [workspace] members list in Cargo.toml')
    out = set()
    for p in re.findall(r'"([^"]+)"', m.group(1)):
        out.add(os.path.realpath(os.path.join(repo, p, 'Cargo.toml')))
    return out


def check(repo=REPO):
    """[(name, version, why)] of the packages that are neither ours nor from crates.io."""
    r = subprocess.run(['cargo', 'metadata', '--format-version', '1', '--all-features'], cwd=repo, capture_output=True, text=True)
    if r.returncode != 0:
        raise RuntimeError('cargo metadata failed: ' + r.stderr[-500:])
    meta = json.loads(r.stdout)
    members = set(meta.get('workspace_members', []))
    ours = listed_members(repo)
    bad = []
    for p in meta['packages']:
        manifest = os.path.realpath(p['manifest_path'])
        if p['id'] in members:
            if manifest not in ours:
                bad.append((p['name'], p['version'], f'a workspace crate Cargo.toml does not list: {manifest}'))
        elif p.get('source') != REGISTRY:
            bad.append((p['name'], p['version'], f"not from crates.io: {p.get('source') or manifest}"))
    return bad


def main():
    try:
        bad = check()
    except (OSError, RuntimeError, ValueError) as e:
        print(f'check_dep_sources: {e}', file=sys.stderr)
        sys.exit(2)
    for name, version, why in bad:
        print(f'{name} {version}: {why}', file=sys.stderr)
    if bad:
        print(f'{len(bad)} packages are neither our own listed crates nor from crates.io (deny.toml cannot vouch for them)', file=sys.stderr)
        sys.exit(1)
    print('every dependency comes from crates.io, and every workspace crate is one Cargo.toml lists')


if __name__ == '__main__':
    main()
