#!/usr/bin/env python3
"""The size of what gpress ships: the native tool, the C library and the WebAssembly module.

    python3 -I bench/sizes.py measure [--features SET]   build all three and record their sizes in bench/sizes.json
    python3 -I bench/sizes.py check   [--features SET]   build the module and fail when its gzip size is more than
                                                         5% over the recorded one, or over the 400 KB target, or
                                                         when wasm-opt is missing (below); warn when it is more
                                                         than 5% under the recorded one

SET is a feature set: "default" (a plain build) or the cargo features to build with, separated by commas
(built with --no-default-features, so the set is exactly what is named). Each package is built with its own
feature of a name when it has one and with the engine's (gpress/NAME) when it does not, so a set that only the
engine knows still builds all three. Without --features both commands use the web module's set, WEB below.
Each set is recorded on its own, so measuring one leaves the others.

Sizes are in bytes, raw and gzip at level 9 (Python's gzip module, with no name or time in its header). The
module is built the way the npm package builds it (crates/gpress-wasm/js/build-wasm.mjs: cargo's "wasm"
profile, then binaryen's wasm-opt), so it needs node and wasm-opt; the native tool and the C library are
built with the release profile. Without wasm-opt on PATH build-wasm.mjs ships the module as cargo built it,
which is not the package's module (about 3% more gzip, still inside the 5% budget), so `measure` refuses to
record without wasm-opt and `check` fails without it when the record was made with it (the record names the
wasm-opt version, and CI installs binaryen); a different wasm-opt or rustc version is only noted. Builds use
cargo's usual target directory and settings (CARGO_BUILD_JOBS and the like). `measure` also prints the set's
rows for the Sizes table in bench/RESULTS.md, in its columns, with the sizes it replaced in the "Before"
cells (empty for a set measured the first time); the rows now in the table give the module built the old way
(the release profile, no wasm-opt) from the same code there instead.
"""
import argparse
import datetime
import gzip
import json
import os
import shutil
import subprocess
import sys
import tempfile

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(HERE)
RECORD = os.path.join(HERE, 'sizes.json')
BUILD_WASM = os.path.join(ROOT, 'crates', 'gpress-wasm', 'js', 'build-wasm.mjs')

# The feature set of the web module, the one `check` guards. Until the "web" feature set exists, the module
# built with the default features (every part of the engine) stands in for it; then this becomes WEB = 'web'.
WEB = 'default'

GROWTH = 1.05            # check: at most 5% over the recorded gzip size
SHRINK = 0.95            # check: a warning more than 5% under it, since the budget is counted from the record
TARGET_GZIP = 400_000    # the web module's budget: 400 KB gzip


def say(msg):
    print(f'sizes: {msg}', file=sys.stderr, flush=True)


def declared_features():
    """The features each package of the workspace declares, by package name."""
    p = subprocess.run(['cargo', 'metadata', '--no-deps', '--format-version', '1'], cwd=ROOT,
                       stdout=subprocess.PIPE, text=True, check=False)
    if p.returncode != 0:
        sys.exit('sizes: cargo metadata failed')
    return {pkg['name']: set(pkg['features']) for pkg in json.loads(p.stdout)['packages']}


def cargo_features(fset, package, declared):
    """The cargo arguments that build one package with a feature set. A name the package does not declare is
    taken from the engine (gpress/NAME), so the C library and the module do not need to forward every set."""
    if fset == 'default':
        return []
    own = declared.get(package, set())
    names = [n.strip() for n in fset.split(',') if n.strip()]
    names = [n if '/' in n or n in own or package == 'gpress' else f'gpress/{n}' for n in names]
    return ['--no-default-features', '--features', ','.join(names)]


def gz(path):
    with open(path, 'rb') as f:
        return len(gzip.compress(f.read(), 9, mtime=0))


def size(path):
    return {'raw': os.path.getsize(path), 'gzip': gz(path)}


def tool_version(cmd):
    try:
        p = subprocess.run(cmd, capture_output=True, text=True, check=False)
    except OSError:
        return None
    return p.stdout.strip() if p.returncode == 0 else None


def cargo_artifacts(args):
    """Runs a cargo build and returns the files it reports for our own crates."""
    p = subprocess.run(['cargo', 'build', '--message-format=json-render-diagnostics'] + args, cwd=ROOT,
                       stdout=subprocess.PIPE, text=True, check=False)
    if p.returncode != 0:
        sys.exit(f'sizes: cargo build {" ".join(args)} failed')
    files = []
    for line in p.stdout.splitlines():
        if line.startswith('{'):
            m = json.loads(line)
            if m.get('reason') == 'compiler-artifact' and m['target']['name'] in ('gpress', 'gpress_c'):
                files += m['filenames']
                if m.get('executable'):
                    files.append(m['executable'])
    return files


def wasm_opt_version():
    """wasm-opt's version line, or None when it is not on PATH (build-wasm.mjs looks for it the same way)."""
    return tool_version(['wasm-opt', '--version'])


NO_WASM_OPT = ('wasm-opt is not on PATH, so build-wasm.mjs would use the module as cargo built it, which is not the '
               "package's module (about 3% more gzip). Install binaryen, which has wasm-opt")


def build_module(cargo_args, out):
    if shutil.which('node') is None:
        sys.exit('sizes: node is needed to build the module the way the package does')
    p = subprocess.run(['node', BUILD_WASM, '--out', out] + cargo_args, cwd=ROOT, check=False)
    if p.returncode != 0:
        sys.exit('sizes: building the module failed')
    return size(out)


def measure(fset):
    # The record is the package's module, so it is made with wasm-opt (CI has it); checked first, before the
    # long native builds.
    wasm_opt = wasm_opt_version()
    if wasm_opt is None:
        sys.exit(f'sizes: {NO_WASM_OPT}, and measure again.')
    declared = declared_features()
    args = {pkg: cargo_features(fset, pkg, declared) for pkg in ('gpress', 'gpress-c', 'gpress-wasm')}
    entry = {'cargo_arguments': args}
    say(f'building the native tool ({fset})')
    tool = [f for f in cargo_artifacts(['--release', '-p', 'gpress', '--bin', 'gpress'] + args['gpress'])
            if os.path.basename(f) in ('gpress', 'gpress.exe')]
    say(f'building the C library ({fset})')
    clib = cargo_artifacts(['--release', '-p', 'gpress-c'] + args['gpress-c'])
    shared = [f for f in clib if f.endswith(('.so', '.dylib', '.dll'))]
    static = [f for f in clib if f.endswith(('.a', '.lib')) and not f.endswith('.dll.lib')]
    if not (tool and shared and static):
        sys.exit('sizes: cargo did not report the native tool and both C libraries')
    entry['tool'] = size(tool[0])
    entry['c_shared'] = size(shared[0])
    entry['c_static'] = size(static[0])
    say(f'building the WebAssembly module ({fset})')
    with tempfile.TemporaryDirectory() as tmp:
        entry['wasm'] = build_module(args['gpress-wasm'], os.path.join(tmp, 'gpress.wasm'))
    entry['wasm_opt'] = wasm_opt
    entry['rustc'] = tool_version(['rustc', '--version'])
    entry['date'] = datetime.date.today().isoformat()

    record = load_record() or {}
    was = record.get('builds', {}).get(fset)
    record.setdefault('about', 'Sizes in bytes of the builds of each feature set, written by '
                               '`python3 -I bench/sizes.py measure`; `check` compares the web module with them.')
    record['web'] = WEB
    record.setdefault('builds', {})[fset] = entry
    with open(RECORD, 'w') as f:
        json.dump(record, f, indent=2, sort_keys=False)
        f.write('\n')
    say(f'recorded in {os.path.relpath(RECORD, ROOT)}; the rows for the Sizes table in bench/RESULTS.md, with the '
        f'sizes this replaced as "Before":')
    files = {'tool': tool[0], 'c_shared': shared[0], 'c_static': static[0]}
    for row in table_rows(fset, entry, was, {k: os.path.basename(v) for k, v in files.items()}):
        print(row)
    return 0


def table_rows(fset, entry, was, names):
    """The set's rows for the Sizes table in bench/RESULTS.md, in its columns and order. `was` is the entry this
    measurement replaced; with none (a set measured the first time) the two "Before" cells are left empty."""
    builds = [('wasm', 'WebAssembly module'), ('tool', f'Native tool `{names["tool"]}`'),
              ('c_shared', f'C library, shared (`{names["c_shared"]}`)'),
              ('c_static', f'C library, static (`{names["c_static"]}`)')]
    rows = []
    for key, build in builds:
        now, before = entry[key], (was or {}).get(key)
        label = f'{fset} (stands in for "web")' if key == 'wasm' and fset == WEB != 'web' else fset
        if before is None:
            then, change = '', ''
        elif before == now:
            then, change = 'the same', '0'
        else:
            then = f'{before["raw"]:,}, {before["gzip"]:,}'
            change = f'{100 * (now["gzip"] / before["gzip"] - 1):+.1f}%'
        rows.append(f'| {build} | {label} | {now["raw"]:,} | {now["gzip"]:,} | {then} | {change} |')
    return rows


def load_record():
    if not os.path.exists(RECORD):
        return None
    with open(RECORD) as f:
        return json.load(f)


def check(fset):
    record = load_record()
    entry = (record or {}).get('builds', {}).get(fset)
    if entry is None:
        sys.exit(f'sizes: no recorded size for the feature set "{fset}"; run `bench/sizes.py measure --features {fset}`')
    wasm_opt = wasm_opt_version()
    if wasm_opt is None and entry.get('wasm_opt'):
        # The module built here would not be the one the record measures (nor the one the package ships).
        print(f'FAIL: the size was recorded with {entry["wasm_opt"]}, and {NO_WASM_OPT}, and check again.')
        return 1
    module_args = cargo_features(fset, 'gpress-wasm', declared_features())
    with tempfile.TemporaryDirectory() as tmp:
        now = build_module(module_args, os.path.join(tmp, 'gpress.wasm'))
    was = entry['wasm']
    limit = int(was['gzip'] * GROWTH)
    floor = int(was['gzip'] * SHRINK)
    target = f'; target {TARGET_GZIP:,}' if fset == WEB else ''
    print(f'WebAssembly module, feature set "{fset}": {now["raw"]:,} bytes raw, {now["gzip"]:,} gzip '
          f'(recorded {was["raw"]:,} and {was["gzip"]:,}, {100 * (now["gzip"] / was["gzip"] - 1):+.1f}% gzip; '
          f'limit {limit:,}, re-record under {floor:,}{target})')
    if wasm_opt != entry.get('wasm_opt'):
        print(f'note: recorded with {entry.get("wasm_opt") or "no wasm-opt"}, built now with {wasm_opt or "no wasm-opt"}')
    rustc = tool_version(['rustc', '--version'])
    if rustc != entry.get('rustc'):
        print(f'note: recorded with {entry.get("rustc")}, built now with {rustc}')
    failed = False
    if now['gzip'] > limit:
        print(f'FAIL: the module grew more than {round(100 * (GROWTH - 1))}% over the recorded gzip size. If the '
              f'growth is wanted, record the new size with `python3 -I bench/sizes.py measure --features {fset}`.')
        failed = True
    if fset == WEB and now['gzip'] > TARGET_GZIP:
        print(f'FAIL: the web module is over its {TARGET_GZIP:,}-byte gzip target')
        failed = True
    if now['gzip'] < floor:
        # Not a failure (a newer rustc or another change may have shrunk it), but the budget would now allow
        # more than 5% growth over the real size. In GitHub Actions the line is a warning on the run's page.
        lead = '::warning::' if os.environ.get('GITHUB_ACTIONS') == 'true' else 'warning: '
        print(f'{lead}the module is {100 * (1 - now["gzip"] / was["gzip"]):.1f}% under its recorded gzip size, so '
              f'the budget allows {100 * (limit / now["gzip"] - 1):.1f}% growth instead of '
              f'{round(100 * (GROWTH - 1))}%. Record the new size with '
              f'`python3 -I bench/sizes.py measure --features {fset}`.')
    if not failed:
        print('ok')
    return 1 if failed else 0


def main():
    ap = argparse.ArgumentParser(description=__doc__.split('\n\n')[0])
    ap.add_argument('command', choices=['measure', 'check'])
    ap.add_argument('--features', default=WEB, metavar='SET',
                    help=f'"default" or cargo features separated by commas (default: the web set, "{WEB}")')
    a = ap.parse_args()
    return measure(a.features) if a.command == 'measure' else check(a.features)


if __name__ == '__main__':
    sys.exit(main())
