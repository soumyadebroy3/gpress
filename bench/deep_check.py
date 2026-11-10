#!/usr/bin/env python3
"""Deep check: what a change does to the engine's outputs, measured on two builds of the tool side by side.

BASE is the build the change starts from (main's), HEAD the build with the change. The "Deep check" workflow
(.github/workflows/deep-check.yml) runs these commands on every pull request and posts the reports as one
comment; nothing here needs GitHub, so the same commands work on any machine with both binaries.

    python3 -I bench/deep_check.py pdf      --base BIN --head BIN --out DIR
    python3 -I bench/deep_check.py archive  --base BIN --head BIN --out DIR
    python3 -I bench/deep_check.py hostile  --base BIN --head BIN --out DIR
    python3 -I bench/deep_check.py all      --base BIN --head BIN --out DIR   the three, then DIR/report.md
    python3 -I bench/deep_check.py selftest                                   plant each finding; it must be caught

pdf      Both builds over every corpus file as a normal PDF: `benchmark.py time` at efforts 0, 50 and 100 with
         one thread, the two builds at the same time. Then head once more with 4 threads (efforts 0 and 50):
         the bytes must be the same. Then every reader (qpdf --check, the object graph by value and PDFium in
         verify_pdf.py; Poppler and pdf.js in verify_render.py) on head's rewritten outputs whose bytes are not
         base's: the output is made again, must have the measured sha256, and is compared with the original.
archive  Both builds over the 120-file sample as archives, efforts 0, 50 and 100, one thread: each archive is
         unpacked and compared with its input byte for byte (benchmark.py does that for every row).
hostile  bench/security.py on head: the damaged archives, the hostile PDFs (one thread, then the default
         thread count) and the suite's check of itself. The hostile PDFs also on base, for the cost of each
         file on both builds.

Each part writes DIR/<part>.md (the report) and DIR/<part>.json (every row behind it, also printed as one line
`DEEP-CHECK-JSON <part> {...}`), and exits 1 when it has a finding, 0 when it has none, 2 when it could not run.

A finding is something a script can decide and that does not depend on the machine: an output larger than
base's, a file base rewrites that head leaves untouched or the other way round, an output that is no longer
exact (it does not open, an untouched file is not the input, an archive does not restore), a file that
fails on head only, other bytes with other thread counts, a reader that shows a changed output differently
from the original, a failure in the hostile suite, a checking tool that is missing or does not work. Each
needs a reviewer: some are the point of the change (a new refusal is a file "newly left untouched").
Outputs that merely differ from base's are counted and listed, not findings. CPU time and peak memory are
printed as `benchmark.py compare` gives them; on a shared machine they are an indication, not a verdict.

A reader counts for a file only when it drew at least one page of the original. When a reader disagrees, the
original is compared with itself once: what a reader says of the original against itself too is not about the
output and is dropped, and the file is "unverified by" that reader; anything else it says stands. A reader that
timed out or could not open the original leaves the file "unverified by" it as well.

Needs what the benchmark needs (docs/tasks/README.md, step 2): the corpus ($CORPUS, bench/fetch_corpus.sh),
qpdf, xz, brotli, zstd, a C compiler, jpegtran, Poppler, pikepdf, pypdfium2, Pillow, and pdf.js with
@napi-rs/canvas ($PDFJS_MODULES); bench/checkers/ pins the versions the workflow installs. And jbig2dec: without
it verify_pdf.py finds a JBIG2 image's pixels different in a file compared with itself. Run with python3 -I: the
corpus is untrusted.
"""
import argparse, collections, concurrent.futures, json, os, resource, shutil, subprocess, sys, tempfile, time

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)  # python3 -I leaves the script's directory out of the path
import benchmark as bm  # noqa: E402
import verify_render as vr  # noqa: E402

BENCH, SECURITY, VERIFY_PDF = (os.path.join(HERE, n) for n in ('benchmark.py', 'security.py', 'verify_pdf.py'))
EFFORTS = [0, 50, 100]
THREADS_AGAIN, THREAD_EFFORTS = 4, [0, 50]  # head once more with this many threads, at these efforts
VERIFY_CAP = 200  # changed outputs that go through the readers; the report says how many of how many
VERIFY_WORKERS = 3
VERIFY_TIMEOUT = 600  # seconds for verify_pdf.py on one pair (verify_render.py has its own limits)
VERIFY_MEMORY = 6 << 30  # address space of verify_pdf.py and what it starts
CONTROL = 'pdfjs/tracemonkey.pdf'  # every reader must draw this one, or the readers do not work here
LIST_CAP = 30  # rows of one table in the report; the JSON holds them all
COST_FACTOR, COST_CPU_S, COST_MIB = 1.25, 1.0, 64  # a hostile file is listed when head costs this much more than base
WORKS = ('OK', 'SMALLER', 'UNTOUCHED')
# The groups a file can fall into, in report order: key, heading, finding or not.
GROUPS = [
    ('larger', 'Rewritten and larger than base\'s', True),
    ('left_untouched', 'Newly left untouched (base rewrites it, head returns the input)', True),
    ('rewritten', 'Newly rewritten (base leaves it untouched)', True),
    ('not_exact', 'No longer exact', True),
    ('failing', 'Fails on head only', True),
    ('missing', 'Not measured on head', True),
    ('reason', 'Untouched on both, for another reason', False),
    ('fixed', 'Works on head only', False),
    ('changed', 'Other bytes than base\'s', False),
]
NOT_CHECKED = """#### Not checked here

Files of shapes the corpus does not hold (new parsing code needs hand-built files), new hostile shapes beyond
the suite's, the fuzzing in `bench/security.py`, the C library and the WebAssembly module (CI builds and tests
both; their outputs are not compared with base's), archives of the files outside the 120-file sample, the cost
of threads (CPU time with N threads against one), and whether the description's sentences are true."""


def say(msg):
    print(f'deep check: {msg}', file=sys.stderr, flush=True)


# ----------------------------------------------------------------------------------------------------
# The rules: two builds' rows of one setting

def entry(group, f, x, y):
    why = {'left_untouched': y.get('details', ''), 'rewritten': 'base: ' + x.get('details', ''), 'not_exact': y.get('check', ''),
           'failing': f"{y.get('status')} {y.get('stderr', '')}"[:160].strip(),
           'reason': f"{x.get('details', '')} -> {y.get('details', '')}"}.get(group, '')
    e = {'file': f, 'base': [x.get('status'), x.get('bytes_out')], 'head': [y.get('status'), y.get('bytes_out')], 'why': why}
    return dict(e, sha256=y.get('sha256')) if group == 'changed' else e


def judge(base, head):
    """One setting's rows of both builds -> the files of each group of GROUPS, and each side's outcome counts."""
    g = {k: [] for k, _, _ in GROUPS}
    b, h = {r['file']: r for r in base}, {r['file']: r for r in head}
    for f, x in b.items():
        y = h.get(f)
        if y is None:
            g['missing'].append({'file': f, 'base': [x.get('status'), x.get('bytes_out')], 'head': [None, None], 'why': ''})
            continue
        ok_x, ok_y = x.get('status') in WORKS, y.get('status') in WORKS
        if not ok_y:
            if ok_x:
                g['failing'].append(entry('failing', f, x, y))
            continue
        if not ok_x:
            g['fixed'].append(entry('fixed', f, x, y))
            g['changed'].append(entry('changed', f, x, y))
            continue
        if x.get('sha256') != y.get('sha256'):
            g['changed'].append(entry('changed', f, x, y))
        if x['status'] != y['status']:
            k = 'left_untouched' if y['status'] == 'UNTOUCHED' else 'rewritten'
            g[k].append(entry(k, f, x, y))
        elif y.get('bytes_out', 0) > x.get('bytes_out', 0):
            g['larger'].append(entry('larger', f, x, y))
        elif x['status'] == 'UNTOUCHED' and x.get('details') != y.get('details'):
            g['reason'].append(entry('reason', f, x, y))
        if x.get('exact') and not y.get('exact'):
            g['not_exact'].append(entry('not_exact', f, x, y))
    both = [f for f in b if f in h and b[f].get('status') in WORKS and h[f].get('status') in WORKS]
    return {'files': len(b), 'groups': g,
            'same': sum(1 for f in both if b[f].get('sha256') == h[f].get('sha256')),
            'counts': [dict(collections.Counter(r.get('status') for r in rows.values())) for rows in (b, h)],
            'bytes_out': [sum(rows[f].get('bytes_out', 0) for f in both) for rows in (b, h)]}


def threads_differ(one, more):
    """Head's rows of one setting with one thread and with more: the files whose bytes or outcome differ."""
    m = {r['file']: r for r in more}
    return sorted(r['file'] for r in one if r['file'] in m and (r.get('status'), r.get('sha256')) != (m[r['file']].get('status'), m[r['file']].get('sha256')))


def findings(settings):
    """How many files of each finding group, over all settings (a file counts once per setting)."""
    return {k: sum(len(s['groups'][k]) for s in settings.values()) for k, _, bad in GROUPS if bad}


# ----------------------------------------------------------------------------------------------------
# The readers on one original and one output

def limit_memory():
    resource.setrlimit(resource.RLIMIT_AS, (VERIFY_MEMORY, VERIFY_MEMORY))


def readers(orig, out):
    """Every reader on a pair -> {'problems': [[reader, text]], 'unverified': [reader], 'pages': {reader: n}}."""
    problems, unverified, pages = [], set(), {}
    try:
        p = subprocess.run([sys.executable, '-I', VERIFY_PDF, orig, out, '--pages', '30'], capture_output=True, text=True,
                           timeout=VERIFY_TIMEOUT, preexec_fn=limit_memory)
        v = json.loads(p.stdout.strip().splitlines()[-1])
    except (subprocess.TimeoutExpired, ValueError, IndexError):
        v = None
        unverified.update(('qpdf', 'objects', 'PDFium'))
    if v:
        if not (v['qpdf_check'] in (0, 3) or v['qpdf_check'] == v['qpdf_check_input']):
            problems.append(['qpdf', f"qpdf --check exit {v['qpdf_check']} (the original: {v['qpdf_check_input']})"])
        problems += [['objects', s] for s in v.get('object_problems') or []]
        problems += [['PDFium', s] for s in v.get('render_problems') or []]
        pages['PDFium'] = v.get('pages_checked') or 0
    r = vr.compare(orig, out)
    problems += [['Poppler' if s.startswith('poppler') else 'pdf.js', s] for s in r['problems']]
    pages['Poppler'], pages['pdf.js'] = r['poppler_pages'], r['pdfjs_pages']
    unverified.update('Poppler' if s.startswith('poppler') else 'pdf.js' for s in r['notes'])
    unverified.update(k for k in ('PDFium', 'Poppler', 'pdf.js') if not pages.get(k))
    return {'problems': problems, 'unverified': sorted(unverified), 'pages': pages}


def settle(first, again):
    """`first` is the readers on (original, output), `again` on (original, original), asked for only when
    `first` has problems: what a reader says of the original against itself too is dropped, and that reader is
    unstable on the file; every other problem stands (a real difference must not hide behind an unstable one)."""
    unstable = {k: s for k, s in reversed(again['problems'])}  # each such reader, with the first thing it said
    return dict(first, problems=[p for p in first['problems'] if p not in again['problems']], unstable=unstable,
                unverified=sorted(set(first['unverified']) | set(unstable)))


def reader_targets(settings, cap):
    """Head's rewritten outputs whose bytes are not base's, one per distinct output: outcome changes and
    growths first, then the rest spread evenly up to `cap`. -> (targets, how many there were)"""
    first, rest, seen = [], [], set()
    for name, s in settings.items():
        urgent = {e['file'] for k in ('rewritten', 'larger') for e in s['groups'][k]}
        for e in s['groups']['changed']:
            if e['head'][0] == 'SMALLER' and e['sha256'] not in seen:
                seen.add(e['sha256'])
                (first if e['file'] in urgent else rest).append({'file': e['file'], 'effort': s['effort'], 'sha256': e['sha256']})
    room = max(0, cap - len(first))
    if len(rest) > room:
        rest = [rest[i * len(rest) // room] for i in range(room)]
    return first[:cap] + rest, len(seen)


def read_one(engine, corpus, tmp, t):
    """Make one of head's outputs again and put it before every reader."""
    src, out = os.path.join(corpus, t['file']), os.path.join(tmp, t['sha256'] + '.pdf')
    try:
        subprocess.run(bm.fill(bm.DEFAULTS['ours']['pdf'], engine=engine, **{'in': src}, out=out, effort=t['effort'], threads=1),
                       capture_output=True, timeout=bm.DEFAULTS['timeout'])
        if not os.path.exists(out) or bm.sha256_file(out) != t['sha256']:
            return dict(t, problems=[['engine', 'made again, the output is not the bytes that were measured']], unverified=[], pages={})
        res = readers(src, out)
        if res['problems']:
            res = settle(res, readers(src, src))
        return dict(t, **res)
    except Exception as e:  # one pair's check must not take the report down: it is a problem of that file
        return dict(t, problems=[['check', f'failed: {type(e).__name__}: {e}'[:200]]], unverified=[], pages={})
    finally:
        if os.path.exists(out):
            os.unlink(out)


def control(corpus):
    """The readers must work before their silence means anything: each draws the control file and agrees with itself."""
    src = os.path.join(corpus, CONTROL)
    res = readers(src, src)
    bad = [f'{k}: {s}' for k, s in res['problems']] + [f'{k} drew no page' for k in res['unverified']]
    return bad


def read_changed(a, settings):
    bad = control(a.corpus)
    if bad:
        return {'control': bad, 'results': [], 'changed': 0}
    targets, n = reader_targets(settings, a.verify_cap)
    say(f'readers on {len(targets)} of {n} changed outputs')
    tmp = tempfile.mkdtemp(prefix='deep-check-')
    try:
        with concurrent.futures.ThreadPoolExecutor(VERIFY_WORKERS) as pool:
            results = list(pool.map(lambda t: read_one(a.head, a.corpus, tmp, t), targets))
    finally:
        shutil.rmtree(tmp, ignore_errors=True)
    return {'control': [], 'results': results, 'changed': n}


# ----------------------------------------------------------------------------------------------------
# Running the benchmark and the security suite

def bench_time(run, log, engine, label, a, set_name, mode, efforts, threads):
    cmd = [sys.executable, '-I', BENCH, 'time', run, '--engine', engine, '--build', label, '--corpus', a.corpus, '--set', set_name,
           '--who', 'ours', '--modes', mode, '--efforts', ','.join(str(e) for e in efforts), '--threads', str(threads), '--verify', '0']
    if a.limit:
        cmd += ['--limit', str(a.limit)]
    return subprocess.Popen(cmd, stdout=open(os.path.join(a.out, log), 'w'), stderr=subprocess.STDOUT)


def measure_both(a, part, set_name, mode):
    """`benchmark.py time` for base and head at the same time -> (the two run directories, what went wrong)."""
    runs = [os.path.join(a.out, f'{part}-{side}') for side in ('base', 'head')]
    for run, engine in zip(runs, (a.base, a.head)):  # benchmark.py continues a run it finds, whatever build made it
        meta = os.path.join(run, 'meta.json')
        if os.path.exists(meta) and json.load(open(meta)).get('engine_sha256') != bm.sha256_file(engine):
            say(f'{run} holds the rows of another build: give --out an empty directory')
            sys.exit(2)
    say(f'{part}: both builds over {set_name}, efforts {a.efforts}')
    ps = [bench_time(run, f'{part}-{side}.log', engine, label, a, set_name, mode, a.efforts, 1)
          for run, side, engine, label in zip(runs, ('base', 'head'), (a.base, a.head), (a.base_label, a.head_label))]
    return runs, [f'benchmark.py time failed for {side} (exit {rc}): see {part}-{side}.log' for side, rc in zip(('base', 'head'), [p.wait() for p in ps]) if rc != 0]


def load_settings(runs, mode, efforts):
    settings = {}
    for e in efforts:
        name = bm.config_name('ours', mode, e, 1)
        base, head = (bm.load_rows(os.path.join(run, name + '.jsonl')) for run in runs)
        settings[name] = dict(judge(base, head), effort=e)
    return settings


def machine(runs):
    """What the report says about the two measurements, from their meta.json; missing tools are a finding."""
    metas = [json.load(open(p)) if os.path.exists(p) else {} for p in (os.path.join(run, 'meta.json') for run in runs)]
    missing = sorted(set().union(*(m['missing_tools'] if 'missing_tools' in m else ['meta.json (a measurement did not start)'] for m in metas)))
    missing += [t for t in ('pdftoppm', 'pdftotext', 'node', 'jbig2dec') if not shutil.which(t)]
    m = metas[1]
    return {'missing_tools': missing, 'cpus': m.get('cpus'), 'cpu_model': m.get('cpu_model'), 'tools': m.get('tools'),
            'binaries': [x.get('engine_sha256') for x in metas], 'builds': [x.get('build') for x in metas],
            'calibration_cpu_s': [(x.get('calibration') or {}).get('cpu_s') for x in metas]}


def compare_text(runs):
    p = subprocess.run([sys.executable, '-I', BENCH, 'compare', *runs], capture_output=True, text=True)
    return (p.stdout + p.stderr).strip()


def part_pdf(a):
    t0 = time.time()
    runs, errors = measure_both(a, 'pdf', a.set or 'all', 'pdf')
    settings = load_settings(runs, 'pdf', a.efforts)
    thread_efforts = [e for e in THREAD_EFFORTS if e in a.efforts]
    say(f'pdf: head again with {THREADS_AGAIN} threads, efforts {thread_efforts}')
    if thread_efforts and bench_time(runs[1], 'pdf-head-threads.log', a.head, a.head_label, a, a.set or 'all', 'pdf', thread_efforts, THREADS_AGAIN).wait() != 0:
        errors.append('benchmark.py time failed for head with more threads: see pdf-head-threads.log')
    threads = {}
    for e in thread_efforts:
        one, more = (bm.load_rows(os.path.join(runs[1], bm.config_name('ours', 'pdf', e, t) + '.jsonl')) for t in (1, THREADS_AGAIN))
        threads[f'effort {e}'] = {'files': len(more), 'differ': threads_differ(one, more)}
        if len(more) != len(one):
            errors.append(f'effort {e}: {len(more)} files measured with {THREADS_AGAIN} threads, {len(one)} with one')
    res = {'part': 'pdf', 'errors': errors, 'settings': settings, 'threads': threads, 'machine': machine(runs),
           'compare': compare_text(runs), 'readers': read_changed(a, settings)}
    res['seconds'] = round(time.time() - t0)
    return res


def part_archive(a):
    t0 = time.time()
    runs, errors = measure_both(a, 'archive', a.set or 'sample120', 'archive')
    return {'part': 'archive', 'errors': errors, 'settings': load_settings(runs, 'archive', a.efforts), 'machine': machine(runs),
            'compare': compare_text(runs), 'seconds': round(time.time() - t0)}


def read_tsv(path):
    lines = [l.split('\t') for l in open(path).read().splitlines() if l] if os.path.exists(path) else []
    return [dict(zip(lines[0], l)) for l in lines[1:]]


def costlier(base, head):
    """Hostile files that cost head clearly more than base, in CPU time or peak memory -> [file, op, what]"""
    b = {(r['file'], r['op']): r for r in base if 'cpu_s' in r}
    out = []
    for r in head:
        x = b.get((r.get('file'), r.get('op')))
        if x is None or 'cpu_s' not in r:
            continue
        for key, unit, floor in (('cpu_s', 's of CPU', COST_CPU_S), ('maxrss_mib', 'MiB', COST_MIB)):
            was, now = float(x[key]), float(r[key])
            if now >= floor and now > was * COST_FACTOR:
                out.append([r['file'], r['op'], f'{was:g} -> {now:g} {unit}'])
    return out


def part_hostile(a):
    t0 = time.time()
    runs = {side: os.path.join(a.out, f'hostile-{side}') for side in ('base', 'head')}
    steps = [('archives', 'head'), ('pdfs', 'head'), ('selfcheck', 'head'), ('pdfs', 'base')]
    failed = []
    with open(os.path.join(a.out, 'hostile.log'), 'w') as log:
        for mode, side in steps:
            say(f'hostile: {mode} on {side}')
            engine, label = (a.head, a.head_label) if side == 'head' else (a.base, a.base_label)
            rc = subprocess.run([sys.executable, '-I', SECURITY, mode, runs[side], '--engine', engine, '--corpus', a.corpus, '--build', label],
                                stdout=log, stderr=subprocess.STDOUT).returncode
            if rc != 0:
                failed.append([mode, side, rc])
    tables = {}
    for name in ('archives', 'pdfs', 'threads0-pdfs', 'selfcheck'):
        rows = read_tsv(os.path.join(runs['head'], name + '.tsv'))
        bad = [r for r in rows if r.get('result') == 'FAIL' or r.get('ok') == 'BAD']
        tables[name] = {'rows': len(rows), 'failures': [[r.get('file') or r.get('check') or r.get('seed'), r.get('op') or r.get('kind') or '', r.get('verdict') or r.get('outcome') or ''] for r in bad],
                        'skipped': rows[0].get('detail') if rows and rows[0].get('status') == 'skipped' else None}
    cost = [c for name in ('pdfs', 'threads0-pdfs') for c in costlier(read_tsv(os.path.join(runs['base'], name + '.tsv')), read_tsv(os.path.join(runs['head'], name + '.tsv')))]
    return {'part': 'hostile', 'errors': [f'security.py {m} on head ended with exit {rc}: see hostile.log' for m, side, rc in failed if side == 'head'],
            'base_failed': [m for m, side, _ in failed if side == 'base'], 'tables': tables, 'costlier': cost, 'seconds': round(time.time() - t0)}


# ----------------------------------------------------------------------------------------------------
# The report

def red(res):
    """The findings of a part, as short phrases; empty when there are none."""
    out = list(res.get('errors') or [])
    if any(not s['files'] for s in (res.get('settings') or {}).values()) or any(not t['rows'] for t in (res.get('tables') or {}).values()):
        out.append('a setting or a suite measured nothing')  # silence must not read as "no finding"
    names = dict((k, title) for k, title, _ in GROUPS)
    for k, n in findings(res.get('settings') or {}).items():
        if n:
            out.append(f'{n} x {names[k].lower()}')
    out += [f'{len(t["differ"])} outputs differ with {THREADS_AGAIN} threads ({e})' for e, t in (res.get('threads') or {}).items() if t['differ']]
    if 'machine' in res and res['machine']['missing_tools']:
        out.append('checking tools missing: ' + ', '.join(res['machine']['missing_tools']))
    rd = res.get('readers')
    if rd:
        if rd['control']:
            out.append('the readers do not work here: ' + '; '.join(rd['control'])[:300])
        n = sum(1 for r in rd['results'] if r['problems'])
        if n:
            out.append(f'{n} changed outputs a reader shows differently from the original')
    return out


def cell(s):
    return str(s).replace('|', '\\|').replace('\n', ' ')


def table(head, rows):
    return ['| ' + ' | '.join(head) + ' |', '|' + '---|' * len(head)] + ['| ' + ' | '.join(cell(c) for c in r) + ' |' for r in rows]


def side(pair):
    return '-' if pair[0] is None else f'{pair[0]} {pair[1]:,}' if pair[1] is not None else str(pair[0])


def pct(a, b):
    return f'{(b - a) * 100 / a:+.2f}%' if a else ''


def render_settings(res, what):
    L = []
    rows = []
    for name, s in res['settings'].items():
        cb, ch = s['counts']
        rows.append([s['effort'], s['files'], s['same'], len(s['groups']['changed'])] +
                    ([f"{cb.get('SMALLER', 0)} -> {ch.get('SMALLER', 0)}", f"{cb.get('UNTOUCHED', 0)} -> {ch.get('UNTOUCHED', 0)}"] if what == 'pdf' else []) +
                    [f"{s['bytes_out'][0]:,} -> {s['bytes_out'][1]:,} ({pct(*s['bytes_out'])})"])
    L += table(['Effort', 'Files', 'Same bytes as base', 'Other bytes'] + (['Rewritten, base -> head', 'Untouched, base -> head'] if what == 'pdf' else []) +
               ['Bytes out, base -> head'], rows) + ['']
    listed = set()
    for k, title, bad in GROUPS:
        found = [(s['effort'], e) for s in res['settings'].values() for e in s['groups'][k]]
        if k == 'changed':  # the top table counts them all; the rows above already show most
            found, title = [x for x in found if (x[0], x[1]['file']) not in listed], title + ', and in no table above'
        listed.update((eff, e['file']) for eff, e in found)
        if not found:
            continue
        found.sort(key=lambda x: (x[1]['file'], x[0]))
        L += [f"#### {title}: {len(found)}" + ('' if bad else ' (not a finding)'), '']
        L += table(['File', 'Effort', 'Base', 'Head', 'Why'], [[f"`{e['file']}`", eff, side(e['base']), side(e['head']), e['why']] for eff, e in found[:LIST_CAP]])
        if len(found) > LIST_CAP:
            L.append(f'... and {len(found) - LIST_CAP} more, in the JSON.')
        L.append('')
    return L


def verdict(res):
    bad = red(res)
    return ['**Needs a reviewer: ' + '; '.join(bad) + '.**' if bad else '**No finding.**', '']


def tail(res):
    m = res['machine']
    cal = ', '.join(f'{c:.2f} s' if c else 'none' for c in m['calibration_cpu_s'])
    return ['<details><summary>benchmark.py compare base head (on a shared machine its CPU time and peak memory lines are an indication, not a verdict)</summary>', '',
            '```text', res['compare'], '```', '</details>', '',
            f"Base `{m['builds'][0]}` (binary sha256 `{str(m['binaries'][0])[:16]}`), head `{m['builds'][1]}` (`{str(m['binaries'][1])[:16]}`). "
            f"{m['cpus']} CPUs, {m['cpu_model']}; calibration {cal} of CPU (base, head). Took {res['seconds']} s.", '']


def render_pdf(res):
    n = next(iter(res['settings'].values()))['files'] if res['settings'] else 0
    L = [f"### Normal PDF: {n} files, efforts {', '.join(str(s['effort']) for s in res['settings'].values())}, one thread", ''] + verdict(res)
    L += render_settings(res, 'pdf')
    for e, t in res['threads'].items():
        L.append(f"Same bytes with 1 and {THREADS_AGAIN} threads, {e}: {t['files'] - len(t['differ'])} of {t['files']}" +
                 (': ' + ', '.join(f'`{f}`' for f in t['differ'][:LIST_CAP]) if t['differ'] else '') + '.')
    rd = res['readers']
    L.append('')
    if rd['control']:
        L.append('Readers: not run, they do not work on the control file: ' + cell('; '.join(rd['control'])[:500]))
    elif not rd['changed']:
        L.append('Readers: no rewritten output differs from base\'s, so none was read. Outputs with base\'s bytes are not read again here.')
    else:
        r = rd['results']
        pages = {k: sum(x['pages'].get(k, 0) for x in r) for k in ('PDFium', 'Poppler', 'pdf.js')}
        L.append(f"Readers (qpdf --check, object graph, PDFium, Poppler, pdf.js) on {len(r)} of the {rd['changed']} distinct rewritten outputs that differ from base's: "
                 f"{sum(1 for x in r if not x['problems'])} read the same as their originals, {sum(1 for x in r if x['problems'])} differently. "
                 f"Pages compared: {', '.join(f'{k} {v:,}' for k, v in pages.items())} (at most 30 per file). Outputs with base's bytes are not read again here.")
        bad = [x for x in r if x['problems']]
        if bad:
            L += ['', '#### A reader shows the output differently from the original', ''] + table(
                ['File', 'Effort', 'Reader', 'What'], [[f"`{x['file']}`", x['effort'], k, s] for x in bad[:LIST_CAP] for k, s in dict(reversed(x['problems'])).items()])  # each reader's first
        partly = [x for x in r if x['unverified'] and not x['problems']]
        if partly:
            L += ['', f"Not verified by every reader: {len(partly)} outputs. " + '; '.join(
                f"`{x['file']}` (effort {x['effort']}): {', '.join(x['unverified'])}" + ''.join(f' ({k} says of the original against itself: {cell(s)[:120]})' for k, s in (x.get('unstable') or {}).items()) for x in partly[:LIST_CAP])]
    return '\n'.join(L + [''] + tail(res) + [NOT_CHECKED, ''])


def render_archive(res):
    n = next(iter(res['settings'].values()))['files'] if res['settings'] else 0
    L = [f"### Archives: {n} files, efforts {', '.join(str(s['effort']) for s in res['settings'].values())}, one thread, each unpacked and compared with its input", ''] + verdict(res)
    return '\n'.join(L + render_settings(res, 'archive') + tail(res))


def render_hostile(res):
    t = res['tables']
    L = ['### Hostile input: bench/security.py on head', ''] + verdict(res)
    rows = [['Damaged archives', t['archives']], ['Hostile PDFs, one thread', t['pdfs']], ['Hostile PDFs, default thread count', t['threads0-pdfs']], ['The suite checking itself (planted failures)', t['selfcheck']]]
    L += table(['Part', 'Cases', 'Failures'], [[name, 'skipped: ' + x['skipped'] if x['skipped'] else x['rows'], len(x['failures'])] for name, x in rows]) + ['']
    bad = [[name] + f for name, x in rows for f in x['failures']]
    if bad:
        L += table(['Part', 'Case', 'Step', 'Verdict'], bad[:LIST_CAP]) + ['']
    if res['base_failed']:
        L += ['The hostile PDFs fail on base too (not counted here).', '']
    if res['costlier']:
        L += [f'#### Costs head over {COST_FACTOR}x what it costs base (a look, not a finding)', ''] + table(['File', 'Step', 'Base -> head'], res['costlier'][:LIST_CAP]) + ['']
    else:
        L += [f'No hostile PDF costs head over {COST_FACTOR}x base\'s CPU time or peak memory.', '']
    return '\n'.join(L + [f"Not run here: the suite's fuzzing, its C library pass and its supply-chain part. Took {res['seconds']} s.", ''])


PARTS = {'pdf': (part_pdf, render_pdf), 'archive': (part_archive, render_archive), 'hostile': (part_hostile, render_hostile)}


def run_part(a, name):
    run, render = PARTS[name]
    res = run(a)
    res['findings'] = red(res)
    with open(os.path.join(a.out, name + '.md'), 'w') as f:
        f.write(render(res))
    with open(os.path.join(a.out, name + '.json'), 'w') as f:
        json.dump(res, f, indent=1)
    print(f'DEEP-CHECK-JSON {name} ' + json.dumps(res, separators=(',', ':')), flush=True)
    say(f"{name}: {'; '.join(res['findings']) or 'no finding'} ({res['seconds']} s)")
    return 1 if res['findings'] else 0


# ----------------------------------------------------------------------------------------------------
# The self-test: every finding planted once

def selftest():
    def row(f, status='SMALLER', n=100, sha=None, exact=True, details=''):
        return {'file': f, 'status': status, 'bytes_out': n, 'sha256': sha or f'{f}-{status}-{n}', 'exact': exact, 'details': details}
    base = [row('same'), row('smaller'), row('larger'), row('refused'), row('accepted', 'UNTOUCHED', details='PDF/A'), row('opens'),
            row('breaks'), row('gone'), row('reason', 'UNTOUCHED', sha='r', details='PDF/A'), row('mended', 'ERROR')]
    head = [row('same'), row('smaller', n=90), row('larger', n=101), row('refused', 'UNTOUCHED', 120, details='keeps earlier revisions'),
            row('accepted', 'SMALLER', 80), row('opens', sha='other', exact=False), row('breaks', 'PANIC'),
            row('reason', 'UNTOUCHED', sha='r', details='PDF/X'), row('mended')]
    s = judge(base, head)
    got = {k: [e['file'] for e in v] for k, v in s['groups'].items()}
    want = {'larger': ['larger'], 'left_untouched': ['refused'], 'rewritten': ['accepted'], 'not_exact': ['opens'], 'failing': ['breaks'],
            'missing': ['gone'], 'reason': ['reason'], 'fixed': ['mended'], 'changed': ['smaller', 'larger', 'refused', 'accepted', 'opens', 'mended']}
    assert got == want, got
    assert s['same'] == 2 and s['files'] == 10, s
    assert not any(findings({'x': judge(base, base)}).values()), 'a build against itself has a finding'
    assert not any(findings({'x': judge(base[:2], head[:2])}).values()), 'a smaller output is a finding'
    assert threads_differ([row('a'), row('b')], [row('a'), row('b', sha='x')]) == ['b']
    # The readers: what one says of the original against itself too is dropped, everything else stands.
    first = {'problems': [['Poppler', 'poppler pages differ'], ['PDFium', 'page 1 renders differently']], 'unverified': [], 'pages': {}}
    settled = settle(first, {'problems': [['Poppler', 'poppler pages differ']]})
    assert settled['problems'] == [['PDFium', 'page 1 renders differently']] and settled['unstable'] == {'Poppler': 'poppler pages differ'}, settled
    assert settle(first, {'problems': []})['problems'] == first['problems']
    assert settle(first, {'problems': [['Poppler', 'poppler text differs']]})['problems'] == first['problems'], 'another problem of the same reader stands'
    # Which outputs are read: one per distinct output, outcome changes and growths first, at most the cap.
    targets, n = reader_targets({'a': dict(s, effort=0), 'b': dict(s, effort=50)}, 3)
    assert n == 5 and [t['file'] for t in targets[:2]] == ['larger', 'accepted'] and len(targets) == 3, (targets, n)
    assert costlier([{'file': 'f', 'op': 'pack', 'cpu_s': '2.0', 'maxrss_mib': '100'}], [{'file': 'f', 'op': 'pack', 'cpu_s': '2.6', 'maxrss_mib': '120'}]) == [['f', 'pack', '2 -> 2.6 s of CPU']]
    # A report from planted results says what it must.
    res = {'part': 'pdf', 'errors': [], 'settings': {'a': dict(s, effort=0)}, 'threads': {'effort 0': {'files': 2, 'differ': ['b']}}, 'seconds': 1, 'compare': 'PASS',
           'machine': {'missing_tools': ['qpdf'], 'cpus': 4, 'cpu_model': 'x', 'builds': ['b', 'h'], 'binaries': ['1', '2'], 'calibration_cpu_s': [1.0, None]},
           'readers': {'control': [], 'changed': 1, 'results': [{'file': 'opens', 'effort': 0, 'problems': [['pdf.js', 'pdf.js text differs']], 'unverified': [], 'pages': {'PDFium': 1}}]}}
    text, bad = render_pdf(res), red(res)
    assert len(bad) == 9, bad
    assert text.count('| `larger` |') == 1 and text.count('| `smaller` |') == 1, 'a changed file is listed once'
    for word in ('Needs a reviewer', 'Newly left untouched', '`refused`', 'keeps earlier revisions', 'pdf.js text differs', 'checking tools missing: qpdf', 'Not checked here'):
        assert word in text, word
    res.update(settings={'a': dict(judge(base[:2], head[:2]), effort=0)}, threads={}, readers={'control': [], 'changed': 0, 'results': []})
    res['machine']['missing_tools'] = []
    assert not red(res) and '**No finding.**' in render_pdf(res)
    assert red(dict(res, settings={'a': dict(judge([], []), effort=0)})) == ['a setting or a suite measured nothing']
    assert red({'tables': {'pdfs': {'rows': 0, 'failures': [], 'skipped': None}}}) == ['a setting or a suite measured nothing']
    print('deep check selftest: every planted finding is caught, and a clean run has none')


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument('part', choices=[*PARTS, 'all', 'selftest'])
    ap.add_argument('--base', help='the base build of the tool (main\'s)')
    ap.add_argument('--head', help='the head build of the tool (with the change)')
    ap.add_argument('--out', help='directory for the runs and the reports')
    ap.add_argument('--corpus', default=bm.DEFAULTS['corpus'], help='the corpus (default: $CORPUS, else bench/corpus)')
    ap.add_argument('--base-label', default='base', help='what the report calls the base build (a commit)')
    ap.add_argument('--head-label', default='head', help='what the report calls the head build (a commit)')
    ap.add_argument('--efforts', default=','.join(str(e) for e in EFFORTS), help='efforts to measure (default 0,50,100)')
    ap.add_argument('--set', help='the list of files (default: all for pdf, sample120 for archive)')
    ap.add_argument('--limit', type=int, help='only the first N files of the list (a quick look)')
    ap.add_argument('--verify-cap', type=int, default=VERIFY_CAP, help=f'changed outputs put before the readers (default {VERIFY_CAP})')
    a = ap.parse_args()
    if a.part == 'selftest':
        return selftest()
    if not (a.base and a.head and a.out):
        ap.error('--base, --head and --out are needed')
    a.base, a.head, a.out, a.corpus = (os.path.abspath(p) for p in (a.base, a.head, a.out, a.corpus))
    a.efforts = [int(e) for e in a.efforts.split(',')]
    for what, path in (('base build', a.base), ('head build', a.head), ('corpus (bench/fetch_corpus.sh fetches it)', os.path.join(a.corpus, CONTROL))):
        if not os.path.isfile(path):
            say(f'no {what}: {path}')
            sys.exit(2)
    os.makedirs(a.out, exist_ok=True)
    names = list(PARTS) if a.part == 'all' else [a.part]
    rc = max(run_part(a, n) for n in names)
    if a.part == 'all':
        with open(os.path.join(a.out, 'report.md'), 'w') as f:
            f.write('\n'.join(open(os.path.join(a.out, n + '.md')).read() for n in names))
    sys.exit(rc)


if __name__ == '__main__':
    main()
