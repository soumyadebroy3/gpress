#!/usr/bin/env python3
"""Flate encoders on the Flate streams pulled out of the test PDFs.

  flate_bench.py DIR zopfli      Zopfli (15 iterations) sizes -> DIR/zopfli.tsv   (resumable, slow)
  flate_bench.py DIR others      zlib-9 and libdeflate-12 sizes -> DIR/others.tsv
  flate_bench.py DIR ours [ARGS] our encoder (pdfe batch ARGS) -> DIR/ours.tsv, every output checked with zlib
  flate_bench.py DIR report      totals by stream kind

Run with python3 -I."""
import os, subprocess, sys, time, zlib

PDFE = '/home/claude/pdf-engine/target/release/pdfe'
CAP = 4 << 20

def manifest(d):
    rows = []
    for line in open(os.path.join(d, 'manifest.tsv')):
        h, n, stored, kind, src = line.rstrip('\n').split('\t')
        if int(n) <= CAP:
            rows.append((h, int(n), int(stored), kind, src))
    return rows

def done(path):
    if not os.path.exists(path): return {}
    out = {}
    for line in open(path):
        p = line.rstrip('\n').split('\t')
        out[p[0]] = p[1:]
    return out

def zopfli_run(d):
    import zopfli.zlib as zz
    path = os.path.join(d, 'zopfli.tsv')
    have = done(path)
    with open(path, 'a') as f:
        for h, n, *_ in sorted(manifest(d), key=lambda r: r[1]):
            if h in have: continue
            data = open(os.path.join(d, 'streams', h + '.bin'), 'rb').read()
            t = time.time(); z = zz.compress(data); dt = time.time() - t
            assert zlib.decompress(z) == data
            f.write(f'{h}\t{len(z)}\t{dt:.3f}\n'); f.flush()

def others_run(d):
    import deflate
    path = os.path.join(d, 'others.tsv')
    with open(path, 'w') as f:
        for h, n, *_ in manifest(d):
            data = open(os.path.join(d, 'streams', h + '.bin'), 'rb').read()
            f.write(f'{h}\t{len(zlib.compress(data, 9))}\t{len(deflate.zlib_compress(data, 12))}\n')

def ours_run(d, args):
    os.makedirs(os.path.join(d, 'ours'), exist_ok=True)
    rows = manifest(d)
    lst = ''.join(f"{d}/streams/{h}.bin\t{d}/ours/{h}.z\n" for h, *_ in rows)
    t = time.time()
    r = subprocess.run([PDFE, 'batch'] + args, input=lst.encode(), capture_output=True)
    wall = time.time() - t
    bad = 0
    with open(os.path.join(d, 'ours.tsv'), 'w') as f:
        for line in r.stdout.decode().splitlines():
            p = line.split('\t')
            h = os.path.basename(p[0])[:-4]
            if p[1] in ('ERROR', 'MISMATCH'):
                bad += 1; f.write(f'{h}\t-1\t0\n'); continue
            data = open(os.path.join(d, 'streams', h + '.bin'), 'rb').read()
            z = open(os.path.join(d, 'ours', h + '.z'), 'rb').read()
            if zlib.decompress(z) != data:   # independent check with zlib
                bad += 1; f.write(f'{h}\t-1\t0\n'); continue
            f.write(f'{h}\t{p[2]}\t{float(p[3])/1000:.3f}\n')
    print(f'ours: {len(rows)} streams in {wall:.0f} s, {bad} failures')

def report(d):
    rows = manifest(d)
    zo, ot, ou = done(os.path.join(d, 'zopfli.tsv')), done(os.path.join(d, 'others.tsv')), done(os.path.join(d, 'ours.tsv'))
    kinds = {}
    for h, n, stored, kind, src in rows:
        if h not in zo or h not in ot or h not in ou or int(ou[h][0]) < 0: continue
        k = kinds.setdefault(kind, {'n': 0, 'raw': 0, 'stored': 0, 'z9': 0, 'l12': 0, 'zop': 0, 'ours': 0, 'best': 0, 'win': 0, 'tie': 0, 'loss': 0, 'tz': 0.0, 'to': 0.0})
        z9, l12 = int(ot[h][0]), int(ot[h][1]); zp, tz = int(zo[h][0]), float(zo[h][1]); o, to = int(ou[h][0]), float(ou[h][1])
        for key, v in (('n', 1), ('raw', n), ('stored', stored), ('z9', z9), ('l12', l12), ('zop', zp), ('ours', o), ('best', min(zp, o, l12, z9))):
            k[key] += v
        k['tz'] += tz; k['to'] += to
        k['win' if o < zp else 'tie' if o == zp else 'loss'] += 1
    tot = {key: sum(k[key] for k in kinds.values()) for key in ('n','raw','stored','z9','l12','zop','ours','best','win','tie','loss','tz','to')}
    kinds['ALL'] = tot
    print('%-7s %6s %9s %9s %9s %9s %9s %9s  %7s  %5s/%4s/%4s  %8s %8s' % ('kind','n','raw MB','stored','zlib-9','ldef-12','zopfli','ours','vs zop','win','tie','loss','zop s','ours s'))
    for name, k in kinds.items():
        if not k['n']: continue
        print('%-7s %6d %9.2f %9.0f %9.0f %9.0f %9.0f %9.0f  %+6.2f%%  %5d/%4d/%4d  %8.0f %8.0f' % (name, k['n'], k['raw']/1e6, k['stored'], k['z9'], k['l12'], k['zop'], k['ours'], 100*(k['ours']/k['zop']-1), k['win'], k['tie'], k['loss'], k['tz'], k['to']))

if __name__ == '__main__':
    d, cmd = sys.argv[1], sys.argv[2]
    {'zopfli': lambda: zopfli_run(d), 'others': lambda: others_run(d), 'ours': lambda: ours_run(d, sys.argv[3:]), 'report': lambda: report(d)}[cmd]()
