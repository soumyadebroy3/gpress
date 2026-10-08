#!/usr/bin/env python3
"""Makes the JPEG test files in this folder with libjpeg-turbo's tools (cjpeg, jpegtran, wrjpgcom; Ubuntu package
libjpeg-turbo-progs 2.1.5) and Pillow, from a synthetic picture. X.jpg is the test input; X.ref.jpg is
`jpegtran -optimize -copy all` of it (with the same restart interval and scans), the yardstick the tests compare
our output with.

  TOOLS=/path/to/bin python3 make.py"""
import os, subprocess, tempfile

from PIL import Image

HERE = os.path.dirname(os.path.abspath(__file__))
TOOLS = os.environ.get('TOOLS', '/usr/bin')


def tool(name, *args, data=None):
    r = subprocess.run([os.path.join(TOOLS, name), *args], input=data, capture_output=True, check=True)
    return r.stdout


def picture(w, h, colours=3):
    """Gradients, a few hard edges and some noise, so every kind of symbol occurs."""
    px = bytearray()
    s = 12345
    for y in range(h):
        for x in range(w):
            s = (s * 1103515245 + 12345) & 0x7fffffff
            n = (s >> 16) % 23
            edge = 90 if (x // 9 + y // 7) % 3 == 0 else 0
            r = (x * 255 // max(w - 1, 1) + n + edge) & 255
            g = (y * 255 // max(h - 1, 1) + 2 * n) & 255
            b = ((x + y) * 3 + edge + n) & 255
            px += bytes([r, g, b][:colours] if colours < 3 else [r, g, b])
    if colours == 1:
        return b'P5 %d %d 255\n' % (w, h) + bytes(px)
    return b'P6 %d %d 255\n' % (w, h) + bytes(px)


def main():
    rgb = picture(75, 53)
    gray = picture(61, 37, 1)
    files = {
        'base420': (rgb, ['-quality', '80'], []),
        'base444': (rgb, ['-quality', '80', '-sample', '1x1'], []),
        'base422': (rgb, ['-quality', '80', '-sample', '2x1'], []),
        'base440': (rgb, ['-quality', '80', '-sample', '1x2'], []),
        'gray': (gray, ['-quality', '85'], []),
        'restart1': (rgb, ['-quality', '80', '-restart', '1B'], ['-restart', '1B']),
        'restart5': (rgb, ['-quality', '80', '-restart', '5B'], ['-restart', '5B']),
        'optimized': (rgb, ['-quality', '80', '-optimize'], []),
        'sof1': (rgb, ['-quality', '3'], []),
        'tiny': (picture(1, 1), ['-quality', '90'], []),
        'edge17x9': (picture(17, 9), ['-quality', '95', '-sample', '1x1'], []),
    }
    with tempfile.TemporaryDirectory() as td:
        scripts = {'multiscan': '0;\n1;\n2;\n', 'multiscan2': '0;\n1,2;\n'}
        for name, text in scripts.items():
            path = os.path.join(td, name + '.txt')
            open(path, 'w').write(text)
            files[name] = (rgb, ['-quality', '80', '-scans', path], ['-scans', path])
        for name, (src, opts, ref_opts) in files.items():
            data = tool('cjpeg', *opts, data=src)
            if name == 'base444':
                # An ICC profile (made-up bytes; readers keep them as they are) and a comment.
                icc = os.path.join(td, 'profile.icc')
                open(icc, 'wb').write(bytes(range(256)) * 3)
                data = tool('cjpeg', *opts, '-icc', icc, data=src)
                data = tool('wrjpgcom', '-comment', 'test comment', data=data)
            open(os.path.join(HERE, name + '.jpg'), 'wb').write(data)
            ref = tool('jpegtran', '-optimize', '-copy', 'all', *ref_opts, data=data)
            open(os.path.join(HERE, name + '.ref.jpg'), 'wb').write(ref)
        # Refused kinds.
        open(os.path.join(HERE, 'progressive.jpg'), 'wb').write(tool('cjpeg', '-quality', '80', '-progressive', data=rgb))
        open(os.path.join(HERE, 'arithmetic.jpg'), 'wb').write(tool('cjpeg', '-quality', '80', '-arithmetic', data=rgb))
    # CMYK with an Adobe marker, written by Pillow (its own libjpeg).
    im = Image.frombytes('RGB', (75, 53), rgb[rgb.index(b' 255\n') + 5:]).convert('CMYK')
    path = os.path.join(HERE, 'cmyk.jpg')
    im.save(path, quality=80)
    open(os.path.join(HERE, 'cmyk.ref.jpg'), 'wb').write(tool('jpegtran', '-optimize', '-copy', 'all', path))


if __name__ == '__main__':
    main()
