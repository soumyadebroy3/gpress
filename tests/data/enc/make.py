#!/usr/bin/env python3
"""Makes the encrypted test files in this folder. Run once by hand (python3 make.py); the tests use the files
as they are. Needs qpdf 11 and the Python package 'cryptography'.

- base.pdf: a small one-page document (text, a font, an image, a note, document information, XMP metadata).
- qpdf encrypts it with the empty user password in every standard mode (r2_rc4_40 ... r6_objstm), and once
  with a user password (needs_password.pdf). keys.txt holds the file key qpdf derives for each.
  r4_aes_cleartext_md_plain has its streams stored decoded, so that a rewrite saves enough to put each object in
  an object stream of its own (qpdf keeps the catalog out of object streams).
- Hand-made files for cases readers disagree on, which the engine must leave as they are. They are built here
  with RC4, MD5 and AES code independent of the engine's.
"""
import hashlib, os, random, subprocess, zlib

from cryptography.hazmat.primitives.ciphers import Cipher, algorithms, modes

HERE = os.path.dirname(os.path.abspath(__file__))
PAD = bytes.fromhex('28bf4e5e4e758a4164004e56fffa01082e2e00b6d0683e802f0ca9fe6453697a')


def write_pdf(path, objs, trailer):
    """objs: {number: body bytes}; a classic cross-reference table."""
    d = bytearray(b'%PDF-1.6\n%\xe2\xe3\xcf\xd3\n')
    offs = {}
    for k in sorted(objs):
        offs[k] = len(d)
        d += b'%d 0 obj\n' % k + objs[k] + b'\nendobj\n'
    size = max(objs) + 1
    x = len(d)
    d += b'xref\n0 %d\n0000000000 65535 f \n' % size
    for k in range(1, size):
        d += b'%010d 00000 n \n' % offs[k] if k in offs else b'0000000000 00000 f \n'
    d += b'trailer\n<< /Size %d ' % size + trailer + b' >>\nstartxref\n%d\n%%%%EOF\n' % x
    open(path, 'wb').write(bytes(d))


def stream(dict_body, data):
    return b'<< ' + dict_body + b' /Length %d >>\nstream\n' % len(data) + data + b'\nendstream'


XMP = (b'<?xpacket begin="" id="W5M0MpCehiHzreSzNTczkc9d"?>\n<x:xmpmeta xmlns:x="adobe:ns:meta/">\n'
       b'<rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">\n'
       b'<rdf:Description rdf:about="" xmlns:dc="http://purl.org/dc/elements/1.1/">\n'
       b'<dc:title><rdf:Alt><rdf:li xml:lang="x-default">Encrypted test</rdf:li></rdf:Alt></dc:title>\n'
       b'<dc:creator><rdf:Seq><rdf:li>peyaji</rdf:li></rdf:Seq></dc:creator>\n'
       b'</rdf:Description>\n</rdf:RDF>\n</x:xmpmeta>\n' + b' ' * 400 + b'\n<?xpacket end="w"?>')


def base():
    lines = [b'BT /F1 11 Tf 72 %d Td (Line %d: the quick brown fox jumps over the lazy dog.) Tj ET' % (760 - 16 * i, i)
             for i in range(40)]
    content = b'\n'.join(lines) + b'\nq 64 0 0 64 440 680 cm /Im1 Do Q\n'
    img = bytes((x * 16, y * 16, (x + y) * 8) [c] for y in range(16) for x in range(16) for c in range(3))
    objs = {
        1: b'<< /Type /Catalog /Pages 2 0 R /Metadata 7 0 R >>',
        2: b'<< /Type /Pages /Kids [3 0 R] /Count 1 >>',
        3: b'<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R /Annots [8 0 R]'
           b' /Resources << /Font << /F1 5 0 R >> /XObject << /Im1 6 0 R >> >> >>',
        4: stream(b'', content),
        5: b'<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>',
        6: stream(b'/Type /XObject /Subtype /Image /Width 16 /Height 16 /ColorSpace /DeviceRGB /BitsPerComponent 8', img),
        7: stream(b'/Type /Metadata /Subtype /XML', XMP),
        8: b'<< /Type /Annot /Subtype /Text /Rect [72 72 92 92] /Contents (A note \\(with parentheses\\)) >>',
        9: b'<< /Title (Encrypted test) /Author (peyaji) /Producer (make.py) >>',
    }
    raw = os.path.join(HERE, 'base.raw.pdf')
    write_pdf(raw, objs, b'/Root 1 0 R /Info 9 0 R /ID [<00112233445566778899aabbccddeeff> <00112233445566778899aabbccddeeff>]')
    subprocess.run(['qpdf', '--deterministic-id', '--compress-streams=y', '--object-streams=disable', raw,
                    os.path.join(HERE, 'base.pdf')], check=True)
    os.remove(raw)


QPDF = {
    'r2_rc4_40': ['--allow-weak-crypto', '--encrypt', '', 'owner', '40', '--'],
    'r3_rc4_128': ['--allow-weak-crypto', '--encrypt', '', 'owner', '128', '--use-aes=n', '--'],
    'r4_rc4_128': ['--allow-weak-crypto', '--encrypt', '', 'owner', '128', '--use-aes=n', '--force-V4', '--'],
    'r4_aes_128': ['--encrypt', '', 'owner', '128', '--use-aes=y', '--'],
    'r4_aes_cleartext_md': ['--encrypt', '', 'owner', '128', '--use-aes=y', '--cleartext-metadata', '--'],
    'r4_aes_cleartext_md_plain': ['--decode-level=generalized', '--compress-streams=n', '--encrypt', '', 'owner', '128',
                                  '--use-aes=y', '--cleartext-metadata', '--'],
    'r5_aes_256': ['--encrypt', '', 'owner', '256', '--force-R5', '--'],
    'r6_aes_256': ['--encrypt', '', 'owner', '256', '--'],
    'r6_restricted': ['--encrypt', '', 'owner', '256', '--print=none', '--modify=none', '--extract=n', '--'],
    'r6_objstm': ['--object-streams=generate', '--encrypt', '', 'owner', '256', '--'],
    'needs_password': ['--encrypt', 'user', 'owner', '256', '--'],
}


def encrypt_with_qpdf():
    keys = []
    for name, args in QPDF.items():
        out = os.path.join(HERE, name + '.pdf')
        subprocess.run(['qpdf'] + args + [os.path.join(HERE, 'base.pdf'), out], check=True)
        if name != 'needs_password':
            r = subprocess.run(['qpdf', '--show-encryption', '--show-encryption-key', '--password=', out],
                               stdout=subprocess.PIPE, check=True).stdout.decode()
            key = [l.split('=', 1)[1].strip() for l in r.splitlines() if l.startswith('Encryption key =')][0]
            keys.append(f'{name} {key}\n')
    open(os.path.join(HERE, 'keys.txt'), 'w').write(''.join(keys))


# ---------------------------------------------------------------- hand-made files (RC4 and AES of our own here)

def rc4(key, data):
    s = list(range(256))
    j = 0
    for i in range(256):
        j = (j + s[i] + key[i % len(key)]) & 255
        s[i], s[j] = s[j], s[i]
    i = j = 0
    out = bytearray()
    for b in data:
        i = (i + 1) & 255
        j = (j + s[i]) & 255
        s[i], s[j] = s[j], s[i]
        out.append(b ^ s[(s[i] + s[j]) & 255])
    return bytes(out)


def md5(*parts):
    h = hashlib.md5()
    for p in parts:
        h.update(p)
    return h.digest()


def aes_cbc(key, iv, data):
    pad = 16 - len(data) % 16
    e = Cipher(algorithms.AES(key), modes.CBC(iv)).encryptor()
    return iv + e.update(data + bytes([pad]) * pad) + e.finalize()


ID0 = bytes.fromhex('0123456789abcdef0123456789abcdef')


def handler(r, n, p):
    """/O, /U and the file key for the empty user password and owner password 'owner' (revisions 2 to 4)."""
    h = md5((b'owner' + PAD)[:32])
    if r >= 3:
        for _ in range(50):
            h = md5(h[:n])
    ok = h[:n]
    o = rc4(ok, PAD)
    if r >= 3:
        for i in range(1, 20):
            o = rc4(bytes(b ^ i for b in ok), o)
    h = md5(PAD, o, (p & 0xffffffff).to_bytes(4, 'little'), ID0)
    if r >= 3:
        for _ in range(50):
            h = md5(h[:n])
    fk = h[:n]
    if r == 2:
        u = rc4(fk, PAD)
    else:
        u = rc4(fk, md5(PAD, ID0))
        for i in range(1, 20):
            u = rc4(bytes(b ^ i for b in fk), u)
        u += bytes(16)
    return o, u, fk


def objkey(fk, num, gen, aes=False):
    return md5(fk, num.to_bytes(4, 'little')[:3], gen.to_bytes(2, 'little'), b'sAlT' if aes else b'')[:min(len(fk) + 5, 16)]


CONTENT = b'BT /F1 24 Tf 72 700 Td (Hello encrypted world) Tj ET\n' * 8


def hand_made(name, enc_body, r, n, p, seal_stream, seal_string, title=None, content=CONTENT):
    """A one-page file: content stream 4, info 6 (its /Title literal or hex), encryption dictionary 7."""
    c4 = seal_stream(4, content)
    if title is None:
        title = b'<' + seal_string(6, b'A title').hex().encode() + b'>'
    objs = {
        1: b'<< /Type /Catalog /Pages 2 0 R >>',
        2: b'<< /Type /Pages /Kids [3 0 R] /Count 1 >>',
        3: b'<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R /Resources << /Font << /F1 5 0 R >> >> >>',
        4: stream(b'', c4),
        5: b'<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>',
        6: b'<< /Title ' + title + b' /Producer (make.py) >>',
        7: enc_body,
    }
    write_pdf(os.path.join(HERE, name + '.pdf'), objs,
              b'/Root 1 0 R /Info 6 0 R /Encrypt 7 0 R /ID [<' + ID0.hex().encode() + b'> <' + ID0.hex().encode() + b'>]')


def raw_cr_title(fk, crlf):
    """A literal string whose RC4 ciphertext (object 6) holds a raw CR (or CR LF) and nothing else that needs
    escaping: readers that keep the CR and readers that read it as LF decrypt different titles."""
    k6 = objkey(fk, 6, 0)
    for i in range(1000000):
        rnd = random.Random(i)
        pt = bytes(rnd.choice(b'abcdefghijklmnopqrstuvwxyz ') for _ in range(24))
        ct = rc4(k6, pt)
        want = (b'\r\n' in ct) if crlf else (b'\r' in ct and b'\r\n' not in ct)
        bad = [c for c in b'()\\\n' if not (crlf and c == 10)]
        if want and not any(c in ct for c in bad) and ct.count(b'\r') == 1:
            return b'(' + ct + b')'
    raise SystemExit('no title found')


def make_hand_made():
    # RC4-40 (V1 R2) with a raw CR, or CR LF, inside an encrypted literal string.
    o, u, fk = handler(2, 5, -44)
    enc = b'<< /Filter /Standard /V 1 /R 2 /Length 40 /P -44 /O <%s> /U <%s> >>' % (o.hex().encode(), u.hex().encode())
    rc = lambda num, data: rc4(objkey(fk, num, 0), data)
    hand_made('raw_cr', enc, 2, 5, -44, rc, rc, raw_cr_title(fk, False))
    hand_made('raw_crlf', enc, 2, 5, -44, rc, rc, raw_cr_title(fk, True))
    hand_made('rc4_40_plain_title', enc, 2, 5, -44, rc, rc)
    # V4 R4 variants.
    o, u, fk = handler(4, 16, -1028)
    head = b'/Filter /Standard /V 4 /R 4 /Length 128 /P -1028 /O <%s> /U <%s>' % (o.hex().encode(), u.hex().encode())
    rc = lambda num, data: rc4(objkey(fk, num, 0), data)
    aes = lambda num, data: aes_cbc(objkey(fk, num, 0, True), bytes(range(num, num + 16)), data)
    plain = lambda num, data: data
    hand_made('identity_cf', b'<< ' + head + b' /CF << /Identity << /CFM /V2 /Length 16 /AuthEvent /DocOpen >> >>'
              b' /StmF /Identity /StrF /Identity >>', 4, 16, -1028, rc, rc)
    hand_made('cfm_none', b'<< ' + head + b' /CF << /StdCF << /CFM /None /AuthEvent /DocOpen /Length 16 >> >>'
              b' /StmF /StdCF /StrF /StdCF >>', 4, 16, -1028, plain, plain)
    hand_made('mixed_filters', b'<< ' + head + b' /CF << /StdCF << /CFM /AESV2 /AuthEvent /DocOpen /Length 16 >> >>'
              b' /StmF /StdCF /StrF /Identity >>', 4, 16, -1028, aes, plain)
    hand_made('v4_rc4_ok', b'<< ' + head + b' /CF << /StdCF << /CFM /V2 /AuthEvent /DocOpen /Length 16 >> >>'
              b' /StmF /StdCF /StrF /StdCF >>', 4, 16, -1028, rc, rc)
    # No key length anywhere: Poppler takes 40 bits.
    head_nolen = head.replace(b' /Length 128', b'')
    hand_made('v4_aes_no_length', b'<< ' + head_nolen + b' /CF << /StdCF << /CFM /AESV2 /AuthEvent /DocOpen >> >>'
              b' /StmF /StdCF /StrF /StdCF >>', 4, 16, -1028, aes, aes)
    # One tiny AES stream: decrypting it can't pay for the key and its initialization vector in a pack.
    hand_made('aes_tiny_stream', b'<< ' + head + b' /CF << /StdCF << /CFM /AESV2 /AuthEvent /DocOpen /Length 16 >> >>'
              b' /StmF /StdCF /StrF /StdCF >>', 4, 16, -1028, aes, aes, content=b'0 g\n')
    signed()


def signed():
    """RC4-40 with a signature field whose value (/Contents) is stored unencrypted, as signing tools do."""
    o, u, fk = handler(2, 5, -44)
    rc = lambda num, data: rc4(objkey(fk, num, 0), data)
    c4 = rc(4, CONTENT)
    objs = {
        1: b'<< /Type /Catalog /Pages 2 0 R /AcroForm << /Fields [8 0 R] /SigFlags 3 >> >>',
        2: b'<< /Type /Pages /Kids [3 0 R] /Count 1 >>',
        3: b'<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R /Annots [8 0 R]'
           b' /Resources << /Font << /F1 5 0 R >> >> >>',
        4: stream(b'', c4),
        5: b'<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>',
        7: b'<< /Filter /Standard /V 1 /R 2 /Length 40 /P -44 /O <%s> /U <%s> >>' % (o.hex().encode(), u.hex().encode()),
        8: b'<< /FT /Sig /Type /Annot /Subtype /Widget /Rect [0 0 0 0] /P 3 0 R /T <' + rc(8, b'Signature1').hex().encode()
           + b'> /V 9 0 R >>',
        9: b'<< /Type /Sig /Filter /Adobe.PPKLite /SubFilter /adbe.pkcs7.detached /ByteRange [0 0 0 0] /Contents <'
           + b'30' * 64 + b'> >>',
    }
    write_pdf(os.path.join(HERE, 'signed_rc4.pdf'), objs,
              b'/Root 1 0 R /Encrypt 7 0 R /ID [<' + ID0.hex().encode() + b'> <' + ID0.hex().encode() + b'>]')


def check_keys_with_python():
    """The file keys of the RC4-40 and V4 hand-made files, as derived above (a second, independent check)."""
    for name, (r, n, p) in {'rc4_40_plain_title': (2, 5, -44), 'v4_rc4_ok': (4, 16, -1028)}.items():
        fk = handler(r, n, p)[2]
        q = subprocess.run(['qpdf', '--show-encryption', '--show-encryption-key', '--password=', os.path.join(HERE, name + '.pdf')],
                           stdout=subprocess.PIPE).stdout.decode()
        assert fk.hex() in q, (name, q)


if __name__ == '__main__':
    base()
    encrypt_with_qpdf()
    make_hand_made()
    check_keys_with_python()
    print('ok')
