#!/usr/bin/env python3
"""The CI test jobs split `cargo test --release --workspace` in two; this checks the halves still run what it runs.

One half runs `cargo test --lib` and `cargo test --doc`, the other `cargo test --bins --test '*'` and builds the
examples (`cargo build --examples`): targets picked by kind. Plain `cargo test` picks them by their `test` and
`doctest` settings instead, and skips an integration test whose `required-features` are off, so the two agree only
while every library, binary and integration test keeps `test = true`, every example and bench keeps `test = false`,
every library rustdoc can test keeps `doctest = true`, and no integration test has required features. A target that
breaks one of these fails this check, so that it cannot slip out of both halves unnoticed: the split in
.github/workflows/ci.yml has to change with it. Run from anywhere, under python3 -I:

  python3 -I bench/check_test_split.py            exit 0 when the halves match `cargo test`, 1 otherwise"""
import json, os, subprocess, sys

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
# Kinds `cargo test` runs as tests by default (test = true) and the halves run whatever their setting.
TESTED = {'lib', 'rlib', 'dylib', 'cdylib', 'staticlib', 'proc-macro', 'bin', 'test'}
# Kinds `cargo test` runs as tests only with test = true, which the halves never do.
UNTESTED = {'example', 'bench'}
# Library kinds (their crate types) rustdoc runs doc tests for: `cargo test` skips them with doctest = false,
# `cargo test --doc` does not.
DOCTESTED = {'lib', 'rlib', 'proc-macro'}


def check(repo=REPO):
    """[(target, why)] of the workspace targets on which the halves and `cargo test` would disagree."""
    cmd = ['cargo', 'metadata', '--no-deps', '--format-version', '1']
    r = subprocess.run(cmd, cwd=repo, capture_output=True, text=True)
    if r.returncode != 0:
        raise RuntimeError('cargo metadata failed: ' + r.stderr[-500:])
    bad = []
    for p in json.loads(r.stdout)['packages']:
        for t in p['targets']:
            kinds = set(t['kind'])
            if 'test' not in t:
                raise RuntimeError('cargo metadata gives no `test` setting for its targets (cargo older than 1.70?)')
            where = f"{p['name']} {'/'.join(t['kind'])} {t['name']}"
            if kinds & TESTED and not t['test']:
                bad.append((where, 'test = false: cargo test skips it, the halves run it'))
            if kinds & UNTESTED and t['test']:
                bad.append((where, 'test = true: cargo test runs it as a test, the halves do not'))
            if kinds & DOCTESTED and not t['doctest']:
                bad.append((where, 'doctest = false: cargo test skips its doc tests, `cargo test --doc` runs them'))
            if 'test' in kinds and t.get('required-features'):
                bad.append((where, "required-features: cargo test skips it when they are off, `--test '*'` refuses to"))
    return bad


def main():
    try:
        bad = check()
    except (OSError, RuntimeError, ValueError, KeyError) as e:
        print(f'check_test_split: {e}', file=sys.stderr)
        sys.exit(2)
    for where, why in bad:
        print(f'{where}: {why}', file=sys.stderr)
    if bad:
        print(f'{len(bad)} targets would make the CI test halves differ from `cargo test`: change the split in '
              '.github/workflows/ci.yml with them', file=sys.stderr)
        sys.exit(1)
    print('the CI test halves run every target `cargo test` runs, and no other')


if __name__ == '__main__':
    main()
