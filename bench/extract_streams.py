#!/usr/bin/env python3
"""Pull every Flate stream out of a set of PDFs, as the Flate decoder sees it (predictor bytes kept),
deduplicated by content. Writes OUT/streams/<sha1>.bin and OUT/manifest.tsv.

Run with python3 -I: the PDFs are untrusted input."""
import hashlib, os, sys, zlib
import pikepdf

def kind_of(d, content_ids, og):
    t, st = d.get('/Type'), d.get('/Subtype')
    if og in content_ids: return 'content'
    if st == '/Image': return 'image'
    if st == '/Form': return 'form'
    if t == '/ObjStm': return 'objstm'
    if t == '/XRef': return 'xref'
    if t == '/Metadata': return 'xmp'
    if '/Length1' in d or '/Length2' in d or st in ('/Type1C', '/CIDFontType0C', '/OpenType'): return 'font'
    if '/N' in d and ('/Alternate' in d or '/Range' in d): return 'icc'
    return 'other'

def main(out, files, cap=8 << 20):
    os.makedirs(os.path.join(out, 'streams'), exist_ok=True)
    seen = set()
    man = open(os.path.join(out, 'manifest.tsv'), 'w')
    stats = {'pdf_ok': 0, 'pdf_fail': 0, 'streams': 0, 'dups': 0, 'bad_flate': 0, 'too_big': 0}
    for f in files:
        try:
            pdf = pikepdf.open(f)
        except Exception:
            stats['pdf_fail'] += 1
            continue
        stats['pdf_ok'] += 1
        content_ids = set()
        try:
            for p in pdf.pages:
                c = p.obj.get('/Contents')
                if c is None: continue
                for s in (c if isinstance(c, pikepdf.Array) else [c]):
                    if s.is_indirect: content_ids.add(s.objgen)
        except Exception:
            pass
        for obj in pdf.objects:
            if not isinstance(obj, pikepdf.Stream): continue
            try:
                d = obj.stream_dict
                flt = d.get('/Filter')
                if isinstance(flt, pikepdf.Array):
                    if len(flt) != 1: continue
                    flt = flt[0]
                if flt != '/FlateDecode': continue
                raw = obj.read_raw_bytes()
                dec = zlib.decompressobj()
                data = dec.decompress(raw, cap + 1)
                if len(data) > cap:
                    stats['too_big'] += 1; continue
                if not dec.eof:
                    stats['bad_flate'] += 1; continue
                stats['streams'] += 1
                h = hashlib.sha1(data).hexdigest()
                if h in seen:
                    stats['dups'] += 1; continue
                seen.add(h)
                with open(os.path.join(out, 'streams', h + '.bin'), 'wb') as fh:
                    fh.write(data)
                man.write(f"{h}\t{len(data)}\t{len(raw)}\t{kind_of(d, content_ids, obj.objgen)}\t{os.path.basename(f)}\n")
            except Exception:
                stats['bad_flate'] += 1
        pdf.close()
    man.close()
    print(stats)

if __name__ == '__main__':
    main(sys.argv[1], sys.argv[2:])
