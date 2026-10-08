#!/usr/bin/env python3
"""Scanned black-and-white pages (CCITT fax images): both outputs on a list of PDFs, before and after.

  fax_bench.py FILES.txt OUT        FILES.txt: one PDF per line

Needs two engine binaries built with --features existing: $PDFE (ours) and $PDFE_BASE (the commit before
this work). For every file:

  normal PDF   pdfe optimize, base and ours (bytes, outcome);
  peyaji       pdfe pack, base and ours; ours restored with a separate pdfe unpack and compared byte for byte;

and for every CCITT image stream (found with pikepdf):

  original bytes; ours as a peyaji fax part (pdfe fax-model, restore checked); xz -9e of the original
  bytes; jbig2enc's lossless generic coding of the same pixels (jbig2 -p, measured only); our JBIG2 stream
  (what normal-PDF output writes when it is at least 5% smaller).

Writes OUT/files.tsv, OUT/streams.tsv and OUT/report.md. Run with python3 -I (the PDFs are untrusted)."""
import lzma, os, shutil, subprocess, sys, tempfile, time

import pikepdf

PDFE = os.environ.get('GPRESS') or os.environ.get('PDFE') or 'target/release/gpress'
BASE = os.environ['PDFE_BASE']


def run(args, **kw):
    t = time.time()
    r = subprocess.run(args, capture_output=True, text=True, **kw)
    return r, time.time() - t


def optimize(binary, f, out):
    r, t = run([binary, 'optimize', f, out])
    p = r.stdout.strip().split('\t')
    return (int(p[3]), p[1], t) if len(p) > 3 else (-1, 'FAILED', t)


def pack(binary, f, out):
    r, t = run([binary, 'pack', f, out])
    p = r.stdout.strip().split('\t')
    return (int(p[2]), t, r.stdout) if len(p) > 2 else (-1, t, r.stdout)


def fax_streams(f):
    """(object number, K, columns, byte align, raw bytes) of every lone-CCITT image stream."""
    out = []
    try:
        pdf = pikepdf.open(f)
    except Exception:
        return out
    for o in pdf.objects:
        if not isinstance(o, pikepdf.Stream):
            continue
        filt = o.get('/Filter')
        names = [str(x) for x in filt] if isinstance(filt, pikepdf.Array) else ([str(filt)] if filt is not None else [])
        if names not in (['/CCITTFaxDecode'], ['/CCF']):
            continue
        dp = o.get('/DecodeParms')
        if isinstance(dp, pikepdf.Array):
            dp = dp[0]
        if not isinstance(dp, pikepdf.Dictionary):
            dp = pikepdf.Dictionary()
        try:
            k, cols, al = int(dp.get('/K', 0)), int(dp.get('/Columns', 1728)), bool(dp.get('/EncodedByteAlign', False))
        except Exception:
            continue
        out.append((o.objgen[0], k, cols, al, o.read_raw_bytes()))
    return out


def stream_row(td, n, k, cols, al, raw):
    rawp = os.path.join(td, 'raw')
    open(rawp, 'wb').write(raw)
    m, _ = run([PDFE, 'fax-model', rawp, str(k), str(cols), '1' if al else '0'])
    ours = -1
    exact = 'no'
    if ' -> ' in m.stdout:
        ours = int(m.stdout.split(' -> ')[1].split()[0])
        exact = 'yes' if 'exact: true' in m.stdout else 'NO'
    xz = len(lzma.compress(raw, preset=9 | lzma.PRESET_EXTREME))
    pbm = os.path.join(td, 'img.pbm')
    d, _ = run([PDFE, 'fax-decode', rawp, str(k), str(cols), '1' if al else '0', pbm])
    rows = d.stdout.split('rows=')[1].split()[0] if 'rows=' in d.stdout else '?'
    enc = subprocess.run(['jbig2', '-p', pbm], capture_output=True) if shutil.which('jbig2') else None
    jbig2enc = len(enc.stdout) if enc is not None and enc.returncode == 0 else -1
    j, _ = run([PDFE, 'jbig2-encode', pbm, os.path.join(td, 'ours.jb2')])
    our_jbig2 = os.path.getsize(os.path.join(td, 'ours.jb2')) if j.returncode == 0 else -1
    return [n, k, cols, rows, len(raw), ours, exact, xz, jbig2enc, our_jbig2]


def main():
    files = [l.strip() for l in open(sys.argv[1]) if l.strip()]
    out = sys.argv[2]
    os.makedirs(out, exist_ok=True)
    frows, srows = [], []
    with tempfile.TemporaryDirectory() as td:
        for f in files:
            name = os.path.basename(os.path.dirname(f)) + '/' + os.path.basename(f)
            size = os.path.getsize(f)
            bo, bwhy, _ = optimize(BASE, f, os.path.join(td, 'b.pdf'))
            no, nwhy, nt = optimize(PDFE, f, os.path.join(td, 'n.pdf'))
            bp, _, _ = pack(BASE, f, os.path.join(td, 'b.pyj'))
            np_, pt, pout = pack(PDFE, f, os.path.join(td, 'n.pyj'))
            u, _ = run([PDFE, 'unpack', os.path.join(td, 'n.pyj'), os.path.join(td, 'back.pdf')])
            same = u.returncode == 0 and open(os.path.join(td, 'back.pdf'), 'rb').read() == open(f, 'rb').read()
            fax = pout.split('fax: ')[1].split()[0].rstrip(',}') if 'fax: ' in pout else '0'
            frows.append([name, size, bo, bwhy, no, nwhy, f'{nt:.1f}', bp, np_, fax, f'{pt:.1f}', 'exact' if same else 'MISMATCH'])
            print('\t'.join(map(str, frows[-1])), flush=True)
            for n, k, cols, al, raw in fax_streams(f):
                srows.append([name] + stream_row(td, n, k, cols, al, raw))
    with open(os.path.join(out, 'files.tsv'), 'w') as fh:
        fh.write('file\tbytes\tpdf_base\tbase_outcome\tpdf_ours\tours_outcome\tpdf_secs\tpyj_base\tpyj_ours\tfax_parts\tpyj_secs\trestore\n')
        fh.writelines('\t'.join(map(str, r)) + '\n' for r in frows)
    with open(os.path.join(out, 'streams.tsv'), 'w') as fh:
        fh.write('file\tobject\tK\tcolumns\trows\toriginal\tours_peyaji\texact\txz9e\tjbig2enc_generic\tours_jbig2\n')
        fh.writelines('\t'.join(map(str, r)) + '\n' for r in srows)
    tot = lambda rows, i: sum(r[i] for r in rows if isinstance(r[i], int) and r[i] > 0)
    with open(os.path.join(out, 'report.md'), 'w') as fh:
        fh.write('| File | Bytes | Normal PDF before | after | peyaji before | after | restore |\n| --- | ---: | ---: | ---: | ---: | ---: | --- |\n')
        for r in frows:
            fh.write(f'| {r[0]} | {r[1]:,} | {r[2]:,} | {r[4]:,} | {r[7]:,} | {r[8]:,} | {r[11]} |\n')
        fh.write(f'| **Total** | {tot(frows, 1):,} | {tot(frows, 2):,} | {tot(frows, 4):,} | {tot(frows, 7):,} | {tot(frows, 8):,} | |\n\n')
        fh.write('| Fax streams | Original | Ours (peyaji) | xz -9e | jbig2enc generic | Our JBIG2 |\n| --- | ---: | ---: | ---: | ---: | ---: |\n')
        fh.write(f'| {len(srows)} streams | {tot(srows, 5):,} | {tot(srows, 6):,} | {tot(srows, 8):,} | {tot(srows, 9):,} | {tot(srows, 10):,} |\n')


if __name__ == '__main__':
    main()
