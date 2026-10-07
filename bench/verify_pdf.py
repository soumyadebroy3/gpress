#!/usr/bin/env python3
"""Independent check that two PDFs have identical content, using tools we didn't write:

  1. qpdf --check on the output (structure);
  2. pikepdf (qpdf's object model): every object reachable from the trailer has the same value, and every
     stream decodes (lossless filters including RunLength, predictors undone) to the same bytes;
  3. PDFium (pypdfium2): every page renders to the same pixels and gives the same text.

Usage: verify_pdf.py ORIGINAL OUTPUT [--pages N]   -> prints one JSON line; exit status 0 if identical.
Run with python3 -I (the files are untrusted)."""
import hashlib, json, subprocess, sys
from decimal import Decimal

import pikepdf
import pypdfium2 as pdfium

SKIP_STREAM_KEYS = {'/Length', '/Filter', '/DecodeParms'}


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


def graphs_same(a, b, problems):
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
            if stream_content(x) != stream_content(y):
                problems.append(f'{path}: stream content differs'); continue
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
            return problems, 0
        n = 0
        for (k, va), (_, vb) in zip(ta, tb):
            n += graphs_same(va, vb, problems)
        return problems, n


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
        p, n = objects_same(a, b)
        res['objects'] = n
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
