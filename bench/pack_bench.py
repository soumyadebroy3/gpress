#!/usr/bin/env python3
"""peyaji format versus general-purpose compressors, whole files.

  pack_bench.py OUT run        OUT/files.txt -> OUT/pack.tsv (ours, restore check) and OUT/base.tsv (baselines)
  pack_bench.py OUT expand     OUT/expand.tsv: the fair "decompress first, then xz" baseline (pdfe expand)
  pack_bench.py OUT report

The engine is $PDFE (default: target/release/pdfe of this repository).
Baselines: xz -9e, Brotli 11 (16 MiB window), Zstandard 19 (long matching), and two "decompress first" ones:
  qpdf + xz: qpdf rewrites the file with every stream decompressed, then xz -9e. Not comparable for every
    file: qpdf drops objects nothing refers to, earlier revisions, and parts of damaged files it can't read.
  expand + xz: the same preparation the peyaji format uses (every stream we can rebuild decoded in place, the
    rest of the file byte for byte), then xz -9e, plus the bytes a restore needs on top (rebuild settings,
    preflate corrections, Lepton data for JPEGs). This is the fair one: same input, general compressor.
Run with python3 -I."""
import hashlib, lzma, os, subprocess, sys, tempfile, time
import brotli, zstandard

ENGINE = os.environ.get('PDFE', '/home/claude/pdf-engine/target/release/pdfe')

def key(path):
    return os.path.basename(os.path.dirname(path)) + '__' + os.path.basename(path)

def run(out):
    files = [l.strip() for l in open(os.path.join(out, 'files.txt')) if l.strip()]
    os.makedirs(os.path.join(out, 'pyj'), exist_ok=True)
    lst = ''.join(f"{f}\t{os.path.join(out, 'pyj', key(f) + '.pyj')}\n" for f in files)
    t = time.time()
    r = subprocess.run([ENGINE, 'pack-batch'], input=lst.encode(), capture_output=True)
    rows = []
    for line in r.stdout.decode().splitlines():
        p = line.split('\t')
        if len(p) < 4:
            rows.append(f'{p[0]}\t-1\t-1\t0\tPANIC\n'); continue
        f, n, z, secs = p[0], int(p[1]), int(p[2]), float(p[3])
        # Independent restore check: unpack and compare with the original bytes.
        back = os.path.join(out, 'restore.tmp')
        u = subprocess.run([ENGINE, 'unpack', os.path.join(out, 'pyj', key(f) + '.pyj'), back], capture_output=True)
        ok = u.returncode == 0 and open(back, 'rb').read() == open(f, 'rb').read()
        rows.append(f'{f}\t{n}\t{z}\t{secs}\t{"exact" if ok else "MISMATCH"}\t{p[4] if len(p) > 4 else ""}\n')
    open(os.path.join(out, 'pack.tsv'), 'w').write(''.join(rows))
    print(f'pack: {time.time() - t:.0f} s')
    if not os.path.exists(os.path.join(out, 'base.tsv')):
        baselines(out)

def baselines(out):
    files = [l.strip() for l in open(os.path.join(out, 'files.txt')) if l.strip()]
    base = []
    zc = zstandard.ZstdCompressor(compression_params=zstandard.ZstdCompressionParameters.from_level(19, window_log=27, enable_ldm=True))
    for f in files:
        d = open(f, 'rb').read()
        xz = len(lzma.compress(d, preset=9 | lzma.PRESET_EXTREME))
        br = len(brotli.compress(d, quality=11, lgwin=24))
        zs = len(zc.compress(d))
        with tempfile.TemporaryDirectory() as td:
            un = os.path.join(td, 'u.pdf')
            q = subprocess.run(['qpdf', '--stream-data=uncompress', '--decode-level=generalized', '--object-streams=disable', f, un], capture_output=True)
            ix = len(lzma.compress(open(un, 'rb').read(), preset=9 | lzma.PRESET_EXTREME)) if q.returncode in (0, 3) and os.path.exists(un) else -1
        base.append(f'{f}\t{len(d)}\t{xz}\t{br}\t{zs}\t{ix}\n')
    open(os.path.join(out, 'base.tsv'), 'w').write(''.join(base))

def expand(out):
    files = [l.strip() for l in open(os.path.join(out, 'files.txt')) if l.strip()]
    rows = []
    with tempfile.TemporaryDirectory() as td:
        x = os.path.join(td, 'x')
        for f in files:
            r = subprocess.run([ENGINE, 'expand', f, x], capture_output=True, text=True)
            if r.returncode != 0:
                rows.append(f'{f}\t-1\t-1\t-1\n'); continue
            _, n, xlen, extra = r.stdout.strip().split('\t')
            xz = len(lzma.compress(open(x, 'rb').read(), preset=9 | lzma.PRESET_EXTREME))
            rows.append(f'{f}\t{xlen}\t{extra}\t{xz}\n')
    open(os.path.join(out, 'expand.tsv'), 'w').write(''.join(rows))

def report(out):
    pk = {l.split('\t')[0]: l.rstrip('\n').split('\t') for l in open(os.path.join(out, 'pack.tsv'))}
    bs = {l.split('\t')[0]: l.rstrip('\n').split('\t') for l in open(os.path.join(out, 'base.tsv'))}
    ex = {}
    if os.path.exists(os.path.join(out, 'expand.tsv')):
        for l in open(os.path.join(out, 'expand.tsv')):
            f, xlen, extra, xz = l.rstrip('\n').split('\t')
            if int(xz) >= 0:
                ex[f] = int(xz) + int(extra)
    T = dict(n=0, raw=0, ours=0, xz=0, br=0, zs=0, ix=0, mism=0, secs=0.0, n_ix=0, raw_ix=0, ours_ix=0, best_gen=0,
             n_ex=0, raw_ex=0, ex=0, ours_ex=0, ex_wins=0)
    wins = 0
    for f, p in pk.items():
        if f not in bs or p[1] == '-1':
            continue
        b = bs[f]
        raw, ours = int(p[1]), int(p[2])
        xz, br, zs, ix = int(b[2]), int(b[3]), int(b[4]), int(b[5])
        T['n'] += 1; T['raw'] += raw; T['ours'] += ours; T['xz'] += xz; T['br'] += br; T['zs'] += zs; T['secs'] += float(p[3])
        T['best_gen'] += min(xz, br, zs)
        T['mism'] += p[4] != 'exact'
        wins += ours < min(xz, br, zs, ix if ix > 0 else 1 << 60)
        if ix > 0:
            T['n_ix'] += 1; T['raw_ix'] += raw; T['ix'] += ix; T['ours_ix'] += ours
        if f in ex:
            T['n_ex'] += 1; T['raw_ex'] += raw; T['ex'] += ex[f]; T['ours_ex'] += ours; T['ex_wins'] += ours < ex[f]
    pct = lambda a, b: 100 * a / b
    print(f"{T['n']} files, {T['raw']/1e6:.2f} MB; restore mismatches: {T['mism']}")
    print(f"  ours (peyaji)      {pct(T['ours'], T['raw']):5.1f}% of original   ({T['secs']:.0f} s, pack + restore check)")
    print(f"  xz -9e             {pct(T['xz'], T['raw']):5.1f}%")
    print(f"  Brotli 11          {pct(T['br'], T['raw']):5.1f}%")
    print(f"  Zstandard 19 long  {pct(T['zs'], T['raw']):5.1f}%")
    print(f"  best of the three  {pct(T['best_gen'], T['raw']):5.1f}%")
    print(f"  qpdf + xz          {pct(T['ix'], T['raw_ix']):5.1f}%  (ours on the same {T['n_ix']} files: {pct(T['ours_ix'], T['raw_ix']):.1f}%; not comparable, see above)")
    if T['n_ex']:
        print(f"  expand + xz        {pct(T['ex'], T['raw_ex']):5.1f}%  (ours on the same {T['n_ex']} files: {pct(T['ours_ex'], T['raw_ex']):.1f}%; ours smaller on {T['ex_wins']})")
    print(f"  ours smallest on {wins} of {T['n']} files")

if __name__ == '__main__':
    {'run': run, 'base': baselines, 'expand': expand, 'report': report}[sys.argv[2]](sys.argv[1])
