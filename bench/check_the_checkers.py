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

Two more cases from a JPEG made here (Pillow, quality 100, 4:4:4), each in a one-page PDF against the same PDF
with the original JPEG:

  one coefficient 1 larger, where libjpeg's integer IDCT gives the same pixels (qpdf and Pillow can't see it)
                                         -> must be flagged
  a layout pdf.js reads differently: a comment holding a small JPEG, the Huffman tables, fill bytes, the rest;
  the same bytes with the tables moved to just before the scan (libjpeg decodes both alike)
                                         -> must be flagged

With --fax FAX_PDF (a CCITT fax image on its first page), two more, both written by libtiff's Group 4
encoder (through Pillow), not by us:

  the fax image coded again, same pixels -> must pass (images are compared by pixels, not bytes)
  one pixel of the fax image flipped     -> must be flagged

  PDFJS_MODULES=... [JPEGTRAN=...] check_the_checkers.py TEXT_PDF RLE_PDF JPEG_PDF WORKDIR [--fax FAX_PDF]
Run with python3 -I."""
import hashlib, importlib.util, io, json, os, subprocess, sys

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


def ac_coefficients(jpeg):
    """The coded AC coefficients of a baseline JPEG without restart markers, in order: (zigzag index, size, bit
    position of the first extra bit in the coded data), and the byte offset of every coded data byte (stuffed
    zero bytes skipped)."""
    d = jpeg
    tables, comps, i = {}, {}, 2
    while True:
        while d[i] == 0xFF:
            i += 1
        m, n = d[i], int.from_bytes(d[i + 1:i + 3], 'big')
        body = d[i + 3:i + 1 + n]
        if m in (0xC0, 0xC1):
            height, width = int.from_bytes(body[1:3], 'big'), int.from_bytes(body[3:5], 'big')
            comps = {body[6 + 3 * k]: (body[7 + 3 * k] >> 4, body[7 + 3 * k] & 15) for k in range(body[5])}
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
    # Blocks of an MCU: (DC table, AC table) for each; and the number of MCUs of the (first) scan.
    ns = body[0]
    hmax, vmax = max(h for h, _ in comps.values()), max(v for _, v in comps.values())
    blocks = []
    for k in range(ns):
        t = body[2 + 2 * k]
        h, v = comps[body[1 + 2 * k]]
        blocks += [(t >> 4, 0x10 | t & 15)] * (h * v if ns > 1 else 1)
    if ns > 1:
        mcus = -(-width // (8 * hmax)) * -(-height // (8 * vmax))
    else:
        h, v = comps[body[1]]
        mcus = -(-width * h // (8 * hmax)) * -(-height * v // (8 * vmax))
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

    out = []
    for b in range(min(mcus * len(blocks), 200000)):
        dc, ac = blocks[b % len(blocks)]
        size = symbol(dc)  # (not `p += symbol(dc)`: that would add to p as it was before the code was read)
        p += size
        k = 1
        while k < 64:
            rs = symbol(ac)
            r, s = rs >> 4, rs & 15
            if s == 0:
                if r != 15:
                    break
                k += 16
                continue
            k += r
            out.append((k, s, p))
            p += s
            k += 1
    return out, at


def flip_bit(jpeg, at, p):
    """The JPEG with coded data bit p flipped, or None if that would make or unmake a 0xFF byte."""
    d = bytearray(jpeg)
    byte, mask = at[p // 8], 0x80 >> p % 8
    if d[byte] == 0xFF or d[byte] ^ mask == 0xFF:
        return None
    d[byte] ^= mask
    return bytes(d)


def one_coefficient_changed(jpeg):
    """The JPEG with one AC coefficient changed: the first extra bit of the first coded AC coefficient is
    flipped (a value v becomes another value of the same size class), so every code keeps its place and only
    that coefficient changes. Baseline JPEG without restart markers only."""
    coefs, at = ac_coefficients(jpeg)
    for _, _, p in coefs:
        changed = flip_bit(jpeg, at, p)
        if changed:
            return changed
    raise SystemExit('no AC coefficient to change')


def libjpeg_pixels(jpeg):
    """Pixels from Pillow and from qpdf (both libjpeg)."""
    from PIL import Image
    im = Image.open(io.BytesIO(jpeg))
    im.load()
    scratch = pikepdf.new()
    s = pikepdf.Stream(scratch, jpeg)
    s.Filter = pikepdf.Name.DCTDecode
    return im.tobytes(), s.read_bytes(decode_level=pikepdf.StreamDecodeLevel.all)


def own_jpeg():
    """A 64 x 64 noise picture at quality 100, 4:4:4 (Pillow), and an 8 x 8 one for a thumbnail."""
    from PIL import Image
    s, px = 7, bytearray()
    for _ in range(64 * 64 * 3):
        s = (s * 1103515245 + 12345) & 0x7fffffff
        px.append(s >> 16 & 255)
    out = []
    for size, q in ((64, 100), (8, 50)):
        f = io.BytesIO()
        Image.frombytes('RGB', (size, size), bytes(px[:size * size * 3])).save(f, 'JPEG', quality=q, subsampling=0)
        out.append(f.getvalue())
    return out


def invisible_coefficient_change(jpeg):
    """The JPEG with one high-frequency coefficient changed by 1 (its lowest extra bit flipped) where libjpeg's
    integer IDCT still gives the same pixels, so qpdf and Pillow see no difference."""
    want = libjpeg_pixels(jpeg)
    coefs, at = ac_coefficients(jpeg)
    for k, size, p in coefs:
        if k >= 20 and size >= 2:
            changed = flip_bit(jpeg, at, p + size - 1)
            if changed and libjpeg_pixels(changed) == want:
                return changed
    raise SystemExit('no coefficient change that libjpeg rounds away')


def segments(jpeg):
    """(marker, bytes) of each segment after the start marker; a scan's include its coded data."""
    out, i = [], 2
    while True:
        s = i
        m = jpeg[i + 1]
        i += 2
        if m == 0xD9:
            out.append((m, jpeg[s:]))
            return out
        i += int.from_bytes(jpeg[i:i + 2], 'big')
        if m == 0xDA:
            while not (jpeg[i] == 0xFF and jpeg[i + 1] != 0 and not 0xD0 <= jpeg[i + 1] <= 0xD7):
                i += 1
        out.append((m, jpeg[s:i]))


def moved_tables(jpeg, thumb):
    """(original, rewritten): SOI, JFIF, a comment holding `thumb`, the Huffman tables, fill bytes, the
    quantisation tables, frame, scan; and the same with the tables just before the scan. Both are valid JPEGs
    libjpeg decodes alike; pdf.js takes the 0xFFFF after the comment for a wrong length and fails."""
    seg = segments(jpeg)
    pick = lambda m: b''.join(x for k, x in seg if k == m)
    com = b'thumbnail:' + thumb
    com = b'\xff\xfe' + (len(com) + 2).to_bytes(2, 'big') + com
    a = b'\xff\xd8' + pick(0xE0) + com + pick(0xC4) + b'\xff\xff' + pick(0xDB) + pick(0xC0) + pick(0xDA) + pick(0xD9)
    b = b'\xff\xd8' + pick(0xE0) + com + b'\xff\xff' + pick(0xDB) + pick(0xC0) + pick(0xC4) + pick(0xDA) + pick(0xD9)
    return a, b


def one_image_pdfs(jpeg, other, w, h, work, name):
    """Two one-page PDFs, the same but for the image's JPEG (and the same /ID)."""
    paths = []
    for tag, data in (('orig', jpeg), ('changed', other)):
        p = pikepdf.new()
        img = pikepdf.Stream(p, data)
        img.Type, img.Subtype, img.Width, img.Height = pikepdf.Name.XObject, pikepdf.Name.Image, w, h
        img.ColorSpace, img.BitsPerComponent, img.Filter = pikepdf.Name.DeviceRGB, 8, pikepdf.Name.DCTDecode
        page = p.add_blank_page(page_size=(w, h))
        page.Resources = pikepdf.Dictionary(XObject=pikepdf.Dictionary(Im1=img))
        page.Contents = p.make_stream(f'q {w} 0 0 {h} 0 0 cm /Im1 Do Q'.encode())
        path = os.path.join(work, f'{name}-{tag}.pdf')
        p.save(path, static_id=True, compress_streams=False)
        paths.append(path)
    return paths


def group4(im, black_is_1):
    """A 1-bit PIL image (as pikepdf decodes a CCITT image: the filter's output bits) as one Group 4 strip,
    coded by libtiff. Checked by decoding it again with pikepdf, so the polarity is right whatever the
    TIFF photometric does."""
    import tifffile
    from PIL import Image, ImageOps
    w, h = im.size
    for inverted in (False, True):
        src = ImageOps.invert(im.convert('L')).convert('1') if inverted else im
        buf = io.BytesIO()
        src.save(buf, 'TIFF', compression='group4', tiffinfo={278: h})
        page = tifffile.TiffFile(io.BytesIO(buf.getvalue())).pages[0]
        if len(page.dataoffsets) != 1:
            raise SystemExit('libtiff wrote more than one strip')
        data = buf.getvalue()[page.dataoffsets[0]:page.dataoffsets[0] + page.databytecounts[0]]
        test = pikepdf.new()
        parms = pikepdf.Dictionary(K=-1, Columns=w, Rows=h, BlackIs1=black_is_1)
        st = pikepdf.Stream(test, data, Type=pikepdf.Name.XObject, Subtype=pikepdf.Name.Image, Width=w, Height=h,
                            BitsPerComponent=1, ColorSpace=pikepdf.Name.DeviceGray, Filter=pikepdf.Name.CCITTFaxDecode, DecodeParms=parms)
        if pikepdf.PdfImage(st).as_pil_image().tobytes() == im.tobytes():
            return data, parms
    raise SystemExit('could not code the fax image with libtiff')


def make(text_pdf, rle_pdf, jpeg_pdf, work, fax_pdf=None):
    cases = []
    def save(p, name):
        # pikepdf gives a saved file a new second /ID; put the original one back (same length, in place), so
        # the only difference left is the one each case makes (and it must not edit the XMP metadata).
        orig_id = [bytes(x) for x in p.trailer.ID] if '/ID' in p.trailer else None
        path = os.path.join(work, name)
        p.save(path, compress_streams=False, stream_decode_level=pikepdf.StreamDecodeLevel.none, fix_metadata_version=False)
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
    jpeg, thumb = own_jpeg()
    a, b = one_image_pdfs(jpeg, invisible_coefficient_change(jpeg), 64, 64, work, 'coefficient-by-one')
    cases.append(('one coefficient 1 larger, the same pixels in libjpeg', a, b, True))
    a, b = one_image_pdfs(*moved_tables(jpeg, thumb), 64, 64, work, 'moved-tables')
    cases.append(('a JPEG layout only pdf.js reads differently', a, b, True))
    if fax_pdf:
        for flip in (False, True):
            with pikepdf.open(fax_pdf) as p:
                for name, im in p.pages[0].Resources.XObject.items():
                    if im.get('/Filter') == pikepdf.Name.CCITTFaxDecode:
                        dp = im.get('/DecodeParms') or pikepdf.Dictionary()
                        pil = pikepdf.PdfImage(im).as_pil_image()
                        if flip:
                            x, y = pil.size[0] // 2, pil.size[1] // 2
                            pil.putpixel((x, y), 255 - pil.getpixel((x, y)))
                        data, parms = group4(pil, bool(dp.get('/BlackIs1', False)))
                        im.write(data, filter=pikepdf.Name.CCITTFaxDecode, decode_parms=parms)
                        break
                else:
                    raise SystemExit('the fax PDF needs a CCITT image on its first page')
                if flip:
                    cases.append(('one pixel of the fax image', fax_pdf, save(p, 'fax-pixel.pdf'), True))
                else:
                    cases.append(('fax image coded again (same pixels)', fax_pdf, save(p, 'fax-same.pdf'), False))
    return cases


def main():
    text_pdf, rle_pdf, jpeg_pdf, work = sys.argv[1:5]
    fax_pdf = sys.argv[sys.argv.index('--fax') + 1] if '--fax' in sys.argv else None
    os.makedirs(work, exist_ok=True)
    ok = True
    for what, orig, changed, should_flag in make(text_pdf, rle_pdf, jpeg_pdf, work, fax_pdf):
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
