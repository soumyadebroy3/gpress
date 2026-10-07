#!/usr/bin/env python3
"""Two more renderers, each written by other people, look at every original and its output:

  Poppler (pdftoppm, pdftotext): the engine behind many Linux viewers;
  pdf.js (verify_pdfjs.mjs): the engine of Firefox's PDF viewer.

For each renderer, every page (up to 30) must give the same pixels and the same text, and Poppler must not
report errors for our output that it didn't report for the original. Each file is opened in its own process
with a time limit, so one huge file can't take the whole check down; results are appended as they come and
a rerun continues where it stopped.

  PDFJS_MODULES=/path/to/node_modules verify_render.py OUT     reads OUT/ours.tsv, appends OUT/render.jsonl
Run with python3 -I."""
import hashlib, json, os, re, resource, subprocess, sys, tempfile, threading

PAGES = 30
TIMEOUT = 120  # seconds per file and tool
HERE = os.path.dirname(os.path.abspath(__file__))

def key(path):
    return os.path.basename(os.path.dirname(path)) + '__' + os.path.basename(path)

def limit_memory():
    gb = 3 << 30
    resource.setrlimit(resource.RLIMIT_AS, (gb, gb))

def text_hash(pdf):
    """Hash of pdftotext's output, read as it comes (a text bomb can't fill memory); killed after TIMEOUT."""
    h = hashlib.sha256()
    p = subprocess.Popen(['pdftotext', '-l', str(PAGES), pdf, '-'], stdout=subprocess.PIPE, stderr=subprocess.DEVNULL,
                         preexec_fn=limit_memory)
    timer = threading.Timer(TIMEOUT, p.kill)
    timer.start()
    try:
        while chunk := p.stdout.read(1 << 16):
            h.update(chunk)
        p.wait()
    finally:
        fired = not timer.is_alive()
        timer.cancel()
    return 'timeout' if fired else f'{p.returncode}:{h.hexdigest()}'

def poppler(pdf, td, tag):
    base = os.path.join(td, tag)
    err = os.path.join(td, tag + '.err')
    try:
        with open(err, 'wb') as e:
            rc = subprocess.run(['pdftoppm', '-l', str(PAGES), '-scale-to', '800', '-png', pdf, base], stdout=subprocess.DEVNULL,
                                stderr=e, preexec_fn=limit_memory, timeout=TIMEOUT).returncode
    except subprocess.TimeoutExpired:
        rc = 'timeout'
    pages = []
    for name in sorted(os.listdir(td)):
        if name.startswith(tag + '-') and name.endswith('.png'):
            p = os.path.join(td, name)
            pages.append(hashlib.sha256(open(p, 'rb').read()).hexdigest())
            os.remove(p)
    text = text_hash(pdf)
    with open(err, 'rb') as e:
        lines = e.read(1 << 16).decode('latin-1').splitlines()
    # Error messages without positions and numbers (those legitimately change when a file is rewritten).
    errors = set(re.sub(r'\d+', 'N', l) for l in lines if l.strip())
    return {'rc': rc, 'pages': pages, 'text': text, 'errors': errors}

def pdfjs(pdf):
    try:
        r = subprocess.run(['node', '--max-old-space-size=4096', os.path.join(HERE, 'verify_pdfjs.mjs'), pdf],
                           stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, text=True, timeout=TIMEOUT)
    except subprocess.TimeoutExpired:
        return {'error': 'timeout'}
    if r.returncode != 0 or not r.stdout.strip():
        return {'error': f'process failed ({r.returncode})'}
    return json.loads(r.stdout.strip().splitlines()[-1])

def compare(orig, out):
    problems, notes = [], []
    with tempfile.TemporaryDirectory() as td:
        a, b = poppler(orig, td, 'a'), poppler(out, td, 'b')
    if a['rc'] != b['rc']:
        problems.append(f"poppler exit {a['rc']} vs {b['rc']}")
    elif a['rc'] != 0:
        notes.append(f"poppler: both {a['rc']}")
    if a['pages'] != b['pages']:
        diff = [i + 1 for i, (x, y) in enumerate(zip(a['pages'], b['pages'])) if x != y]
        problems.append(f"poppler pages differ {diff[:5]} (counts {len(a['pages'])} vs {len(b['pages'])})")
    if a['text'] != b['text']:
        problems.append('poppler text differs')
    new = sorted(b['errors'] - a['errors'])
    if new:
        problems.append('poppler new errors: ' + ' | '.join(new)[:300])
    x, y = pdfjs(orig), pdfjs(out)
    if x.get('error') and x.get('error') == y.get('error'):
        notes.append('pdf.js: both ' + x['error'])
    elif x.get('error') != y.get('error'):
        problems.append(f"pdf.js {x.get('error')} vs {y.get('error')}")
    else:
        if x['count'] != y['count']:
            problems.append(f"pdf.js page count {x['count']} vs {y['count']}")
        diff = [i + 1 for i, (p, q) in enumerate(zip(x['pages'], y['pages'])) if p != q]
        if diff:
            problems.append(f'pdf.js pages differ {diff[:5]}')
        if x['text'] != y['text']:
            problems.append('pdf.js text differs')
    return {'file': orig, 'poppler_pages': len(a['pages']), 'pdfjs_pages': len(x.get('pages') or []),
            'problems': problems, 'notes': notes, 'identical': not problems}

def main():
    out = sys.argv[1]
    res_path = os.path.join(out, 'render.jsonl')
    done = set()
    if os.path.exists(res_path):
        done = {json.loads(l)['file'] for l in open(res_path) if l.strip()}
    rows = [l.rstrip('\n').split('\t') for l in open(os.path.join(out, 'ours.tsv')) if l.strip()]
    todo = [r[0] for r in rows if len(r) > 1 and r[1] == 'SMALLER' and r[0] not in done]
    with open(res_path, 'a') as fh:
        for f in todo:
            try:
                r = compare(f, os.path.join(out, 'ours', key(f)))
            except Exception as e:
                r = {'file': f, 'problems': [f'check failed: {type(e).__name__}: {e}'[:200]], 'identical': False}
            fh.write(json.dumps(r) + '\n'); fh.flush()
    res = [json.loads(l) for l in open(res_path) if l.strip()]
    bad = [r for r in res if not r['identical']]
    pp = sum(r.get('poppler_pages', 0) for r in res); jp = sum(r.get('pdfjs_pages', 0) for r in res)
    print(f'{len(res)} checked ({pp} pages in Poppler, {jp} in pdf.js): {len(res) - len(bad)} identical, {len(bad)} flagged')
    for r in bad[:40]:
        print('  ', r['file'], r['problems'])
    notes = [r for r in res if r.get('notes')]
    print(f'{len(notes)} files a renderer failed on in the same way for both (not differences):')
    for r in notes[:20]:
        print('  ', os.path.basename(r['file']), r['notes'])

if __name__ == '__main__':
    main()
