#!/usr/bin/env python3
"""Independent check that an output keeps the original's encryption exactly, using tools we didn't write:

  1. qpdf --show-encryption (empty password): the same text for both files (revision, /P, every permission,
     stream, string and file methods, and the file key, which qpdf derives itself);
  2. pikepdf: both open with the empty user password; the same R, V, P, key length, methods and file key;
     the same /ID; the same /Encrypt dictionary, value by value (its objects followed by value).

Usage: verify_encryption.py ORIGINAL OUTPUT   -> prints one JSON line; exit status 0 if the same.
Run with python3 -I (the files are untrusted)."""
import json, subprocess, sys

import pikepdf


def show_encryption(path):
    """qpdf's reading of the encryption. Warnings about other things (offsets, /Size) are left out: an output
    may fix or move them without changing the encryption."""
    r = subprocess.run(['qpdf', '--show-encryption', '--show-encryption-key', '--password=', path],
                       stdout=subprocess.PIPE, stderr=subprocess.STDOUT, timeout=120)
    lines = [l for l in r.stdout.decode('latin-1').splitlines()
             if not l.startswith('WARNING:') and 'operation succeeded with warnings' not in l]
    return 0 if r.returncode == 3 else r.returncode, lines


def value(o, depth=0):
    """A comparable form of an object of the encryption dictionary (references followed)."""
    if depth > 8:
        return 'too deep'
    if isinstance(o, pikepdf.Dictionary):
        return {str(k): value(v, depth + 1) for k, v in o.items()}
    if isinstance(o, pikepdf.Array):
        return [value(v, depth + 1) for v in o]
    if isinstance(o, pikepdf.String):
        return 'str:' + bytes(o).hex()
    if isinstance(o, pikepdf.Name):
        return 'name:' + str(o)
    return repr(o)


def info(path):
    with pikepdf.open(path) as pdf:
        e = pdf.encryption
        return {
            'encrypted': pdf.is_encrypted,
            'user_password_matched': pdf.user_password_matched,
            'R': e.R, 'V': e.V, 'P': e.P, 'bits': e.bits,
            'stream': str(e.stream_method), 'string': str(e.string_method), 'file': str(e.file_method),
            'key': e.encryption_key.hex(),
            'allow': repr(pdf.allow),
            'id': [bytes(x).hex() for x in pdf.trailer.get('/ID', [])],
            'encrypt': value(pdf.trailer.get('/Encrypt')) if pdf.is_encrypted else None,
        }


def main():
    a, b = sys.argv[1], sys.argv[2]
    problems = []
    sa, sb = show_encryption(a), show_encryption(b)
    if sa != sb:
        problems.append('qpdf --show-encryption differs')
    try:
        ia, ib = info(a), info(b)
        for k in ia:
            if ia[k] != ib[k]:
                problems.append(f'{k} differs')
        if not ia['encrypted']:
            problems.append('original is not encrypted')
    except Exception as e:
        problems.append('pikepdf: ' + type(e).__name__ + ': ' + str(e)[:100])
        ia = {}
    print(json.dumps({'file': a, 'R': ia.get('R'), 'stream': ia.get('stream'), 'problems': problems, 'same': not problems}))
    sys.exit(0 if not problems else 1)


main()
