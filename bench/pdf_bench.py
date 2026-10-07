#!/usr/bin/env python3
"""Whole-PDF comparison on the test corpus.

  pdf_bench.py OUT ours [pdfe args]   our engine on every PDF -> OUT/ours/, OUT/ours.tsv
  pdf_bench.py OUT qpdf               qpdf (object streams, recompressed Flate, level 9) -> OUT/qpdf/, OUT/qpdf.tsv
  pdf_bench.py OUT verify             independent check of every smaller output -> OUT/verify.jsonl
  pdf_bench.py OUT report

The corpus list is OUT/files.txt (one path per line). Run with python3 -I."""
import json, os, subprocess, sys, time
from concurrent.futures import ThreadPoolExecutor

ENGINE = '/home/claude/pdf-engine/target/release/pdfe'
VERIFY = '/home/claude/pdf-engine/bench/verify_pdf.py'

def files(out):
    return [l.strip() for l in open(os.path.join(out, 'files.txt')) if l.strip()]

def key(path):
    return os.path.basename(os.path.dirname(path)) + '__' + os.path.basename(path)

def ours(out, args):
    os.makedirs(os.path.join(out, 'ours'), exist_ok=True)
    lst = ''.join(f"{f}\t{os.path.join(out, 'ours', key(f))}\n" for f in files(out))
    t = time.time()
    r = subprocess.run([ENGINE, 'pdf-batch'] + args, input=lst.encode(), capture_output=True)
    open(os.path.join(out, 'ours.tsv'), 'w').write(r.stdout.decode())
    if r.stderr:
        open(os.path.join(out, 'ours.err'), 'w').write(r.stderr.decode())
    print(f'ours: {time.time() - t:.0f} s')

def qpdf(out):
    os.makedirs(os.path.join(out, 'qpdf'), exist_ok=True)
    def one(f):
        o = os.path.join(out, 'qpdf', key(f))
        t = time.time()
        r = subprocess.run(['qpdf', '--object-streams=generate', '--compress-streams=y', '--recompress-flate',
                            '--compression-level=9', '--decode-level=generalized', f, o], capture_output=True)
        size = os.path.getsize(o) if os.path.exists(o) and r.returncode in (0, 3) else -1
        return f'{f}\t{r.returncode}\t{os.path.getsize(f)}\t{size}\t{time.time() - t:.2f}\n'
    with ThreadPoolExecutor(2) as ex:
        rows = list(ex.map(one, files(out)))
    open(os.path.join(out, 'qpdf.tsv'), 'w').write(''.join(rows))

def verify(out):
    rows = [l.rstrip('\n').split('\t') for l in open(os.path.join(out, 'ours.tsv'))]
    todo = [r for r in rows if len(r) > 1 and r[1] == 'SMALLER']
    def one(r):
        p = subprocess.run(['python3', '-I', VERIFY, r[0], os.path.join(out, 'ours', key(r[0]))], capture_output=True, text=True, timeout=1800)
        return p.stdout.strip() or json.dumps({'file': r[0], 'identical': False, 'error': p.stderr[-300:]})
    with ThreadPoolExecutor(2) as ex:
        res = list(ex.map(one, todo))
    open(os.path.join(out, 'verify.jsonl'), 'w').write('\n'.join(res) + '\n')
    bad = [json.loads(x) for x in res if not json.loads(x).get('identical')]
    print(f'verified {len(res)}: {len(res) - len(bad)} identical, {len(bad)} flagged')
    for b in bad[:20]:
        print('  ', b.get('file'), b.get('qpdf_check'), b.get('object_problems', [])[:2], b.get('render_problems', [])[:2], b.get('error', '')[:200])

def report(out):
    rows = [l.rstrip('\n').split('\t') for l in open(os.path.join(out, 'ours.tsv')) if l.strip()]
    status = {}
    for r in rows:
        status.setdefault(r[1], []).append(r)
    print({k: len(v) for k, v in status.items()})
    reasons = {}
    for r in status.get('UNTOUCHED', []):
        why = r[5].split(':')[0]
        reasons[why] = reasons.get(why, 0) + 1
    print('untouched because:', dict(sorted(reasons.items(), key=lambda x: -x[1])))
    q = {}
    if os.path.exists(os.path.join(out, 'qpdf.tsv')):
        for l in open(os.path.join(out, 'qpdf.tsv')):
            f, rc, a, b, t = l.rstrip('\n').split('\t')
            q[f] = int(b)
    tin = tout = tq = n = 0
    for r in status.get('SMALLER', []):
        f, a, b = r[0], int(r[2]), int(r[3])
        if q.get(f, -1) > 0:
            tin += a; tout += b; tq += min(q[f], a); n += 1
    if n:
        print(f'{n} files we made smaller (and qpdf handled): input {tin/1e6:.2f} MB, ours {tout/1e6:.2f} MB ({100*(1-tout/tin):.1f}% smaller), qpdf {tq/1e6:.2f} MB ({100*(1-tq/tin):.1f}% smaller)')
    allin = sum(int(r[2]) for r in rows if len(r) > 3 and r[1] in ('SMALLER', 'UNTOUCHED'))
    allout = sum(int(r[3]) for r in rows if len(r) > 3 and r[1] in ('SMALLER', 'UNTOUCHED'))
    print(f'all files: {allin/1e6:.2f} MB -> {allout/1e6:.2f} MB ({100*(1-allout/allin):.1f}% smaller)')

if __name__ == '__main__':
    out, cmd = sys.argv[1], sys.argv[2]
    {'ours': lambda: ours(out, sys.argv[3:]), 'qpdf': lambda: qpdf(out), 'verify': lambda: verify(out), 'report': lambda: report(out)}[cmd]()
