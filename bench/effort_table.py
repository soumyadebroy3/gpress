#!/usr/bin/env python3
"""The size table of README.md and docs/reference.md: what each of the six efforts makes of a set of files.

    python3 -I bench/effort_table.py NAME [--set all] [--corpus DIR] [--engine BIN] [--efforts 0,20,...] [--jobs N] [--psnr]

Runs `BIN optimize IN OUT --effort E --threads 1` on each file of a list in bench/sets/ at each effort, writes
one row per file to bench/results/NAME/files.tsv (path, bytes in, then the outcome and the bytes out at each
effort) and prints one row per effort: the bytes of all the outputs together, and their share of the input. A
file gpress leaves untouched counts with its own size (its output is the input), and so does one a run fails on.

--psnr is for the photo set (bench/photo_set.py). At each effort from 60 up it also measures how far the
output's pages are from the input's: PDFium (pypdfium2) draws both at 150 pixels per inch, up to 30 pages, and
the number is the peak signal-to-noise ratio in dB of the page that differs most ("same": no pixel differs). It
measures one file at a time (PDFium must not be called from two threads) and prints a row per file as well.

The files are untrusted: this process and the runs it starts are held to 3 GB of address space.
"""
import argparse
import math
import os
import resource
import subprocess
import sys
import tempfile
from concurrent.futures import ThreadPoolExecutor

HERE = os.path.dirname(os.path.abspath(__file__))
LIMIT = 3_000_000_000
PAGES = 30


def worst_psnr(a, b):
    """The lowest PSNR in dB over the pages of two PDFs as PDFium draws them, or 'same'."""
    import numpy
    import pypdfium2
    worst = 'same'
    one, two = pypdfium2.PdfDocument(a), pypdfium2.PdfDocument(b)
    for i in range(min(len(one), PAGES)):
        x, y = (d[i].render(scale=150 / 72).to_numpy().astype(numpy.int32) for d in (one, two))
        mse = float(((x - y) ** 2).mean())
        if mse:
            worst = min(10 * math.log10(255 * 255 / mse), math.inf if worst == 'same' else worst)
    one.close()
    two.close()
    return worst if worst == 'same' else f'{worst:.1f}'


def measure(engine, path, efforts, psnr):
    """[(outcome, bytes out, PSNR or '')] for each effort; the outcome is the tool's word, or EXIT and its
    status, and then the bytes are the input's."""
    got = []
    with tempfile.TemporaryDirectory() as tmp:
        out = os.path.join(tmp, 'out.pdf')
        for effort in efforts:
            p = subprocess.run([engine, 'optimize', path, out, '--effort', str(effort), '--threads', '1'],
                               capture_output=True, text=True)
            cols = (p.stdout.strip().splitlines() or [''])[-1].split('\t')
            if p.returncode == 0 and len(cols) > 1 and os.path.exists(out):
                got.append((cols[1], os.path.getsize(out), worst_psnr(path, out) if psnr and effort >= 60 else ''))
            else:
                got.append((f'EXIT{p.returncode}', os.path.getsize(path), ''))
    return got


def main():
    ap = argparse.ArgumentParser(description=__doc__.split('\n\n')[0])
    ap.add_argument('name', help='the results go to bench/results/NAME/')
    ap.add_argument('--set', default='all', help='a list in bench/sets/ (default all)')
    ap.add_argument('--corpus', default=os.path.join(HERE, 'corpus'), help='root of the test files')
    ap.add_argument('--engine', default=os.path.join(HERE, '..', 'target', 'release', 'gpress'))
    ap.add_argument('--efforts', default='0,20,40,60,80,100')
    ap.add_argument('--jobs', type=int, default=os.cpu_count() or 1, help='files measured at a time')
    ap.add_argument('--psnr', action='store_true', help='also compare the pages of the lossy outputs with the input')
    args = ap.parse_args()
    resource.setrlimit(resource.RLIMIT_AS, (LIMIT, LIMIT))
    efforts = [int(e) for e in args.efforts.split(',')]
    engine = os.path.abspath(args.engine)
    with open(os.path.join(HERE, 'sets', args.set + '.txt')) as f:
        files = [line.strip() for line in f if line.strip() and not line.startswith('#')]
    sizes = [os.path.getsize(os.path.join(args.corpus, rel)) for rel in files]
    with ThreadPoolExecutor(1 if args.psnr else args.jobs) as pool:
        rows = list(pool.map(lambda rel: measure(engine, os.path.join(args.corpus, rel), efforts, args.psnr), files))
    out_dir = os.path.join(HERE, 'results', args.name)
    os.makedirs(out_dir, exist_ok=True)
    cells = ('outcome', 'bytes', 'psnr') if args.psnr else ('outcome', 'bytes')
    with open(os.path.join(out_dir, 'files.tsv'), 'w') as f:
        f.write('\t'.join(['file', 'bytes_in'] + [f'{k}{e}' for e in efforts for k in cells]) + '\n')
        for rel, size, row in zip(files, sizes, rows):
            f.write('\t'.join([rel, str(size)] + [str(v) for cell in row for v in cell[:len(cells)]]) + '\n')
    if args.psnr:
        print('| File | Bytes | ' + ' | '.join(str(e) for e in efforts) + ' |')
        print('|---|---|' + '---|' * len(efforts))
        for rel, size, row in zip(files, sizes, rows):
            print(f'| {rel} | {size:,} | ' + ' | '.join(f'{n:,}' + (f' ({db})' if db else '') for _, n, db in row) + ' |')
        print()
    total = sum(sizes)
    print(f'{len(files)} files of {args.set}, {total:,} bytes')
    print('| Effort | Bytes | Share of the input |')
    print('|---|---|---|')
    for k, effort in enumerate(efforts):
        out = sum(row[k][1] for row in rows)
        print(f'| {effort} | {out:,} | {100 * out / total:.2f}% |')
    still = sum(all(o == 'UNTOUCHED' for o, _, _ in row) for row in rows)
    failed = sum(any(o.startswith('EXIT') for o, _, _ in row) for row in rows)
    print(f'untouched at every effort: {still} files; a run failed: {failed} files')


if __name__ == '__main__':
    sys.exit(main())
