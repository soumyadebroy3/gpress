#!/usr/bin/env python3
"""Independent check that two PDFs have identical content, using tools we didn't write:

  1. qpdf --check on the output (structure);
  2. pikepdf (qpdf's object model): every object reachable from the trailer has the same value, and every
     stream decodes (lossless filters including RunLength, predictors undone) to the same bytes; a JPEG
     (DCTDecode) must be the same bytes, or else have the same coefficients and decode to the same pixels with
     every decoder here: libjpeg-turbo's jpegtran (coefficients: `jpegtran -copy all` must write the same
     bytes for both), qpdf's libjpeg and Pillow (pixels; their integer IDCT can round a small coefficient
     change away, hence the coefficients), and pdf.js's own decoder (it parses the markers its own way);
  3. PDFium (pypdfium2): every page renders to the same pixels and gives the same text.

A changed JPEG needs jpegtran (on the PATH, or JPEGTRAN=path) and pdf.js (PDFJS_MODULES=node_modules folder
with pdfjs-dist, as for verify_render.py); without them the file is reported as not identical.

Usage: verify_pdf.py ORIGINAL OUTPUT [--pages N]   -> prints one JSON line; exit status 0 if identical.
Run with python3 -I (the files are untrusted)."""
import hashlib, io, json, os, shutil, subprocess, sys, tempfile
from decimal import Decimal

import pikepdf
import pypdfium2 as pdfium

SKIP_STREAM_KEYS = {'/Length', '/Filter', '/DecodeParms'}
HERE = os.path.dirname(os.path.abspath(__file__))
JPEGTRAN = os.environ.get('JPEGTRAN') or shutil.which('jpegtran')


def is_ind(o):
    return isinstance(o, pikepdf.Object) and o.is_indirect


def scalar(o):
    """Canonical value of a non-container object."""
    if isinstance(o, bool):
        return ('bool', o)
    if isinstance(o, int):
        return ('num', Decimal(o))
    if isinstance(o, Decimal):
        return ('num', o)
    if isinstance(o, float):
        return ('num', Decimal(repr(o)))
    if isinstance(o, pikepdf.Name):
        return ('name', o.unparse())
    if isinstance(o, pikepdf.String):
        return ('str', bytes(o))
    if o is None:
        return ('null',)
    try:
        return ('other', o.unparse())
    except Exception:
        return ('other', repr(o))


def items(d, skip=()):
    """(key bytes, value) pairs; keys that aren't valid UTF-8 are compared as raw names."""
    out = []
    for k, v in d.items():
        if k in skip:
            continue
        kb = k.encode('utf-8', 'surrogateescape') if isinstance(k, str) else bytes(k)
        out.append((kb, v))
    return sorted(out, key=lambda x: x[0])


def stream_content(s):
    try:
        return hashlib.sha256(s.read_bytes(decode_level=pikepdf.StreamDecodeLevel.specialized)).hexdigest()
    except Exception as e:
        return 'undecodable:' + type(e).__name__


def filter_names(s):
    f = s.stream_dict.get('/Filter')
    if isinstance(f, pikepdf.Name):
        return [str(f)]
    if isinstance(f, pikepdf.Array):
        return [str(x) for x in f]
    return []


def jpeg_bytes(s):
    """A DCT stream's JPEG: the stream with its general filters undone (by qpdf, on a copy). None if the
    stream's last filter isn't DCTDecode."""
    names = filter_names(s)
    if not names or names[-1] not in ('/DCTDecode', '/DCT'):
        return None
    raw = s.read_raw_bytes()
    if len(names) == 1:
        return raw
    scratch = pikepdf.new()
    t = pikepdf.Stream(scratch, raw)
    t.Filter = pikepdf.Array([pikepdf.Name(n) for n in names[:-1]])
    parms = s.stream_dict.get('/DecodeParms')
    if isinstance(parms, pikepdf.Array) and len(parms) == len(names):
        t.DecodeParms = pikepdf.Array([scratch.copy_foreign(p) if isinstance(p, pikepdf.Dictionary) else None for p in list(parms)[:-1]])
    return t.read_bytes(decode_level=pikepdf.StreamDecodeLevel.specialized)


def dct_parms(s):
    """The DCT stage's own parameters (ColorTransform changes the colours)."""
    names = filter_names(s)
    p = s.stream_dict.get('/DecodeParms')
    if isinstance(p, pikepdf.Array):
        p = p[len(names) - 1] if len(p) == len(names) else 'mismatch'
    elif len(names) != 1:
        p = None
    return 'null' if p is None else p if isinstance(p, str) else p.unparse(resolved=True)


def jpeg_pixels(data):
    """Decoded pixels by two decoders: qpdf (libjpeg, decode level all; on a copy of the JPEG alone, as qpdf
    decodes no DCT stream that has parameters) and Pillow."""
    from PIL import Image
    out = []
    try:
        scratch = pikepdf.new()
        t = pikepdf.Stream(scratch, data)
        t.Filter = pikepdf.Name.DCTDecode
        out.append(hashlib.sha256(t.read_bytes(decode_level=pikepdf.StreamDecodeLevel.all)).hexdigest())
    except Exception as e:
        out.append('qpdf failed:' + type(e).__name__)
    try:
        im = Image.open(io.BytesIO(data))
        im.load()
        out.append(hashlib.sha256(im.mode.encode() + repr(im.size).encode() + im.tobytes()).hexdigest())
    except Exception as e:
        out.append('Pillow failed:' + type(e).__name__)
    return out


def jpeg_coefficients(data):
    """libjpeg-turbo's jpegtran with its standard tables (no -optimize, no restart markers): the bytes it writes
    depend only on the coefficients, quantisation tables, frame and the markers it copies."""
    if not JPEGTRAN:
        return 'failed: jpegtran not found (set JPEGTRAN)'
    try:
        r = subprocess.run([JPEGTRAN, '-copy', 'all'], input=data, capture_output=True, timeout=600)
    except Exception as e:
        return 'failed: ' + type(e).__name__
    # Exit status 2: a warning about the input, with the whole output written.
    if r.returncode not in (0, 2) or not r.stdout:
        return 'failed: ' + r.stderr.decode('latin-1')[:80]
    return f'{r.returncode}:' + hashlib.sha256(r.stdout).hexdigest()


def stream_difference(x, y, pending, path):
    """None if both streams have the same content, else what differs. A JPEG whose bytes changed is also put in
    `pending` for pdf.js, which looks at all of a file's JPEGs at once."""
    a, b = stream_content(x), stream_content(y)
    if not (a.startswith('undecodable') and b.startswith('undecodable')):
        return None if a == b else 'stream content differs'
    # qpdf undoes lossy filters (DCT) only at decode level "all": compare a JPEG's own bytes, and when they
    # differ (other Huffman tables), its coefficients and the pixels several decoders give.
    try:
        ja, jb = jpeg_bytes(x), jpeg_bytes(y)
    except Exception:
        return 'stream content differs (JPEG not readable)'
    if ja is None and jb is None:
        return None if a == b else 'stream content differs'
    if ja is None or jb is None or dct_parms(x) != dct_parms(y):
        return 'stream content differs (JPEG on one side only, or other DCT parameters)'
    if ja == jb:
        return None
    pa, pb = jpeg_pixels(ja), jpeg_pixels(jb)
    if pa != pb or any('failed' in p for p in pa):
        return 'JPEG pixels differ (qpdf, Pillow)'
    ca, cb = jpeg_coefficients(ja), jpeg_coefficients(jb)
    if 'failed' in ca:
        return 'JPEG coefficients not compared: ' + ca
    if ca != cb:
        return 'JPEG coefficients differ (jpegtran)'
    pending.append((ja, jb, path))
    return None


def pdfjs_difference(pending, problems):
    """pdf.js's own JPEG decoder on every changed JPEG of the file (one node process)."""
    if not pending:
        return
    mods = os.environ.get('PDFJS_MODULES')
    if not mods:
        problems.append(f'{len(pending)} changed JPEGs not compared in pdf.js (set PDFJS_MODULES)')
        return
    with tempfile.TemporaryDirectory() as td:
        pairs = []
        for i, (ja, jb, _) in enumerate(pending):
            fa, fb = os.path.join(td, f'{i}a.jpg'), os.path.join(td, f'{i}b.jpg')
            open(fa, 'wb').write(ja)
            open(fb, 'wb').write(jb)
            pairs.append([fa, fb])
        lst = os.path.join(td, 'pairs.json')
        open(lst, 'w').write(json.dumps(pairs))
        try:
            r = subprocess.run(['node', '--max-old-space-size=4096', os.path.join(HERE, 'verify_jpeg_pdfjs.mjs'), lst],
                               capture_output=True, text=True, timeout=1200, env=dict(os.environ, PDFJS_MODULES=mods))
            res = json.loads(r.stdout.strip().splitlines()[-1])
        except Exception as e:
            problems.append(f'pdf.js JPEG check failed: {type(e).__name__}')
            return
    if len(res) != len(pending):
        problems.append('pdf.js JPEG check: wrong number of results')
        return
    for (_, _, path), r in zip(pending, res):
        if not r['same']:
            problems.append(f"{path}: JPEG decodes differently in pdf.js ({r['detail']})"[:300])


def graphs_same(a, b, problems, pending):
    """Walk both object graphs from the same starting points in step; references are followed (not compared
    by number), so objects a reader creates or renumbers on its own don't count as differences."""
    seen = set()
    stack = [(a, b, 'trailer')]
    count = 0
    while stack and len(problems) < 6:
        x, y, path = stack.pop()
        if is_ind(x) or is_ind(y):
            if not (is_ind(x) and is_ind(y)):
                # One side direct, the other indirect: compare the values.
                pass
            else:
                key = (x.objgen, y.objgen)
                if key in seen:
                    continue
                seen.add(key)
                count += 1
        if isinstance(x, pikepdf.Stream) or isinstance(y, pikepdf.Stream):
            if not (isinstance(x, pikepdf.Stream) and isinstance(y, pikepdf.Stream)):
                problems.append(f'{path}: stream vs non-stream'); continue
            ix, iy = items(x.stream_dict, SKIP_STREAM_KEYS), items(y.stream_dict, SKIP_STREAM_KEYS)
            if [k for k, _ in ix] != [k for k, _ in iy]:
                problems.append(f'{path}: stream dictionary keys differ'); continue
            why = stream_difference(x, y, pending, path)
            if why:
                problems.append(f'{path}: {why}'); continue
            stack.extend((vx, vy, path + '/' + k.decode('latin-1')) for (k, vx), (_, vy) in zip(ix, iy))
        elif isinstance(x, pikepdf.Dictionary) or isinstance(y, pikepdf.Dictionary):
            if not (isinstance(x, pikepdf.Dictionary) and isinstance(y, pikepdf.Dictionary)):
                problems.append(f'{path}: dictionary vs other'); continue
            ix, iy = items(x), items(y)
            if [k for k, _ in ix] != [k for k, _ in iy]:
                problems.append(f'{path}: keys differ'); continue
            stack.extend((vx, vy, path + '/' + k.decode('latin-1')) for (k, vx), (_, vy) in zip(ix, iy))
        elif isinstance(x, pikepdf.Array) or isinstance(y, pikepdf.Array):
            if not (isinstance(x, pikepdf.Array) and isinstance(y, pikepdf.Array)) or len(x) != len(y):
                problems.append(f'{path}: array differs'); continue
            stack.extend((x[i], y[i], f'{path}[{i}]') for i in range(len(x)))
        else:
            if scalar(x) != scalar(y):
                problems.append(f'{path}: value differs')
    return count


def objects_same(a_path, b_path):
    problems = []
    with pikepdf.open(a_path) as a, pikepdf.open(b_path) as b:
        ta = items(a.trailer, ('/Prev', '/XRefStm', '/Size', '/Type', '/W', '/Index', '/Filter', '/DecodeParms', '/Length', '/Encrypt'))
        tb = items(b.trailer, ('/Prev', '/XRefStm', '/Size', '/Type', '/W', '/Index', '/Filter', '/DecodeParms', '/Length', '/Encrypt'))
        if [k for k, _ in ta] != [k for k, _ in tb]:
            problems.append('trailer keys differ')
            return problems, 0, {}
        n = 0
        pending = []
        for (k, va), (_, vb) in zip(ta, tb):
            n += graphs_same(va, vb, problems, pending)
        pdfjs_difference(pending, problems)
        return problems, n, {'jpegs_changed': len(pending)}


def render_same(a_path, b_path, max_pages):
    problems = []
    def load(p):
        try:
            return pdfium.PdfDocument(p), None
        except Exception as e:
            return None, type(e).__name__
    (da, ea), (db, eb) = load(a_path), load(b_path)
    if da is None or db is None:
        # Both unreadable by PDFium in the same way is not a difference.
        return ([] if (da is None and db is None and ea == eb) else [f'PDFium loads one file only ({ea}, {eb})']), 0
    if len(da) != len(db):
        return [f'page count {len(da)} vs {len(db)}'], 0
    n = min(len(da), max_pages)
    def shot(p, scale):
        try:
            return hashlib.sha256(p.render(scale=scale).to_pil().tobytes()).hexdigest()
        except Exception as e:
            return 'render failed: ' + type(e).__name__ + ': ' + str(e)[:80]
    for i in range(n):
        pa, pb = da[i], db[i]
        if pa.get_size() != pb.get_size():
            problems.append(f'page {i + 1} size differs')
            continue
        # Same scale for both; giant pages are rendered smaller (at most about 16 million pixels).
        w, h = pa.get_size()
        scale = min(0.75, (16e6 / max(w * h, 1)) ** 0.5)
        if shot(pa, scale) != shot(pb, scale):
            problems.append(f'page {i + 1} renders differently')
        ta, tb = pa.get_textpage().get_text_range(), pb.get_textpage().get_text_range()
        if ta != tb:
            problems.append(f'page {i + 1} text differs')
        if len(problems) > 3:
            break
    return problems, n


def main():
    a, b = sys.argv[1], sys.argv[2]
    max_pages = int(sys.argv[sys.argv.index('--pages') + 1]) if '--pages' in sys.argv else 30
    res = {'file': a}
    chk = subprocess.run(['qpdf', '--check', b], capture_output=True, text=True)
    res['qpdf_check'] = chk.returncode  # 0 ok, 3 warnings, 2 errors
    res['qpdf_check_input'] = subprocess.run(['qpdf', '--check', a], capture_output=True, text=True).returncode
    try:
        p, n, more = objects_same(a, b)
        res['objects'] = n
        res.update(more)
        res['object_problems'] = p
    except Exception as e:
        res['object_problems'] = [f'pikepdf failed: {type(e).__name__}: {e}'[:200]]
    try:
        p, n = render_same(a, b, max_pages)
        res['pages_checked'] = n
        res['render_problems'] = p
    except Exception as e:
        res['render_problems'] = [f'pdfium failed: {type(e).__name__}: {e}'[:200]]
    # The output must check at least as clean as the input did.
    qpdf_ok = res['qpdf_check'] in (0, 3) or res['qpdf_check'] == res['qpdf_check_input']
    res['identical'] = qpdf_ok and not res['object_problems'] and not res['render_problems']
    print(json.dumps(res))
    sys.exit(0 if res['identical'] else 1)


if __name__ == '__main__':
    main()
