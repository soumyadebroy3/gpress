#!/usr/bin/env python3
"""The effort table of README.md and docs/reference.md: what each of the six efforts makes of a text file, an
image-heavy file and a scan.

    python3 -I bench/effort_table.py NAME [--set all] [--corpus DIR] [--engine BIN] [--efforts 0,20,...] [--jobs N]

Sorts the files of a list in bench/sets/ into kinds, runs `BIN optimize IN OUT --effort E --threads 1` on each
file of a kind at each effort, writes one row per file to bench/results/NAME/files.tsv (path, kind, bytes in,
then the outcome and the bytes out at each effort) and prints the table: for each kind, the number of files and,
at each effort, the median output size and the total output size, both as a share of the input.

A file's kind comes from its own objects, read with pikepdf; nothing in a file is run or drawn:

  text          its image streams hold under a tenth of the file's bytes
  image-heavy   its image streams hold half of the file's bytes or more, and a page shows text
  scans         half or more too, and no page shows text (none at all, or only the invisible text a character
                recogniser lays over a scanned page; text inside a form is not looked for)

A file between a tenth and a half has no kind, and neither has one pikepdf cannot open. The table counts a
file when gpress rewrites it at one effort at least (where it is left untouched the output is the input); a
file left untouched at every effort, or one a run failed on, is only counted in the line under the table.

The corpus is untrusted: this process and the runs it starts are held to 3 GB of address space.
"""
import argparse
import os
import resource
import statistics
import subprocess
import sys
import tempfile
from concurrent.futures import ThreadPoolExecutor

import pikepdf

HERE = os.path.dirname(os.path.abspath(__file__))
KINDS = ('text', 'image-heavy', 'scans')
LIMIT = 3_000_000_000


def shows_text(page):
    """Whether the page's own content draws text in a mode other than 3 (invisible)."""
    mode = 0
    try:
        for operands, operator in pikepdf.parse_content_stream(page):
            name = str(operator)
            if name == 'Tr' and operands:
                mode = int(operands[0])
            elif name in ('Tj', 'TJ', "'", '"') and mode != 3:
                return True
    except Exception:  # a page pikepdf cannot read shows nothing we can count
        return False
    return False


def kind_of(path):
    """The file's kind, or None."""
    try:
        with pikepdf.open(path) as pdf:
            images = sum(len(o.read_raw_bytes()) for o in pdf.objects
                         if isinstance(o, pikepdf.Stream) and o.get('/Subtype') == '/Image')
            share = images / os.path.getsize(path)
            if share < 0.1:
                return 'text'
            if share < 0.5:
                return None
            return 'image-heavy' if any(shows_text(p) for p in pdf.pages) else 'scans'
    except Exception:
        return None


def measure(engine, path, efforts):
    """[(outcome, bytes out)] for each effort; the outcome is the tool's word, or EXIT and its status."""
    got = []
    with tempfile.TemporaryDirectory() as tmp:
        out = os.path.join(tmp, 'out.pdf')
        for effort in efforts:
            p = subprocess.run([engine, 'optimize', path, out, '--effort', str(effort), '--threads', '1'],
                               capture_output=True, text=True)
            cols = (p.stdout.strip().splitlines() or [''])[-1].split('\t')
            ok = p.returncode == 0 and len(cols) > 1 and os.path.exists(out)
            got.append((cols[1], os.path.getsize(out)) if ok else (f'EXIT{p.returncode}', 0))
    return got


def main():
    ap = argparse.ArgumentParser(description=__doc__.split('\n\n')[0])
    ap.add_argument('name', help='the results go to bench/results/NAME/')
    ap.add_argument('--set', default='all', help='a list in bench/sets/ (default all)')
    ap.add_argument('--corpus', default=os.path.join(HERE, 'corpus'), help='root of the test files')
    ap.add_argument('--engine', default=os.path.join(HERE, '..', 'target', 'release', 'gpress'))
    ap.add_argument('--efforts', default='0,20,40,60,80,100')
    ap.add_argument('--jobs', type=int, default=os.cpu_count() or 1, help='files measured at a time')
    args = ap.parse_args()
    resource.setrlimit(resource.RLIMIT_AS, (LIMIT, LIMIT))
    efforts = [int(e) for e in args.efforts.split(',')]
    engine = os.path.abspath(args.engine)
    with open(os.path.join(HERE, 'sets', args.set + '.txt')) as f:
        files = [line.strip() for line in f if line.strip() and not line.startswith('#')]
    kinds = {rel: kind_of(os.path.join(args.corpus, rel)) for rel in files}
    todo = [rel for rel in files if kinds[rel]]
    with ThreadPoolExecutor(args.jobs) as pool:
        rows = list(pool.map(lambda rel: measure(engine, os.path.join(args.corpus, rel), efforts), todo))
    out_dir = os.path.join(HERE, 'results', args.name)
    os.makedirs(out_dir, exist_ok=True)
    with open(os.path.join(out_dir, 'files.tsv'), 'w') as f:
        f.write('\t'.join(['file', 'kind', 'bytes_in'] + [f'{k}{e}' for e in efforts for k in ('outcome', 'bytes')]) + '\n')
        for rel, row in zip(todo, rows):
            size = os.path.getsize(os.path.join(args.corpus, rel))
            f.write('\t'.join([rel, kinds[rel], str(size)] + [str(v) for cell in row for v in cell]) + '\n')
    print(f'{len(files)} files of {args.set}, {len(todo)} with a kind; each cell: median, total (output as a share of input)')
    print('| Kind | Files | ' + ' | '.join(str(e) for e in efforts) + ' |')
    print('|---|---|' + '---|' * len(efforts))
    apart = []
    for kind in KINDS:
        mine = [(os.path.getsize(os.path.join(args.corpus, rel)), row) for rel, row in zip(todo, rows) if kinds[rel] == kind]
        failed = [m for m in mine if any(o.startswith('EXIT') for o, _ in m[1])]
        still = [m for m in mine if all(o == 'UNTOUCHED' for o, _ in m[1])]
        counted = [m for m in mine if m not in failed and m not in still]
        apart.append(f'{kind}: {len(still)} untouched at every effort, {len(failed)} failed')
        cells = []
        for k in range(len(efforts)):
            shares = [row[k][1] / size for size, row in counted]
            total = sum(row[k][1] for _, row in counted) / max(1, sum(size for size, _ in counted))
            cells.append(f'{100 * statistics.median(shares):.1f}%, {100 * total:.1f}%' if shares else '-')
        print(f'| {kind} | {len(counted)} | ' + ' | '.join(cells) + ' |')
    print('not counted: ' + '; '.join(apart))


if __name__ == '__main__':
    sys.exit(main())
