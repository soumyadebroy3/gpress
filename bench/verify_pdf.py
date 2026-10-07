#!/usr/bin/env python3
"""Independent check that two PDFs have identical content, using tools we didn't write:

  1. qpdf --check on the output (structure);
  2. pikepdf (qpdf's object model): every object reachable from the trailer has the same value, and every
     stream decodes (general filters, predictors undone) to the same bytes;
  3. PDFium (pypdfium2): every page renders to the same pixels and gives the same text.

Usage: verify_pdf.py ORIGINAL OUTPUT [--pages N]   -> prints one JSON line; exit status 0 if identical.
Run with python3 -I (the files are untrusted)."""
import hashlib, json, subprocess, sys
from decimal import Decimal

import pikepdf
import pypdfium2 as pdfium

SKIP_STREAM_KEYS = {'/Length', '/Filter', '/DecodeParms'}


def canon(o, seen_ok=True):
    """Value of a pikepdf object, with references as (num, gen) and numbers as exact decimals."""
    if isinstance(o, pikepdf.Object) and o.is_indirect and not isinstance(o, pikepdf.Stream) and seen_ok is False:
        return ('ref', o.objgen)
    if isinstance(o, bool):
        return ('bool', o)
    if isinstance(o, int):
        return ('num', Decimal(o))
    if isinstance(o, Decimal):
        return ('num', o)
    if isinstance(o, float):
        return ('num', Decimal(repr(o)))
    if isinstance(o, pikepdf.Name):
        return ('name', str(o))
    if isinstance(o, pikepdf.String):
        return ('str', bytes(o))
    if isinstance(o, pikepdf.Array):
        return ('arr', tuple(ref_or_canon(x) for x in o))
    if isinstance(o, pikepdf.Stream):
        return ('stream', dict_items(o.stream_dict, skip=SKIP_STREAM_KEYS))
    if isinstance(o, pikepdf.Dictionary):
        return ('dict', dict_items(o))
    if o is None:
        return ('null',)
    try:
        if o == pikepdf.Object.parse(b'null'):
            return ('null',)
    except Exception:
        pass
    return ('other', repr(o))


def ref_or_canon(x):
    if isinstance(x, pikepdf.Object) and x.is_indirect:
        return ('ref', x.objgen)
    return canon(x)


def dict_items(d, skip=()):
    return tuple(sorted((str(k), ref_or_canon(d.get(k))) for k in d.keys() if str(k) not in skip))


def reachable(pdf):
    """objgen -> object, for everything reachable from the trailer (not following /Length)."""
    out, todo = {}, []
    tr = pdf.trailer
    for k in tr.keys():
        if str(k) in ('/Prev', '/XRefStm', '/Size', '/Encrypt'):
            continue
        todo.append(tr.get(k))
    while todo:
        o = todo.pop()
        if isinstance(o, pikepdf.Object) and o.is_indirect:
            if o.objgen in out:
                continue
            out[o.objgen] = o
        if isinstance(o, pikepdf.Stream):
            items = [(k, o.stream_dict.get(k)) for k in o.stream_dict.keys() if str(k) != '/Length']
        elif isinstance(o, pikepdf.Dictionary):
            items = [(k, o.get(k)) for k in o.keys()]
        elif isinstance(o, pikepdf.Array):
            items = [(None, x) for x in o]
        else:
            items = []
        for _, v in items:
            if isinstance(v, (pikepdf.Dictionary, pikepdf.Array, pikepdf.Stream)):
                if v.is_indirect and v.objgen in out:
                    continue
                todo.append(v)
            elif isinstance(v, pikepdf.Object) and v.is_indirect:
                todo.append(v)
    return out


def stream_content(s):
    try:
        return hashlib.sha256(s.read_bytes(decode_level=pikepdf.StreamDecodeLevel.generalized)).hexdigest()
    except Exception as e:
        return 'undecodable:' + type(e).__name__


def objects_same(a_path, b_path):
    problems = []
    with pikepdf.open(a_path) as a, pikepdf.open(b_path) as b:
        ra, rb = reachable(a), reachable(b)
        if set(ra) != set(rb):
            problems.append(f'reachable objects differ: {len(set(ra) ^ set(rb))}')
        for k in sorted(set(ra) & set(rb)):
            oa, ob = ra[k], rb[k]
            if canon(oa) != canon(ob):
                problems.append(f'object {k} differs')
            elif isinstance(oa, pikepdf.Stream):
                if stream_content(oa) != stream_content(ob):
                    problems.append(f'stream {k} content differs')
            if len(problems) > 5:
                break
        for key in ('/Root', '/Info', '/ID'):
            va, vb = a.trailer.get(key), b.trailer.get(key)
            if (va is None) != (vb is None) or (va is not None and ref_or_canon(va) != ref_or_canon(vb)):
                problems.append(f'trailer {key} differs')
        return problems, len(ra)


def render_same(a_path, b_path, max_pages):
    problems = []
    da, db = pdfium.PdfDocument(a_path), pdfium.PdfDocument(b_path)
    if len(da) != len(db):
        return [f'page count {len(da)} vs {len(db)}'], 0
    n = min(len(da), max_pages)
    for i in range(n):
        pa, pb = da[i], db[i]
        ia = pa.render(scale=0.75).to_pil().tobytes()
        ib = pb.render(scale=0.75).to_pil().tobytes()
        if hashlib.sha256(ia).digest() != hashlib.sha256(ib).digest():
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
    res['identical'] = res['qpdf_check'] in (0, 3) and not res['object_problems'] and not res['render_problems']
    print(json.dumps(res))
    sys.exit(0 if res['identical'] else 1)


if __name__ == '__main__':
    main()
