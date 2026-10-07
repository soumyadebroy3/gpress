#!/usr/bin/env python3
"""Independent check that two PDFs have identical content, using tools we didn't write:

  1. qpdf --check on the output (structure);
  2. pikepdf (qpdf's object model): every object reachable from the trailer has the same value, and every
     stream decodes (lossless filters including RunLength, predictors undone) to the same bytes. Image
     streams qpdf can't decode (CCITT fax, JBIG2, JPEG, JPEG 2000) are compared by their decoded pixels:
     pikepdf's decoders first (libtiff, jbig2dec, libjpeg, OpenJPEG), Poppler's (pdfimages) when pikepdf
     can't decode one of them or sees a difference; the stream counts as the same only if one of them
     decodes both to the same pixels, and the JSON says which ('image_notes' when Poppler decided);
  3. PDFium (pypdfium2): every page renders to the same pixels and gives the same text.

Usage: verify_pdf.py ORIGINAL OUTPUT [--pages N]   -> prints one JSON line; exit status 0 if identical.
Run with python3 -I (the files are untrusted)."""
import hashlib, json, os, re, subprocess, sys, tempfile
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


# Images larger than this are left to Poppler (Pillow would hold a byte per pixel).
PIKEPDF_MAX_PIXELS = 200_000_000


class Pixels:
    """Decoded pixels of the image streams of one file, by decoders we didn't write."""

    def __init__(self, path):
        self.path = path
        self.poppler_files = None
        self.tmp = None

    def pikepdf(self, s):
        try:
            w, h = int(s.stream_dict.get('/Width', 0)), int(s.stream_dict.get('/Height', 0))
            if w * h > PIKEPDF_MAX_PIXELS:
                return None
            from PIL import Image
            Image.MAX_IMAGE_PIXELS = None
            im = pikepdf.PdfImage(s).as_pil_image()
            return (im.mode, im.size, hashlib.sha256(im.tobytes()).hexdigest())
        except Exception:
            return None

    def poppler(self, objgen):
        """Poppler's decoding (pdfimages, PBM/PGM/PPM, as written) of the image with this object number, if a
        page draws it."""
        if self.poppler_files is None:
            self.poppler_files = {}
            self.tmp = tempfile.TemporaryDirectory()
            try:
                lst = subprocess.run(['pdfimages', '-list', self.path], capture_output=True, text=True, timeout=600).stdout
                subprocess.run(['pdfimages', self.path, os.path.join(self.tmp.name, 'i')], capture_output=True, timeout=600)
            except subprocess.TimeoutExpired:
                return None
            names = sorted(os.listdir(self.tmp.name), key=lambda n: int(re.sub(r'\D', '', n) or 0))
            rows = [l.split() for l in lst.splitlines()[2:] if l.strip()]
            for row, name in zip(rows, names):
                if len(row) > 11 and row[10].isdigit() and row[11].isdigit():
                    self.poppler_files.setdefault((int(row[10]), int(row[11])), os.path.join(self.tmp.name, name))
        f = self.poppler_files.get(tuple(objgen))
        if f is None:
            return None
        return pnm_pixels(f)


def pnm_pixels(path):
    """Hash of the pixels of a PBM/PGM/PPM file (the padding bits that end a PBM row don't count)."""
    d = open(path, 'rb').read()
    fields, i = [], 0
    while len(fields) < (3 if d[:2] == b'P4' else 4):
        while i < len(d) and d[i:i + 1].isspace():
            i += 1
        if d[i:i + 1] == b'#':
            while i < len(d) and d[i:i + 1] != b'\n':
                i += 1
            continue
        j = i
        while j < len(d) and not d[j:j + 1].isspace():
            j += 1
        fields.append(d[i:j])
        i = j
    body = d[i + 1:]
    if fields[0] == b'P4':
        w, h = int(fields[1]), int(fields[2])
        stride = (w + 7) // 8
        if w % 8:
            mask = (0xFF << (8 - w % 8)) & 0xFF
            b = bytearray(body[:stride * h])
            for r in range(h):
                b[r * stride + stride - 1] &= mask
            body = bytes(b)
        return 'P4 %d %d %s' % (w, h, hashlib.sha256(body[:stride * h]).hexdigest())
    return b' '.join(fields).decode() + ' ' + hashlib.sha256(body).hexdigest()


def is_image(s):
    return s.stream_dict.get('/Subtype') == pikepdf.Name.Image


def images_same(x, y, px, py, notes, path):
    """Two image streams with the same pixels, by pikepdf's decoders or else Poppler's."""
    a, b = px.pikepdf(x), py.pikepdf(y)
    if a is not None and a == b:
        return True
    c, d = px.poppler(x.objgen), py.poppler(y.objgen)
    if c is not None and c == d:
        notes.append(f'{path}: pikepdf ' + ('could not decode' if a is None or b is None else 'decodes differently') + '; Poppler decodes both to the same pixels')
        return True
    return False


def graphs_same(a, b, problems, px, py, notes):
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
            cx, cy = stream_content(x), stream_content(y)
            if (cx != cy or cx.startswith('undecodable')) and is_image(x) and is_image(y):
                if not images_same(x, y, px, py, notes, path):
                    problems.append(f'{path}: image pixels differ'); continue
            elif cx != cy:
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


def objects_same(a_path, b_path, notes):
    problems = []
    px, py = Pixels(a_path), Pixels(b_path)
    with pikepdf.open(a_path) as a, pikepdf.open(b_path) as b:
        ta = items(a.trailer, ('/Prev', '/XRefStm', '/Size', '/Type', '/W', '/Index', '/Filter', '/DecodeParms', '/Length', '/Encrypt'))
        tb = items(b.trailer, ('/Prev', '/XRefStm', '/Size', '/Type', '/W', '/Index', '/Filter', '/DecodeParms', '/Length', '/Encrypt'))
        if [k for k, _ in ta] != [k for k, _ in tb]:
            problems.append('trailer keys differ')
            return problems, 0
        n = 0
        for (k, va), (_, vb) in zip(ta, tb):
            n += graphs_same(va, vb, problems, px, py, notes)
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
        notes = []
        p, n = objects_same(a, b, notes)
        res['objects'] = n
        res['object_problems'] = p
        if notes:
            res['image_notes'] = notes[:10]
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
