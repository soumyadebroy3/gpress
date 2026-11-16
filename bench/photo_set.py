#!/usr/bin/env python3
"""The photo set of the size table (bench/effort_table.py): pages that hold one photograph or one scanned page
each, at a known resolution, beside two files of the corpus that hold photographs.

    python3 -I bench/photo_set.py [--corpus DIR]

Writes DIR/photos/*.pdf (DIR is bench/corpus unless given; git ignores it) and the list bench/sets/photos.txt:

  pdfjs/issue19971.pdf, pdfjs/issue14200.pdf    two files of the corpus, as they are

  astronaut, coffee, hubble    three photographs of scikit-image's sample data, fetched with curl and checked
      against a SHA-256 (FETCHED below, with the licence its skimage/data/_fetchers.py states for each). Each
      is laid on a Letter page at 150, 300 and 600 pixels per inch, once as samples behind Flate and once as a
      JPEG: the fetched bytes of the one that is a JPEG, and Pillow's JPEG at quality 90 of the two PNG files.

  room    object 5 of pdfjs/issue12841_reduced.pdf: a camera photograph of 5280 by 3792 pixels, its JPEG as the
      corpus file stores it, laid 7 inches wide on a Letter page (754 pixels per inch).

  scan    object 6 of ocrmypdf/tests__resources__poster.pdf: a scanned page of text, 2550 by 3300 pixels, over
      the whole of a Letter page (300 pixels per inch), as its JPEG and as samples behind Flate.

The corpus is untrusted: this process is held to 3 GB of address space, and nothing in a file is run.
"""
import argparse
import hashlib
import io
import os
import resource
import subprocess
import sys
import zlib

import pikepdf
from pikepdf import Name
from PIL import Image

HERE = os.path.dirname(os.path.abspath(__file__))
BASE = 'https://raw.githubusercontent.com/scikit-image/scikit-image/v0.24.0/skimage/data/'
FETCHED = [  # name, file, SHA-256, the licence as skimage/data/_fetchers.py of that release states it
    ('astronaut', 'astronaut.png', '88431cd9653ccd539741b555fb0a46b61558b301d4110412b5bc28b5e3ea6cb5',
     'No known copyright restrictions, released into the public domain. (NASA)'),
    ('coffee', 'coffee.png', 'cc02f8ca188b167c775a7101b5d767d1e71792cf762c33d6fa15a4599b5a8de7',
     'No copyright restrictions. CC0 by the photographer (Rachel Michetti).'),
    ('hubble', 'hubble_deep_field.jpg', '3a19c5dd8a927a9334bb1229a6d63711b1c0c767fb27e2286e7c84a3e2c2f5f4',
     'The image was captured by NASA and may be freely used in the public domain.'),
]
AS_THEY_ARE = ['pdfjs/issue19971.pdf', 'pdfjs/issue14200.pdf']
FROM_CORPUS = [  # name, corpus file, object number, pixels per inch, over the whole page, also behind Flate
    ('room', 'pdfjs/issue12841_reduced.pdf', 5, 754, False, False),
    ('scan', 'ocrmypdf/tests__resources__poster.pdf', 6, 300, True, True),
]
LETTER = (612, 792)


def page(path, size, ppi, data, filt, whole=False):
    """A one-page PDF: the RGB image `data` (`size` pixels, behind `filt`) drawn at `ppi`, on a Letter page with
    its top left corner 3/4 inch in, or (`whole`) over the whole of a page of its own size."""
    w, h = size[0] * 72 / ppi, size[1] * 72 / ppi
    box = (w, h) if whole else LETTER
    x, y = (0, 0) if whole else (54, LETTER[1] - 54 - h)
    pdf = pikepdf.new()
    image = pdf.make_stream(b'', Type=Name.XObject, Subtype=Name.Image, Width=size[0], Height=size[1],
                            ColorSpace=Name.DeviceRGB, BitsPerComponent=8)
    image.write(data, filter=filt)
    pdf.add_blank_page(page_size=box)
    pdf.pages[0].Resources = pikepdf.Dictionary(XObject=pikepdf.Dictionary(Im0=image))
    pdf.pages[0].Contents = pdf.make_stream(f'q {w:.2f} 0 0 {h:.2f} {x:.2f} {y:.2f} cm /Im0 Do Q'.encode())
    pdf.save(path, deterministic_id=True)


def flate(picture):
    return zlib.compress(picture.convert('RGB').tobytes(), 6)


def main():
    ap = argparse.ArgumentParser(description=__doc__.split('\n\n')[0])
    ap.add_argument('--corpus', default=os.path.join(HERE, 'corpus'), help='root of the test files')
    args = ap.parse_args()
    resource.setrlimit(resource.RLIMIT_AS, (3_000_000_000, 3_000_000_000))
    out = os.path.join(args.corpus, 'photos')
    os.makedirs(out, exist_ok=True)
    made = []

    def write(name, *what, **how):
        page(os.path.join(out, name), *what, **how)
        made.append('photos/' + name)

    for name, file, sha, _ in FETCHED:
        data = subprocess.run(['curl', '-fsSL', '--max-time', '120', BASE + file], capture_output=True, check=True).stdout
        if hashlib.sha256(data).hexdigest() != sha:
            sys.exit(f'{file}: not the bytes this script was written for')
        picture = Image.open(io.BytesIO(data)).convert('RGB')
        jpeg = data
        if not file.endswith('.jpg'):
            buffer = io.BytesIO()
            picture.save(buffer, 'JPEG', quality=90)
            jpeg = buffer.getvalue()
        for ppi in (150, 300, 600):
            write(f'{name}-{ppi}-flate.pdf', picture.size, ppi, flate(picture), Name.FlateDecode)
            write(f'{name}-{ppi}-jpeg.pdf', picture.size, ppi, jpeg, Name.DCTDecode)
    for name, file, number, ppi, whole, samples in FROM_CORPUS:
        with pikepdf.open(os.path.join(args.corpus, file)) as pdf:
            o = pdf.get_object((number, 0))
            assert o.Filter == Name.DCTDecode and o.ColorSpace == Name.DeviceRGB, file
            size, jpeg = (int(o.Width), int(o.Height)), o.read_raw_bytes()
        write(f'{name}-{ppi}-jpeg.pdf', size, ppi, jpeg, Name.DCTDecode, whole=whole)
        if samples:
            write(f'{name}-{ppi}-flate.pdf', size, ppi, flate(Image.open(io.BytesIO(jpeg))), Name.FlateDecode, whole=whole)
    with open(os.path.join(HERE, 'sets', 'photos.txt'), 'w') as f:
        f.write('# The photo set: python3 -I bench/photo_set.py makes the files under photos/.\n')
        f.write('\n'.join(AS_THEY_ARE + made) + '\n')
    print(f'{len(made)} pages under {out}, {len(AS_THEY_ARE)} files of the corpus beside them: bench/sets/photos.txt')


if __name__ == '__main__':
    sys.exit(main())
