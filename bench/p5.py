#!/usr/bin/env python3
"""What the threads and the store of outputs do for the normal-PDF writer, file by file.

  p5.py run    NAME --engine BIN [--set S | --files F,G] [--efforts 0,50,100] [--threads 1,2,0] [--limit N]
                    [--store-budget B] [--timeout S] [--corpus DIR]
  p5.py report NAME [--base OTHER]

`run` optimizes every file at each effort and thread count, one process at a time, with PDFE_DEBUG set, and
writes one row of JSON per run to RUN/p5-eE-tT.jsonl (RUN is bench/results/NAME when NAME has no slash) as
soon as it is done, so a run that is stopped goes on where it left off. A row holds the child's own wait4
figures (wall and CPU time, peak resident set), the SHA-256 of its output and the engine's summary of the
file, the line

  plan: S streams, J jobs (I identical to an earlier one, H sharing their input); store: budget B, peak P
  bytes, K kept, X hits, Y computed, about Z s saved; segments planned once: N

(on one line). A file the engine leaves as it is before its streams are planned has no such line. The output
is deleted before each run, and the child's standard error goes to a file, never to a pipe: the debug lines
of a big file would fill one. --limit N takes the N smallest files of the list. The files of one effort are
run in turn, each at every thread count before the next, so slow drifts of the machine hit every count alike.

`report` prints, for each effort:
- the files whose output is the same at every thread count;
- the work saved: streams not encoded again (identical to an earlier one), encodes the store answered and
  segments planned once (at the first thread count, one if it was run);
- CPU time at that count, and at each other count as a multiple of it;
- wall time, the busy share CPU / (T x wall) and the mean idle share of a file, 1 - CPU / (T x wall); these
  mean something on a quiet machine only;
- the store's peak and the peak resident set (with the floor this interpreter puts under it).
With --base, also the CPU time saved against the other run on the files where repeats were found (the base
may be an engine without the summary line; the repeats are this run's).

Run with python3 -I. The corpus is untrusted: its files are only passed to the engine (and their sizes read
for --limit); nothing from them is ever executed or parsed here."""
import argparse, hashlib, json, os, re, resource, subprocess, sys, tempfile, threading, time

HERE = os.path.dirname(os.path.abspath(__file__))
RESULTS = os.path.join(HERE, 'results')
SETS = os.path.join(HERE, 'sets')
CORPUS = os.environ.get('CORPUS') or os.path.join(HERE, 'corpus')

SUMMARY = re.compile(
    rb'^plan: (\d+) streams, (\d+) jobs \((\d+) identical to an earlier one, (\d+) sharing their input\); '
    rb'store: budget (\d+), peak (\d+) bytes, (\d+) kept, (\d+) hits, (\d+) computed, about ([0-9.]+) s saved; '
    rb'segments planned once: (\d+)\s*$')
FIELDS = ['streams', 'jobs', 'identical', 'sharing', 'budget', 'peak', 'kept', 'hits', 'computed', 'saved',
          'segments']
CONFIG = re.compile(r'^p5-e(\d+)-t(\d+)\.jsonl$')


def numbers(text):
    return [int(x) for x in text.split(',') if x.strip()]


def read_list(name):
    """The files of a list: a name in bench/sets/ or a path; blank lines and lines starting with # are left out."""
    path = name if os.path.sep in name or os.path.exists(name) else os.path.join(SETS, name + '.txt')
    with open(path) as f:
        return [l.strip() for l in f if l.strip() and not l.strip().startswith('#')]


def run_dir(name):
    return name if os.path.sep in name else os.path.join(RESULTS, name)


def sha256_file(path):
    h = hashlib.sha256()
    with open(path, 'rb') as f:
        for block in iter(lambda: f.read(1 << 20), b''):
            h.update(block)
    return h.hexdigest()


def load_rows(path):
    rows = []
    if os.path.exists(path):
        with open(path) as f:
            for l in f:
                try:
                    rows.append(json.loads(l))
                except ValueError:
                    pass  # a row cut off when the run was stopped: that file is run again
    return rows


def summary_of(err_path):
    """The engine's last summary line in a file of its standard error, as a dict (None without one)."""
    found = None
    with open(err_path, 'rb') as f:
        for line in f:
            if line.startswith(b'plan: '):
                m = SUMMARY.match(line)
                if m:
                    found = m
    if not found:
        return None
    values = [float(x) if k == 'saved' else int(x) for k, x in zip(FIELDS, found.groups())]
    return dict(zip(FIELDS, values))


def tail(path, n=300):
    with open(path, 'rb') as f:
        f.seek(0, os.SEEK_END)
        f.seek(max(0, f.tell() - n))
        return f.read().decode('utf-8', 'replace')


def run_one(engine, src, out, err_path, effort, threads, budget, timeout):
    """One optimize: the child's return code, wall, CPU and peak memory, and what it wrote."""
    if os.path.exists(out):
        os.remove(out)
    cmd = [engine, 'optimize', src, out, '--effort', str(effort), '--threads', str(threads)]
    if budget is not None:
        cmd += ['--store-budget', str(budget)]
    env = dict(os.environ, PDFE_DEBUG='1')
    timed_out = []
    with open(err_path, 'wb') as err:
        t0 = time.perf_counter()
        p = subprocess.Popen(cmd, stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL, stderr=err, env=env)

        def kill():
            timed_out.append(True)
            try:
                p.kill()
            except OSError:
                pass

        timer = threading.Timer(timeout, kill)
        timer.start()
        try:
            _, status, ru = os.wait4(p.pid, 0)
        finally:
            timer.cancel()
        wall = time.perf_counter() - t0
    rc = os.waitstatus_to_exitcode(status)
    p.returncode = rc  # so Popen does not wait a second time
    row = {'rc': rc, 'timed_out': bool(timed_out), 'wall': round(wall, 4), 'cpu': round(ru.ru_utime + ru.ru_stime, 4),
           'rss_kb': ru.ru_maxrss, 'summary': summary_of(err_path)}
    if rc == 0 and os.path.exists(out):
        row['out_bytes'] = os.path.getsize(out)
        row['sha256'] = sha256_file(out)
    else:
        row['error'] = tail(err_path)
    if os.path.exists(out):
        os.remove(out)
    return row


def cmd_run(a):
    if not os.access(a.engine, os.X_OK):
        sys.exit(f'{a.engine}: not an executable')
    files = [x for x in a.files.split(',') if x] if a.files else read_list(a.set)
    path_of = lambda rel: rel if os.path.isabs(rel) else os.path.join(a.corpus, rel)
    missing = [f for f in files if not os.path.isfile(path_of(f))]
    if missing:
        sys.exit(f'{len(missing)} files not found, the first: {path_of(missing[0])}')
    if a.limit:
        files = sorted(files, key=lambda f: os.path.getsize(path_of(f)))[:a.limit]
    d = run_dir(a.name)
    os.makedirs(d, exist_ok=True)
    meta = {'engine': os.path.abspath(a.engine), 'engine_sha256': sha256_file(a.engine), 'store_budget': a.store_budget,
            'cpus': len(os.sched_getaffinity(0)), 'parent_rss_kb': resource.getrusage(resource.RUSAGE_SELF).ru_maxrss}
    meta_path = os.path.join(d, 'p5-meta.json')
    if os.path.exists(meta_path):
        with open(meta_path) as f:
            old = json.load(f)
        if (old.get('engine_sha256'), old.get('store_budget')) != (meta['engine_sha256'], meta['store_budget']):
            sys.exit(f'{d} holds rows of another engine or store budget; give the run another name')
    else:
        with open(meta_path, 'w') as f:
            json.dump(meta, f, indent=1)
    with tempfile.TemporaryDirectory(prefix='p5-') as tmp:
        out, err = os.path.join(tmp, 'out.pdf'), os.path.join(tmp, 'stderr.txt')
        for e in numbers(a.efforts):
            done = {t: {r['file'] for r in load_rows(os.path.join(d, f'p5-e{e}-t{t}.jsonl'))} for t in numbers(a.threads)}
            for rel in files:
                for t in numbers(a.threads):
                    if rel in done[t]:
                        continue
                    row = {'file': rel, 'effort': e, 'threads': t, 'in_bytes': os.path.getsize(path_of(rel))}
                    row.update(run_one(a.engine, path_of(rel), out, err, e, t, a.store_budget, a.timeout))
                    with open(os.path.join(d, f'p5-e{e}-t{t}.jsonl'), 'a') as w:
                        w.write(json.dumps(row, separators=(',', ':')) + '\n')
                    s = row['summary']
                    what = (f"{s['jobs']} jobs, {s['hits']} store hits, {s['segments']} segments planned once" if s
                            else 'no summary')
                    print(f"e{e} t{t} {rel}: rc {row['rc']}, cpu {row['cpu']:.2f} s, wall {row['wall']:.2f} s, {what}",
                          flush=True)


def load_run(name):
    """{effort: {threads: {file: row}}} and the run's meta."""
    d = run_dir(name)
    if not os.path.isdir(d):
        sys.exit(f'{d}: no such run')
    runs = {}
    for f in sorted(os.listdir(d)):
        m = CONFIG.match(f)
        if m:
            runs.setdefault(int(m[1]), {})[int(m[2])] = {r['file']: r for r in load_rows(os.path.join(d, f))}
    meta = {}
    if os.path.exists(os.path.join(d, 'p5-meta.json')):
        with open(os.path.join(d, 'p5-meta.json')) as f:
            meta = json.load(f)
    return runs, meta


def ok(r):
    return r is not None and r['rc'] == 0 and 'sha256' in r


def cmd_report(a):
    runs, meta = load_run(a.name)
    base, _ = load_run(a.base) if a.base else ({}, {})
    cpus = min(max(meta.get('cpus', 1), 1), 8)
    used = lambda t: t if t else cpus  # threads 0: what the machine offers, at most 8 (par::default_threads)
    for e in sorted(runs):
        by_t = runs[e]
        counts = sorted(by_t, key=lambda t: (t == 0, t))
        first = 1 if 1 in by_t else counts[0]
        files = sorted(set().union(*[set(x) for x in by_t.values()]))
        common = [f for f in files if all(ok(by_t[t].get(f)) for t in counts)]
        failed = [f for f in files if any(by_t[t].get(f) and not ok(by_t[t][f]) for t in counts)]
        print(f'effort {e}: {len(files)} files, threads {", ".join(map(str, counts))}'
              + (f' (threads 0 is {cpus} here)' if 0 in counts else ''))
        if failed:
            print(f'  failed or timed out at some count: {len(failed)} ({", ".join(failed[:5])})')
        if len(counts) > 1:
            differ = [f for f in common if len({by_t[t][f]['sha256'] for t in counts}) > 1]
            print(f'  the same output at every thread count: {len(common) - len(differ)} of {len(common)} files'
                  + (f'; DIFFERENT: {", ".join(differ[:10])}' if differ else ''))
        # The work saved, at the first count.
        rows = [by_t[first][f] for f in common if by_t[first][f].get('summary')]
        sums = [r['summary'] for r in rows]
        tot = {k: sum(s[k] for s in sums) for k in FIELDS}
        top = max(rows, key=lambda r: r['summary']['peak'], default=None)
        print(f'  at threads {first}: {len(sums)} files planned, {tot["streams"]} streams, {tot["jobs"]} jobs '
              f'({tot["identical"]} identical to an earlier one, {tot["sharing"]} sharing their input)')
        if sums:
            asks = tot['hits'] + tot['computed']
            print(f'  store: {tot["hits"]} hits of {asks} asks, about {tot["saved"]:.2f} s saved; segments planned once: '
                  f'{tot["segments"]}; the highest peak {top["summary"]["peak"]} bytes of a budget of '
                  f'{top["summary"]["budget"]} ({top["file"]})')
        base_cpu = sum(by_t[first][f]['cpu'] for f in common)
        for t in counts:
            cpu = sum(by_t[t][f]['cpu'] for f in common)
            wall = sum(by_t[t][f]['wall'] for f in common)
            n = used(t)
            idle = [1 - by_t[t][f]['cpu'] / (n * by_t[t][f]['wall']) for f in common if by_t[t][f]['wall'] > 0]
            rss = max((by_t[t][f]['rss_kb'] for f in common), default=0)
            print(f'  threads {t}: cpu {cpu:.2f} s ({cpu / base_cpu if base_cpu else 0:.3f}x threads {first}), '
                  f'wall {wall:.2f} s, busy {cpu / (n * wall) if wall else 0:.1%}, mean idle '
                  f'{sum(idle) / len(idle) if idle else 0:.1%} (quiet machine only), peak RSS {rss / 1024:.1f} MB')
        print(f'  (peak RSS has a floor of about {meta.get("parent_rss_kb", 0) / 1024:.0f} MB, this interpreter\'s)')
        if a.base:
            b = base.get(e, {}).get(first)
            if not b:
                print(f'  {a.base}: no rows at effort {e}, threads {first}')
                continue
            repeats = lambda s: s is not None and s['identical'] + s['hits'] + s['segments'] > 0
            rep = [f for f in common if repeats(by_t[first][f].get('summary')) and ok(b.get(f))]
            old = sum(b[f]['cpu'] for f in rep)
            new = sum(by_t[first][f]['cpu'] for f in rep)
            same = sum(b[f]['sha256'] == by_t[first][f]['sha256'] for f in rep)
            print(f'  against {a.base}, the {len(rep)} files with repeats: cpu {old:.2f} s -> {new:.2f} s, '
                  f'{old - new:+.2f} s saved ({(old - new) / old if old else 0:.1%}); the same output: {same} of {len(rep)}')
            for f in sorted(rep, key=lambda f: b[f]['cpu'] - by_t[first][f]['cpu'], reverse=True)[:5]:
                print(f'    {f}: {b[f]["cpu"]:.2f} s -> {by_t[first][f]["cpu"]:.2f} s')


def main():
    p = argparse.ArgumentParser(description=__doc__.split('\n')[0])
    sub = p.add_subparsers(dest='cmd', required=True)
    r = sub.add_parser('run')
    r.add_argument('name')
    r.add_argument('--engine', required=True)
    g = r.add_mutually_exclusive_group()
    g.add_argument('--set', default='sample120', help='a list in bench/sets/ or a path (default sample120)')
    g.add_argument('--files', help='files relative to the corpus, comma-separated')
    r.add_argument('--efforts', default='0,50,100')
    r.add_argument('--threads', default='1,2,0')
    r.add_argument('--limit', type=int, default=0, help='only the N smallest files')
    r.add_argument('--store-budget', type=int, default=None, help="the engine's --store-budget (default: its own)")
    r.add_argument('--timeout', type=float, default=1800.0, help='seconds per process')
    r.add_argument('--corpus', default=CORPUS, help='the corpus root (default: $CORPUS or bench/corpus)')
    q = sub.add_parser('report')
    q.add_argument('name')
    q.add_argument('--base')
    a = p.parse_args()
    cmd_run(a) if a.cmd == 'run' else cmd_report(a)


if __name__ == '__main__':
    main()
