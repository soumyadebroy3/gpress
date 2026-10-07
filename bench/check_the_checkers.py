#!/usr/bin/env python3
"""The checks must catch small changes. From a PDF with text, a PDF with a RunLength image and a PDF with a
baseline JPEG image, this makes copies that differ in one small way each, and runs verify_pdf.py (qpdf, pikepdf,
PDFium) and verify_render.py (Poppler, pdf.js) on them:

  unchanged copy (rewritten by pikepdf)  -> must pass
  one character of the page content      -> must be flagged
  one value in the document information  -> must be flagged (by the object check; pictures don't show it)
  the page size                          -> must be flagged
  one bit of one pixel of the image      -> must be flagged
  one coefficient of the JPEG            -> must be flagged
  the JPEG with other Huffman tables     -> must pass (jpegtran -optimize; other bytes, the same pixels)

  PDFJS_MODULES=... [JPEGTRAN=...] check_the_checkers.py TEXT_PDF RLE_PDF JPEG_PDF WORKDIR
Run with python3 -I."""
import importlib.util, json, os, subprocess, sys

import pikepdf

HERE = os.path.dirname(os.path.abspath(__file__))
spec = importlib.util.spec_from_file_location('verify_render', os.path.join(HERE, 'verify_render.py'))
vr = importlib.util.module_from_spec(spec)
spec.loader.exec_module(vr)


def rle(data):
    out = bytearray()
    for i in range(0, len(data), 128):
        chunk = data[i:i + 128]
        out.append(len(chunk) - 1)
        out += chunk
    out.append(128)
    return bytes(out)


def one_coefficient_changed(jpeg):
    """The JPEG with one AC coefficient changed: the first extra bit of the first coded AC coefficient is
    flipped (a value v becomes another value of the same size class), so every code keeps its place and only
    that coefficient changes. Baseline JPEG without restart markers only."""
    d = bytearray(jpeg)
    tables, comps, i = {}, {}, 2
    while True:
        while d[i] == 0xFF:
            i += 1
        m, n = d[i], int.from_bytes(d[i + 1:i + 3], 'big')
        body = d[i + 3:i + 1 + n]
        if m in (0xC0, 0xC1):
            comps = {body[6 + 3 * k]: (body[7 + 3 * k] >> 4) * (body[7 + 3 * k] & 15) for k in range(body[5])}
        elif m == 0xC4:
            while body:
                c = sum(body[1:17])
                tables[body[0]] = (list(body[1:17]), list(body[17:17 + c]))
                body = body[17 + c:]
        elif m == 0xDA:
            break
        elif m in (0xC2, 0xC3, 0xC9, 0xCA, 0xCB, 0xDD):
            raise SystemExit('the JPEG PDF needs a baseline JPEG without restart markers')
        i += 1 + n
    # Blocks of an MCU: (DC table, AC table) for each.
    ns = body[0]
    blocks = []
    for k in range(ns):
        t = body[2 + 2 * k]
        blocks += [(t >> 4, 0x10 | t & 15)] * (comps[body[1 + 2 * k]] if ns > 1 else 1)
    # Byte offset of every data byte (stuffed zero bytes skipped).
    at, j = [], i + 1 + n
    while d[j] != 0xFF or d[j + 1] == 0:
        at.append(j)
        j += 2 if d[j] == 0xFF else 1
    p = 0

    def bit():
        nonlocal p
        b = d[at[p // 8]] >> (7 - p % 8) & 1
        p += 1
        return b

    def symbol(t):
        counts, values = tables[t]
        code = first = k = 0
        for length in range(16):
            code = code << 1 | bit()
            if code - first < counts[length]:
                return values[k + code - first]
            k += counts[length]
            first = (first + counts[length]) << 1
        raise SystemExit('bad Huffman code')

    for b in range(10000):
        dc, ac = blocks[b % len(blocks)]
        p += symbol(dc)
        k = 1
        while k < 64:
            rs = symbol(ac)
            r, s = rs >> 4, rs & 15
            if s == 0:
                if r != 15:
                    break
                k += 16
                continue
            byte, mask = at[p // 8], 0x80 >> p % 8
            if d[byte] != 0xFF and d[byte] ^ mask != 0xFF:
                d[byte] ^= mask
                return bytes(d)
            p += s
            k += r + 1
    raise SystemExit('no AC coefficient to change')


def make(text_pdf, rle_pdf, jpeg_pdf, work):
    cases = []
    def save(p, name):
        # pikepdf gives a saved file a new second /ID; put the original one back (same length, in place), so
        # the only difference left is the one each case makes.
        orig_id = [bytes(x) for x in p.trailer.ID] if '/ID' in p.trailer else None
        path = os.path.join(work, name)
        p.save(path, compress_streams=False, stream_decode_level=pikepdf.StreamDecodeLevel.none)
        if orig_id:
            with pikepdf.open(path) as q:
                new_id = bytes(q.trailer.ID[1])
            data = open(path, 'rb').read()
            a, b = new_id.hex().encode(), orig_id[1].hex().encode()
            if len(a) == len(b) and data.count(a) == 1:
                open(path, 'wb').write(data.replace(a, b))
        return path
    with pikepdf.open(text_pdf) as p:
        cases.append(('unchanged copy', text_pdf, save(p, 'same.pdf'), False))
    with pikepdf.open(text_pdf) as p:
        page = p.pages[0]
        data = bytearray(page.Contents.read_bytes() if isinstance(page.Contents, pikepdf.Stream) else page.Contents[0].read_bytes())
        # The first character of the first string shown on the page.
        k = next((j + 1 for j in range(len(data) - 2) if data[j] == ord('(') and data[j + 1] not in b')\\'), -1)
        if k <= 0:
            raise SystemExit('the text PDF needs a text string in its first content stream')
        data[k] = ord('X') if data[k] != ord('X') else ord('Y')
        target = page.Contents if isinstance(page.Contents, pikepdf.Stream) else page.Contents[0]
        target.write(bytes(data))
        cases.append(('one character of the content', text_pdf, save(p, 'content.pdf'), True))
    with pikepdf.open(text_pdf) as p:
        with p.open_metadata(set_pikepdf_as_editor=False, update_docinfo=False):
            pass
        p.docinfo[pikepdf.Name.Title] = 'Changed'
        cases.append(('document information', text_pdf, save(p, 'info.pdf'), True))
    with pikepdf.open(text_pdf) as p:
        mb = [float(x) for x in p.pages[0].MediaBox]
        p.pages[0].MediaBox = pikepdf.Array([mb[0], mb[1], mb[2] + 1, mb[3]])
        cases.append(('page size', text_pdf, save(p, 'mediabox.pdf'), True))
    with pikepdf.open(rle_pdf) as p:
        for name, im in p.pages[0].Resources.XObject.items():
            if im.get('/Filter') in (pikepdf.Name.RunLengthDecode, pikepdf.Array([pikepdf.Name.RunLengthDecode])):
                d = bytearray(im.read_bytes(decode_level=pikepdf.StreamDecodeLevel.specialized))
                d[len(d) // 2] ^= 1
                im.write(rle(bytes(d)), filter=pikepdf.Name.RunLengthDecode)
                break
        else:
            raise SystemExit('the RunLength PDF needs a RunLength image on its first page')
        cases.append(('one bit of one pixel', rle_pdf, save(p, 'pixel.pdf'), True))

    def first_jpeg(p):
        for name, im in p.pages[0].Resources.XObject.items():
            if im.get('/Filter') in (pikepdf.Name.DCTDecode, pikepdf.Array([pikepdf.Name.DCTDecode])):
                return im
        raise SystemExit('the JPEG PDF needs a DCTDecode image on its first page')
    with pikepdf.open(jpeg_pdf) as p:
        im = first_jpeg(p)
        im.write(one_coefficient_changed(im.read_raw_bytes()), filter=pikepdf.Name.DCTDecode)
        cases.append(('one coefficient of the JPEG', jpeg_pdf, save(p, 'coefficient.pdf'), True))
    jpegtran = os.environ.get('JPEGTRAN', 'jpegtran')
    with pikepdf.open(jpeg_pdf) as p:
        im = first_jpeg(p)
        try:
            other = subprocess.run([jpegtran, '-optimize', '-copy', 'all'], input=im.read_raw_bytes(), capture_output=True, check=True).stdout
        except (OSError, subprocess.CalledProcessError):
            print('(no jpegtran: the case "the JPEG with other Huffman tables" is left out)')
        else:
            im.write(other, filter=pikepdf.Name.DCTDecode)
            cases.append(('the JPEG with other Huffman tables', jpeg_pdf, save(p, 'tables.pdf'), False))
    return cases


def main():
    text_pdf, rle_pdf, jpeg_pdf, work = sys.argv[1:5]
    os.makedirs(work, exist_ok=True)
    ok = True
    for what, orig, changed, should_flag in make(text_pdf, rle_pdf, jpeg_pdf, work):
        r = subprocess.run(['python3', '-I', os.path.join(HERE, 'verify_pdf.py'), orig, changed], capture_output=True, text=True)
        objects_renderers = json.loads(r.stdout.strip().splitlines()[-1])
        render = vr.compare(orig, changed)
        flagged = {'qpdf/pikepdf/PDFium': not objects_renderers['identical'], 'Poppler/pdf.js': not render['identical']}
        caught = any(flagged.values())
        good = caught == should_flag
        ok &= good
        print(f"{'ok ' if good else 'BAD'} {what}: {'flagged by ' + ', '.join(k for k, v in flagged.items() if v) if caught else 'passed'}")
    sys.exit(0 if ok else 1)


if __name__ == '__main__':
    main()
