#!/usr/bin/env python3
"""JPEG streams of the test PDFs: our lossless Huffman optimisation against jpegtran, checked by tools we didn't
write.

  jpeg_bench.py OUT find FILES...   PDFs with DCTDecode streams -> OUT/pdfs.txt; every distinct JPEG (the
                                    stream with its general filters undone, by pikepdf) -> OUT/jpegs/, OUT/jpegs.tsv
  jpeg_bench.py OUT ours            pdfe jpeg-batch (PDFE=path) -> OUT/ours/, OUT/ours.tsv
  jpeg_bench.py OUT tools           jpegtran -optimize -copy all, and -progressive (JPEGTRAN=path, libjpeg-turbo),
                                    and mozjpeg's lossless optimisation (MOZJPEG_PY=folder with the
                                    mozjpeg-lossless-optimization package, optional) -> OUT/tools.tsv
  jpeg_bench.py OUT check           every JPEG we changed, against its original: pixels decoded by libjpeg-turbo
                                    (djpeg, DJPEG=path) and by Pillow, and coefficients (jpegtran -copy all with
                                    the standard tables must give the same bytes) -> OUT/check.tsv
  jpeg_bench.py OUT report          totals, and ours against the tools by kind of JPEG (PDFE for jpeg-info)
Run with python3 -I (the files are untrusted)."""
import hashlib, io, os, re, subprocess, sys, time

import pikepdf

PDFE = os.environ.get('PDFE', '/home/claude/pdf-engine/target/release/pdfe')
JPEGTRAN = os.environ.get('JPEGTRAN', 'jpegtran')
DJPEG = os.environ.get('DJPEG', 'djpeg')


def dct_streams(pdf):
    """(object number, filter names, stream) for every stream whose last filter is DCTDecode."""
    for obj in pdf.objects:
        if not isinstance(obj, pikepdf.Stream):
            continue
        try:
            f = obj.stream_dict.get('/Filter')
            names = [str(f)] if isinstance(f, pikepdf.Name) else [str(x) for x in f] if isinstance(f, pikepdf.Array) else []
        except Exception:
            continue
        if names and names[-1] in ('/DCTDecode', '/DCT'):
            yield obj.objgen[0], names, obj


def jpeg_bytes(names, s):
    raw = s.read_raw_bytes()
    if len(names) == 1:
        return raw
    # General filters in front: let qpdf undo just those, on a copy of the stream.
    scratch = pikepdf.new()
    t = pikepdf.Stream(scratch, raw)
    t.Filter = pikepdf.Array([pikepdf.Name(n) for n in names[:-1]])
    parms = s.stream_dict.get('/DecodeParms')
    if isinstance(parms, pikepdf.Array) and len(parms) == len(names):
        t.DecodeParms = pikepdf.Array([scratch.copy_foreign(p) if p is not None else None for p in list(parms)[:-1]])
    return t.read_bytes(decode_level=pikepdf.StreamDecodeLevel.specialized)


def find(out, files):
    os.makedirs(os.path.join(out, 'jpegs'), exist_ok=True)
    pdfs, rows, seen = [], [], set()
    for f in files:
        try:
            pdf = pikepdf.open(f)
        except Exception:
            continue
        found = False
        try:
            for num, names, s in dct_streams(pdf):
                found = True
                try:
                    data = jpeg_bytes(names, s)
                except Exception as e:
                    rows.append(f'-\t0\t{f}\t{num}\t{"+".join(names)}\tunreadable {type(e).__name__}')
                    continue
                h = hashlib.sha1(data).hexdigest()
                rows.append(f'{h}\t{len(data)}\t{f}\t{num}\t{"+".join(names)}\t')
                if h not in seen:
                    seen.add(h)
                    with open(os.path.join(out, 'jpegs', h + '.jpg'), 'wb') as fh:
                        fh.write(data)
        except Exception:
            pass
        if found:
            pdfs.append(f)
        pdf.close()
    open(os.path.join(out, 'pdfs.txt'), 'w').write(''.join(p + '\n' for p in pdfs))
    open(os.path.join(out, 'jpegs.tsv'), 'w').write(''.join(r + '\n' for r in rows))
    total = sum(os.path.getsize(os.path.join(out, 'jpegs', h + '.jpg')) for h in seen)
    print(f'{len(pdfs)} PDFs with DCT streams; {len(rows)} streams, {len(seen)} distinct JPEGs, {total} bytes')


def jpegs(out):
    return sorted(n[:-4] for n in os.listdir(os.path.join(out, 'jpegs')) if n.endswith('.jpg'))


def ours(out):
    os.makedirs(os.path.join(out, 'ours'), exist_ok=True)
    lst = ''.join(f"{os.path.join(out, 'jpegs', h + '.jpg')}\t{os.path.join(out, 'ours', h + '.jpg')}\n" for h in jpegs(out))
    t = time.time()
    r = subprocess.run([PDFE, 'jpeg-batch'], input=lst.encode(), capture_output=True)
    open(os.path.join(out, 'ours.tsv'), 'w').write(r.stdout.decode())
    print(f'ours: {time.time() - t:.1f} s')


def run_size(cmd, src):
    t = time.time()
    r = subprocess.run(cmd + [src], capture_output=True)
    return (len(r.stdout) if r.returncode == 0 and r.stdout else -1), time.time() - t


def tools(out):
    moz = None
    if os.environ.get('MOZJPEG_PY'):
        sys.path.insert(0, os.environ['MOZJPEG_PY'])
        import mozjpeg_lossless_optimization as moz
    rows = []
    for h in jpegs(out):
        src = os.path.join(out, 'jpegs', h + '.jpg')
        opt, t1 = run_size([JPEGTRAN, '-optimize', '-copy', 'all'], src)
        prog, t2 = run_size([JPEGTRAN, '-optimize', '-progressive', '-copy', 'all'], src)
        m = -1
        if moz is not None:
            try:
                m = len(moz.optimize(open(src, 'rb').read(), copy=moz.COPY_MARKERS.ALL))
            except Exception:
                m = -1
        rows.append(f'{h}\t{os.path.getsize(src)}\t{opt}\t{prog}\t{m}\t{t1:.3f}\t{t2:.3f}\n')
    open(os.path.join(out, 'tools.tsv'), 'w').write(''.join(rows))


def pixels_djpeg(data):
    r = subprocess.run([DJPEG, '-ppm'], input=data, capture_output=True)
    return hashlib.sha256(r.stdout).hexdigest() if r.returncode == 0 else 'djpeg failed: ' + r.stderr.decode()[:80]


def pixels_pil(data):
    from PIL import Image
    try:
        im = Image.open(io.BytesIO(data))
        im.load()
        return hashlib.sha256(im.mode.encode() + repr(im.size).encode() + im.tobytes()).hexdigest()
    except Exception as e:
        return 'Pillow failed: ' + type(e).__name__


def coefficients(data):
    """jpegtran with the standard tables (no -optimize, no restart markers): the bytes depend only on the
    coefficients, quantisation tables and frame, plus the markers copied."""
    r = subprocess.run([JPEGTRAN, '-copy', 'all'], input=data, capture_output=True)
    return hashlib.sha256(r.stdout).hexdigest() if r.returncode == 0 else 'jpegtran failed: ' + r.stderr.decode()[:80]


def check(out):
    rows, bad = [], 0
    for line in open(os.path.join(out, 'ours.tsv')):
        p = line.rstrip('\n').split('\t')
        if len(p) < 5 or p[4] != 'optimized':
            continue
        h = os.path.basename(p[0])[:-4]
        a = open(p[0], 'rb').read()
        b = open(os.path.join(out, 'ours', h + '.jpg'), 'rb').read()
        res = []
        for name, fn in (('djpeg', pixels_djpeg), ('Pillow', pixels_pil), ('coefficients', coefficients)):
            x, y = fn(a), fn(b)
            res.append(name + ('=' if x == y and 'failed' not in x else '!'))
        ok = all(r.endswith('=') for r in res)
        bad += not ok
        rows.append(f'{h}\t{"same" if ok else "DIFFERENT"}\t{" ".join(res)}\n')
    open(os.path.join(out, 'check.tsv'), 'w').write(''.join(rows))
    print(f'checked {len(rows)} optimised JPEGs: {len(rows) - bad} the same, {bad} different')


def kind(path, why):
    """Optimised with or without restart markers (jpegtran drops them, we keep them), or why left as it is."""
    if why != 'optimized':
        return 'left as it is: ' + why.replace('refused: ', '')
    info = subprocess.run([PDFE, 'jpeg-info', path], capture_output=True, text=True).stdout
    if any(int(r) > 0 for r in re.findall(r'\| scan \[[^\]]*\] restart (\d+)', info)):
        return 'optimised, with restart markers'
    return 'optimised, no restart markers'


def report(out):
    tools_rows = {}
    if os.path.exists(os.path.join(out, 'tools.tsv')):
        for l in open(os.path.join(out, 'tools.tsv')):
            h, size, opt, prog, moz, *_ = l.rstrip('\n').split('\t')
            tools_rows[h] = (int(size), int(opt), int(prog), int(moz))
    n = tin = tout = 0
    secs = 0.0
    kinds = {}
    larger = {}
    for l in open(os.path.join(out, 'ours.tsv')):
        p = l.rstrip('\n').split('\t')
        if len(p) < 5:
            continue
        h = os.path.basename(p[0])[:-4]
        a, b = int(p[1]), int(p[2])
        secs += float(p[3])
        n += 1
        tin += a
        tout += b
        k = kind(p[0], p[4])
        row = kinds.setdefault(k, [0] * 6)
        size, opt, prog, moz = tools_rows.get(h, (a, -1, -1, -1))
        best = lambda x: x if 0 < x < size else size  # a tool that can't make it smaller leaves it as it is
        for i, v in enumerate((1, a, b, best(opt), best(prog), best(moz))):
            row[i] += v
        if b > best(opt):
            larger.setdefault(k, []).append(b - best(opt))
    print(f'{n} distinct JPEGs: {tin} -> {tout} bytes ({100 * (1 - tout / max(tin, 1)):.2f}% smaller), {secs:.1f} s')
    print('kind\tJPEGs\toriginal\tours\tjpegtran -optimize\tjpegtran -optimize -progressive\tmozjpeg (progressive)')
    total = [0] * 6
    for k, row in sorted(kinds.items()):
        print(k, *row, sep='\t')
        total = [x + y for x, y in zip(total, row)]
    print('all', *total, sep='\t')
    for k, v in sorted(larger.items()):
        print(f'ours larger than jpegtran -optimize: {k}: {len(v)} JPEGs, {sum(v)} bytes')


if __name__ == '__main__':
    out, cmd = sys.argv[1], sys.argv[2]
    {'find': lambda: find(out, sys.argv[3:]), 'ours': lambda: ours(out), 'tools': lambda: tools(out),
     'check': lambda: check(out), 'report': lambda: report(out)}[cmd]()
