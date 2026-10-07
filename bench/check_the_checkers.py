#!/usr/bin/env python3
"""The checks must catch small changes. From a PDF with text and a PDF with a RunLength image, this makes
copies that differ in one small way each, and runs verify_pdf.py (qpdf, pikepdf, PDFium) and verify_render.py
(Poppler, pdf.js) on them:

  unchanged copy (rewritten by pikepdf)  -> must pass
  one character of the page content      -> must be flagged
  one value in the document information  -> must be flagged (by the object check; pictures don't show it)
  the page size                          -> must be flagged
  one bit of one pixel of the image      -> must be flagged

  PDFJS_MODULES=... check_the_checkers.py TEXT_PDF RLE_PDF WORKDIR
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


def make(text_pdf, rle_pdf, work):
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
    return cases


def main():
    text_pdf, rle_pdf, work = sys.argv[1:4]
    os.makedirs(work, exist_ok=True)
    ok = True
    for what, orig, changed, should_flag in make(text_pdf, rle_pdf, work):
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
