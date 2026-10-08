#!/usr/bin/env python3
"""Security benchmark: hostile input must never lose data, never crash, and stay inside a time and memory
bound. Pass or fail, with every failing input saved so it can be replayed. Part 3 of the benchmark suite
(benchmark.py time / space are parts 1 and 2; `benchmark.py security ...` runs this file).

  security.py archives  [RUN] [options]   20 packs, damaged every way; unpack must error or restore, never a wrong file
  security.py pdfs      [RUN] [options]   hostile PDFs through optimize and pack: untouched, smaller or a clean error, bounded
  security.py fuzz      [RUN] [options]   mutation fuzzing of optimize, pack and unpack (--fuzz-seconds per entry point)
  security.py supply    [RUN] [options]   cargo audit, the licence of every dependency, unsafe blocks and their reasons
  security.py selfcheck [RUN] [options]   plant a wrong file, a crash, a hang, a memory blow-up, a bad licence, a bare unsafe
  security.py charness  [RUN] [options]   archives and pdfs through the C library (skips when crates/gpress-c is absent)
  security.py run       [RUN] [options]   all of the above, then the report (--fuzz-seconds 0 leaves the fuzzing out)
  security.py report    [RUN] [options]   the Security section of bench/RESULTS.md from the results of RUN

RUN is a directory (made under bench/results/ when the name has no slash; default "security"). Its small
result files (*.tsv, supply.json, meta.json) are kept in the repository; RUN/work/ (generated inputs) and
RUN/failures/ (every failing input with the engine's stderr next to it) are not. RUN/notes.md, when it
exists, is written by whoever looked at the failures and goes into the report as it is.

The engine is $GPRESS or $PDFE (default target/release/gpress). Every bound, path and size comes from
DEFAULTS below, a JSON file (--config) and the options. Run with python3 -I: the test files are untrusted.

How a run is measured: each command is its own process, started by bench/measure.c (a parent under 1 MB, so
the peak resident set it reports is the command's own; without a C compiler the harness forks from Python
and says so in meta.json) under `prlimit` for the address-space and CPU-time limits. Time bounds apply to
the child's CPU time (user + system), which does not depend on what else the machine is doing; a run past
the hang limit in wall time is killed and counted as a hang. The engine runs single-threaded.

The bounds (also in the report): a damaged archive is refused in under 5 s with a peak under 512 MiB; a
hostile PDF returns or errors in under 30 s plus 5 s per MiB of input, with a peak under 512 MiB or 16 bytes
per input byte, whichever is more (time and memory may grow with the input, never with what the input
claims); the fuzzer runs with a hard 512 MiB address space and 60 s, so an allocation failure there means
the engine asked for more than that on a small file."""
import argparse, atexit, hashlib, json, os, random, re, shutil, signal, subprocess, sys, tempfile, time, zlib

HERE = os.path.dirname(os.path.abspath(__file__))
REPO = os.path.dirname(HERE)
SCRATCH = '/tmp/claude-0/-home-claude/b636d578-f2d3-5309-87d4-a4766d45bbac/scratchpad'
MIB = 1 << 20

DEFAULTS = {
    'engine': os.environ.get('GPRESS') or os.environ.get('PDFE') or os.path.join(REPO, 'target', 'release', 'gpress'),
    'corpus': os.environ.get('CORPUS') or os.path.join(SCRATCH, 'corpus'),
    'results': os.path.join(HERE, 'results'),
    'run': 'security',
    # The corpus list: a path, or None for bench/sets/all.txt (relative to the corpus) and then the
    # scratchpad list of the first measurements.
    'files': None,
    'sets': os.path.join(HERE, 'sets'),
    'effort': 0,  # the quick settings: legitimate work stays short, so a long run is the input's doing
    'threads': 1,
    'time_metric': 'cpu',  # cpu or wall: which time the bounds apply to
    'hang': 60.0,  # a run past this many seconds of wall time is killed and is a hang ...
    'hang_factor': 2.0,  # ... or past this multiple of its time bound, whichever is more
    'as_ceiling_mib': 1024,  # address space while measuring: enough to see a real peak, stops a runaway
    'archive': {
        'packs': 20, 'max_seed_bytes': 3_000_000,
        'time': 5.0, 'mem_mib': 512,
        'flips': 12, 'flip_bits': [0, 3, 7], 'cuts': 16, 'garbage': 3000, 'rng_seed': 12345,
    },
    'pdf': {
        'time': 30.0, 'time_per_mib': 5.0, 'mem_mib': 512, 'mem_per_byte': 16,
        'nesting': 100_000, 'bomb_mib': 1024, 'nested_bomb_mib': 256, 'image_side': 100_000,
        'objects': 1_000_000, 'runlength_mib': 2,
    },
    'fuzz': {
        'seconds': 600, 'mem_mib': 512, 'seeds': 40, 'max_seed_bytes': 1_500_000, 'archive_seeds': 12,
        'seed_mem_fraction': 0.5,  # a seed is used only when its clean run peaks under this share of the limit
        'edits': 8, 'rng_seed': 0xC0FFEE, 'pick_seed': 7,
    },
    'supply': {
        'features': 'existing',  # the features the measured build has
        'src': 'src',  # where our own code is (unsafe blocks are counted there)
        # The owner's rule: MIT/Apache-2.0/BSD/ISC/Zlib/Unicode/MPL-2.0; never GPL, AGPL or LGPL. A dual
        # "A OR B" passes when either side is on this list; "A AND B" needs both.
        'allowed': ['MIT', 'Apache-2.0', 'Apache-2.0 WITH LLVM-exception', 'BSD-2-Clause', 'BSD-3-Clause',
                    '0BSD', 'ISC', 'Zlib', 'Unicode-3.0', 'Unicode-DFS-2016', 'MPL-2.0'],
    },
    'charness': {'crate': 'crates/gpress-c', 'features': 'existing'},
}


def deep_update(base, over):
    for k, v in over.items():
        if isinstance(v, dict) and isinstance(base.get(k), dict):
            deep_update(base[k], v)
        else:
            base[k] = v
    return base


def say(*a):
    print(*a, flush=True)


# ----------------------------------------------------------------------------------------------------
# Running one process and reading its cost

class Launcher:
    """bench/measure.c compiled once per run, and prlimit for the limits. Linux gives a new process the peak
    memory of the process that forked it, so forking from this Python process would put its own resident
    set under every reading; measure is under 1 MB."""
    path = None
    prlimit = shutil.which('prlimit')
    note = 'wait4 from the Python process: peak memory readings have a floor of about 20 MB'
    tmp = None

    @classmethod
    def build(cls):
        cls.tmp = tempfile.mkdtemp(prefix='security-')
        atexit.register(shutil.rmtree, cls.tmp, True)
        exe = os.path.join(cls.tmp, 'measure')
        try:
            r = subprocess.run(['cc', '-O2', '-o', exe, os.path.join(HERE, 'measure.c')], capture_output=True, timeout=120)
            if r.returncode == 0:
                cls.path, cls.note = exe, 'bench/measure.c (parent under 1 MB)'
        except (OSError, subprocess.TimeoutExpired):
            pass
        cls.note += '; address-space limit by ' + ('prlimit' if cls.prlimit else 'the shell (ulimit)')
        return cls.path is not None


def spawn(cmd, timeout, as_limit=None, cwd=None):
    """Run cmd as its own process and wait for it. Returns how it ended: rc (exit code), signal (0 or the
    number that killed it), wall and cpu seconds, maxrss in bytes, timed_out, and its stdout and stderr.
    as_limit caps the address space in bytes; the CPU time is always capped a little past the timeout."""
    if Launcher.tmp is None:
        Launcher.build()
    cmd = [str(c) for c in cmd]
    cpu = int(timeout) + 5
    if Launcher.prlimit:
        wrap = [Launcher.prlimit, f'--cpu={cpu}:{cpu + 2}'] + ([f'--as={as_limit}'] if as_limit else [])
    else:
        script = 'ulimit -t "$1" && { [ "$2" = 0 ] || ulimit -v "$2"; } && shift 2 && exec "$@"'
        wrap = ['sh', '-c', script, 'sh', str(cpu), str((as_limit or 0) // 1024)]
    if Launcher.path is None:
        return _spawn_fork(wrap + cmd, timeout, cwd)
    result = os.path.join(Launcher.tmp, 'result.txt')
    out_path = os.path.join(Launcher.tmp, 'stdout.bin')
    args = [Launcher.path, '-r', result, '-o', out_path, '-t', str(int(timeout + 0.999)), '--'] + wrap + cmd
    p = subprocess.Popen(args, stdin=subprocess.DEVNULL, stdout=subprocess.PIPE, stderr=subprocess.PIPE, cwd=cwd, start_new_session=True)
    try:
        _, err = p.communicate(timeout=timeout + 60)
    except subprocess.TimeoutExpired:  # the launcher itself is stuck: kill its whole group
        os.killpg(p.pid, signal.SIGKILL)
        _, err = p.communicate()
        return _ended(-1, 9, timeout, 0.0, 0, True, b'', err)
    try:
        with open(result) as f:
            rc, wall, user, system, maxrss_kb, timed_out = f.read().split()
        os.unlink(result)
    except (OSError, ValueError):
        return _ended(p.returncode or 2, 0, 0.0, 0.0, 0, False, b'', err + b'\nmeasure: no result')
    stdout = _read_and_rm(out_path)
    rc = int(rc)
    sig = -rc if rc < 0 else 0
    return _ended(rc if rc >= 0 else -1, sig, float(wall), float(user) + float(system), int(maxrss_kb) * 1024, timed_out == '1', stdout, err)


def _ended(rc, sig, wall, cpu, maxrss, timed_out, stdout, stderr):
    return {'rc': rc, 'signal': sig, 'wall': wall, 'cpu': cpu, 'maxrss': maxrss, 'timed_out': timed_out,
            'stdout': stdout, 'stderr': stderr}


def _spawn_fork(argv, timeout, cwd):
    """Without a C compiler: fork from here (the memory floor is then this process's resident set)."""
    out_fd, out_path = tempfile.mkstemp(prefix='sec-out-')
    err_fd, err_path = tempfile.mkstemp(prefix='sec-err-')
    t0 = time.monotonic()
    pid = os.fork()
    if pid == 0:
        try:
            os.setsid()
            devnull = os.open(os.devnull, os.O_RDONLY)
            os.dup2(devnull, 0)
            os.dup2(out_fd, 1)
            os.dup2(err_fd, 2)
            if cwd:
                os.chdir(cwd)
            os.execvp(argv[0], argv)
        except BaseException:
            os._exit(127)
    os.close(out_fd)
    os.close(err_fd)
    timed_out = False
    while True:
        wpid, status, ru = os.wait4(pid, os.WNOHANG)
        if wpid == pid:
            break
        if time.monotonic() - t0 > timeout:
            timed_out = True
            try:
                os.killpg(pid, signal.SIGKILL)
            except ProcessLookupError:
                pass
            _, status, ru = os.wait4(pid, 0)
            break
        time.sleep(0.01)
    wall = time.monotonic() - t0
    sig = os.WTERMSIG(status) if os.WIFSIGNALED(status) else 0
    rc = os.WEXITSTATUS(status) if os.WIFEXITED(status) else -1
    return _ended(rc, sig, wall, ru.ru_utime + ru.ru_stime, ru.ru_maxrss * 1024, timed_out,
                  _read_and_rm(out_path), _read_and_rm(err_path))


def _read_and_rm(path):
    try:
        with open(path, 'rb') as f:
            return f.read()
    except OSError:
        return b''
    finally:
        try:
            os.unlink(path)
        except OSError:
            pass


def classify(r, cfg, time_bound, mem_bound):
    """A short verdict for a finished run, and whether it is a failure. Failures: a hang, a crash, an
    allocation failure, an uncaught panic, or a run over its time or memory bound."""
    if r['timed_out']:
        return 'hang', True
    if r['signal']:
        try:
            name = signal.Signals(r['signal']).name
        except ValueError:
            name = str(r['signal'])
        if name == 'SIGXCPU':
            return 'hang:cpu', True
        if name == 'SIGABRT' and b'memory allocation of' in r['stderr']:
            return 'oom-abort', True  # Rust's allocator could not get memory under the address-space limit
        return 'crash:' + name, True
    if r['rc'] == 101:
        return 'panic', True  # an uncaught Rust panic reached the top of the process
    if r['rc'] == 4:
        return 'panic-caught', True  # the library's guard held (Error::Internal, exit 4), but a panic is a bug
    if r['rc'] == 127:
        return 'exec-failed', True
    over = []
    if r['maxrss'] > mem_bound:
        over.append(f"mem {r['maxrss'] / MIB:.0f}MiB>{mem_bound / MIB:.0f}")
    t = r[cfg['time_metric']]
    if t > time_bound:
        over.append(f"time {t:.1f}s>{time_bound:.0f}")
    if over:
        return 'over-bound:' + '+'.join(over), True
    return ('ok' if r['rc'] == 0 else 'refused'), False


def pdf_bounds(cfg, nbytes):
    """Time and memory a hostile PDF may take: a base plus a share per byte of input."""
    p = cfg['pdf']
    return p['time'] + p['time_per_mib'] * nbytes / MIB, max(p['mem_mib'] * MIB, p['mem_per_byte'] * nbytes)


def hang_after(cfg, time_bound):
    return max(cfg['hang'], cfg['hang_factor'] * time_bound)


# ----------------------------------------------------------------------------------------------------
# The engine under test: the command-line tool, or the C harness with the same three operations

class Tool:
    """argv for optimize / pack / unpack. `kind` is 'binary' (pdfe or gpress: `CMD IN OUT [--effort N
    --threads N]`) or 'charness' (bench/gpress_harness.c: optimize / compress / expand)."""

    def __init__(self, path, cfg, kind='binary'):
        self.path, self.kind = os.path.abspath(path), kind
        self.effort, self.threads = cfg['effort'], cfg['threads']

    def argv(self, op, inp, out):
        if self.kind == 'binary':
            args = [self.path, op, inp, out]
            if op != 'unpack':
                args += ['--effort', self.effort, '--threads', self.threads]
            return args
        name = {'optimize': 'optimize', 'pack': 'compress', 'unpack': 'expand'}[op]
        return [self.path, name, inp, out, '--effort', self.effort, '--threads', self.threads]

    def special(self, r):
        """A verdict the tool's own exit codes carry, or None. Both tools exit 4 for a panic the library's guard
        caught (`classify` counts it); the harness also exits 5 when a round trip restored different bytes."""
        if self.kind == 'charness' and r['rc'] == 5:
            return 'NOT-EXACT', True
        return None


# ----------------------------------------------------------------------------------------------------
# Paths of a run

class Run:
    def __init__(self, cfg):
        name = cfg['run']
        self.dir = name if os.sep in name else os.path.join(cfg['results'], name)
        self.work = os.path.join(self.dir, 'work')
        self.failures = os.path.join(self.dir, 'failures')
        for d in (self.dir, self.work, self.failures):
            os.makedirs(d, exist_ok=True)

    def path(self, *parts):
        return os.path.join(self.dir, *parts)

    def tmp(self, name):
        p = os.path.join(self.work, name)
        if os.path.exists(p):
            os.unlink(p)
        return p

    def save_failure(self, section, name, src, r):
        """Copy a failing input under failures/<section>/ with the engine's stderr next to it."""
        d = os.path.join(self.failures, section)
        os.makedirs(d, exist_ok=True)
        dst = os.path.join(d, name)
        shutil.copy(src, dst)
        with open(dst + '.stderr.txt', 'wb') as f:
            f.write(r['stderr'][-20000:])
        return os.path.relpath(dst, self.dir)


def write_tsv(path, header, rows):
    with open(path, 'w') as f:
        f.write('\t'.join(header) + '\n')
        for row in rows:
            f.write('\t'.join(str(c) for c in row) + '\n')


def read_tsv(path):
    """Rows as dicts, from a file with a header line; [] when it does not exist."""
    if not os.path.exists(path):
        return []
    lines = [l for l in open(path).read().splitlines() if l]
    if not lines:
        return []
    head = lines[0].split('\t')
    return [dict(zip(head, l.split('\t'))) for l in lines[1:]]


def file_list(cfg):
    """The corpus files: --files, else bench/sets/all.txt (relative to the corpus), else the first
    measurements' list. Only files that exist, in the list's order."""
    candidates = [cfg['files']] if cfg['files'] else [os.path.join(cfg['sets'], 'all.txt'), os.path.join(SCRATCH, 'pdfmax3', 'files.txt')]
    for lst in candidates:
        if lst and os.path.exists(lst):
            out = []
            for line in open(lst):
                line = line.strip()
                if not line or line.startswith('#'):
                    continue
                p = line if os.path.isabs(line) else os.path.join(cfg['corpus'], line)
                if os.path.isfile(p):
                    out.append(p)
            return out
    return []


def rel(cfg, path):
    try:
        return os.path.relpath(path, cfg['corpus']) if path.startswith(cfg['corpus']) else path
    except ValueError:
        return path


def sha256_file(path):
    h = hashlib.sha256()
    with open(path, 'rb') as f:
        for block in iter(lambda: f.read(1 << 20), b''):
            h.update(block)
    return h.hexdigest()


def same_bytes(path, data):
    try:
        if os.path.getsize(path) != len(data):
            return False
        with open(path, 'rb') as f:
            return f.read() == data
    except OSError:
        return False


# ----------------------------------------------------------------------------------------------------
# A minimal PDF builder for the hostile files

def make_pdf(objs, root=1, extra_trailer=b''):
    """A PDF from {number: body} with a classic cross-reference table; `body` is what sits between
    "N 0 obj" and "endobj"."""
    out = bytearray(b'%PDF-1.7\n%\xe2\xe3\xcf\xd3\n')
    offs = {}
    for n in sorted(objs):
        offs[n] = len(out)
        out += f"{n} 0 obj\n".encode() + objs[n] + b"\nendobj\n"
    size = max(objs) + 1
    xref_at = len(out)
    out += f"xref\n0 {size}\n".encode()
    out += b"0000000000 65535 f \n"
    for n in range(1, size):
        out += f"{offs[n]:010d} 00000 n \n".encode() if n in offs else b"0000000000 65535 f \n"
    out += b"trailer\n<< /Size " + str(size).encode() + b" /Root " + str(root).encode() + b" 0 R " + extra_trailer + b" >>\n"
    out += f"startxref\n{xref_at}\n%%EOF\n".encode()
    return bytes(out)


def stream_obj(data, extra=b'', filters=None):
    f = b"/Filter " + filters + b" " if filters else b""
    return b"<< " + f + extra + b" /Length " + str(len(data)).encode() + b" >>\nstream\n" + data + b"\nendstream"


def zeros_deflated(nbytes):
    """zlib stream of nbytes zero bytes, made in pieces so the zeros never sit in memory at once."""
    c = zlib.compressobj(9)
    out = bytearray()
    piece = b'\x00' * MIB
    left = nbytes
    while left > 0:
        n = min(left, MIB)
        out += c.compress(piece[:n])
        left -= n
    out += c.flush()
    return bytes(out)


PAGES = b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>"
PAGE = b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>"


def hostile_pdfs(cfg, dst):
    """The hostile PDFs of the spec, written under dst. Returns [(name, path, what)]."""
    os.makedirs(dst, exist_ok=True)
    p = cfg['pdf']
    cases = []

    def add(name, data, what):
        path = os.path.join(dst, name)
        with open(path, 'wb') as f:
            f.write(data)
        cases.append((name, path, what))

    def doc(obj4, what_name, what):
        add(what_name, make_pdf({1: b"<< /Type /Catalog /Pages 2 0 R /C 4 0 R >>", 2: PAGES, 3: PAGE, 4: obj4}), what)

    n = p['nesting']
    add('nested_array.pdf', make_pdf({1: b"<< /Type /Catalog /Pages 2 0 R /X " + b'[' * n + b'0' + b']' * n + b" >>", 2: PAGES, 3: PAGE}),
        f'array nested {n:,} deep')
    add('nested_dict.pdf', make_pdf({1: b"<< /Type /Catalog /Pages 2 0 R /X " + b'<< /A ' * n + b'0' + b' >>' * n + b" >>", 2: PAGES, 3: PAGE}),
        f'dictionary nested {n:,} deep')
    doc(b"<< /Length 9999999999 >>\nstream\nabc\nendstream", 'length_overflow.pdf', '/Length 9,999,999,999 on a 3-byte stream')
    side = p['image_side']
    add('huge_image.pdf', make_pdf({
        1: b"<< /Type /Catalog /Pages 2 0 R >>", 2: PAGES,
        3: b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Resources << /XObject << /Im 4 0 R >> >> >>",
        4: stream_obj(zlib.compress(b'\x00' * 64, 9), filters=b'/FlateDecode',
                      extra=f"/Subtype /Image /Width {side} /Height {side} /BitsPerComponent 8 /ColorSpace /DeviceGray".encode()),
    }), f'image {side:,} x {side:,} with 64 bytes of data')
    # A single zlib stream expands by about 1032:1 at most, so the 1 GiB bomb is a ~1 MiB stream (1 KiB to
    # 1 GiB is not reachable in one pass of Flate, LZW or RunLength). The nested one keeps the file small.
    bomb = zeros_deflated(p['bomb_mib'] * MIB)
    doc(stream_obj(bomb, filters=b'/FlateDecode'), 'flate_bomb.pdf', f'Flate stream of {len(bomb) // 1024} KiB that decodes to {p["bomb_mib"]} MiB')
    inner = zeros_deflated(p['nested_bomb_mib'] * MIB)
    outer = zlib.compress(inner, 9)
    doc(stream_obj(outer, filters=b'[/FlateDecode /FlateDecode]'), 'flate_bomb_nested.pdf', f'two-stage Flate ({len(outer)} B) decoding to {p["nested_bomb_mib"]} MiB')
    lzw = lzw_runs()
    doc(stream_obj(lzw, filters=b'/LZWDecode'), 'lzw_bomb.pdf', f'LZW stream of {len(lzw)} B that keeps repeating one byte')
    rl = (b'\x00' + b'A') * (p['runlength_mib'] * MIB // 2) + b'\x80'
    doc(stream_obj(rl, filters=b'/RunLengthDecode'), 'runlength_bomb.pdf', f'RunLength stream of {p["runlength_mib"]} MiB expanding 128x')
    add('bad_utf16_name.pdf', make_pdf({
        1: b"<< /Type /Catalog /Pages 2 0 R /T <FEFFD800> /N#41me 5 0 R /#00#ff#7f (x) >>", 2: PAGES, 3: PAGE,
        5: b"<< /V (lone surrogate \xd8\x00 and \xff\xfe) /W <FEFFDC00D800FFFE> >>",
    }), 'invalid UTF-16 strings and #-escaped names')
    add('prev_loop.pdf', prev_loop(), '/Prev pointing in a loop (classic tables)')
    add('xref_stream_loop.pdf', xref_stream_loop(), 'xref stream whose /Prev is itself and /XRefStm points back')
    add('objstm_selfref.pdf', objstm_selfref(), 'xref stream saying an object lives inside object stream 2: itself')
    add('objstm_loop.pdf', objstm_loop(), 'object stream that contains itself and /Extends itself')
    add('million_objects.pdf', million_objects(p['objects']), f'{p["objects"]:,} tiny objects reachable from the catalog')
    return cases


def lzw_runs():
    """A short LZW stream (9 to 12-bit codes, EarlyChange 1) that keeps emitting the same byte, so it
    decodes to far more than its own length."""
    out = bytearray()
    bitbuf = nbits = 0

    def put(code, width):
        nonlocal bitbuf, nbits
        bitbuf = (bitbuf << width) | code
        nbits += width
        while nbits >= 8:
            nbits -= 8
            out.append((bitbuf >> nbits) & 0xFF)

    put(256, 9)  # clear table
    put(ord('A'), 9)
    code, width = 258, 9
    while code < 4095:
        put(code - 1 if code > 258 else ord('A'), width)  # each new code is the previous run plus one 'A'
        code += 1
        if code >= (1 << width) - 1 and width < 12:
            width += 1
    put(257, width)  # end of data
    if nbits:
        out.append((bitbuf << (8 - nbits)) & 0xFF)
    return bytes(out)


def prev_loop():
    body = bytearray(b'%PDF-1.7\n')
    o1 = len(body)
    body += b"1 0 obj\n<< /Type /Catalog >>\nendobj\n"
    xa = len(body)
    body += b"xref\n0 2\n0000000000 65535 f \n" + f"{o1:010d} 00000 n \n".encode()
    body += b"trailer\n<< /Size 2 /Root 1 0 R /Prev 0000000000 >>\nstartxref\n0\n%%EOF\n"
    xb = len(body)
    body += b"xref\n0 1\n0000000000 65535 f \n"
    body += b"trailer\n<< /Size 2 /Root 1 0 R /Prev " + f"{xa:010d}".encode() + b" >>\n"
    body += f"startxref\n{xb}\n%%EOF\n".encode()
    return bytes(body).replace(b"/Prev 0000000000", b"/Prev " + f"{xb:010d}".encode(), 1)


def xref_stream_obj(num, entries, size, extra):
    z = zlib.compress(entries, 9)
    return (f"{num} 0 obj\n<< /Type /XRef /Size {size} /Root 1 0 R /W [1 4 2] /Filter /FlateDecode /Length {len(z)} ".encode()
            + extra + b" >>\nstream\n" + z + b"\nendstream\nendobj\n")


def xref_stream_loop():
    """An xref stream whose /Prev is its own offset, plus /XRefStm pointing at it: every chain loops."""
    body = bytearray(b'%PDF-1.7\n')
    o1 = len(body)
    body += b"1 0 obj\n<< /Type /Catalog >>\nendobj\n"
    o2 = len(body)
    entries = bytes([0, 0, 0, 0, 0, 0xFF, 0xFF]) + bytes([1]) + o1.to_bytes(4, 'big') + bytes([0, 0]) + bytes([1]) + o2.to_bytes(4, 'big') + bytes([0, 0])
    body += xref_stream_obj(2, entries, 3, f"/Prev {o2} /XRefStm {o2}".encode())
    body += f"startxref\n{o2}\n%%EOF\n".encode()
    return bytes(body)


def objstm_selfref():
    """An xref stream whose only normal object is marked as living inside object stream 2: the xref stream
    itself, which is not an object stream."""
    body = bytearray(b'%PDF-1.7\n')
    o2 = len(body)
    entries = bytes([0, 0, 0, 0, 0, 0xFF, 0xFF]) + bytes([2]) + (2).to_bytes(4, 'big') + bytes([0, 0]) + bytes([1]) + o2.to_bytes(4, 'big') + bytes([0, 0])
    body += xref_stream_obj(2, entries, 3, b"")
    body += f"startxref\n{o2}\n%%EOF\n".encode()
    return bytes(body)


def objstm_loop():
    """A real object stream (object 5) whose table says object 5 is inside it, whose /Extends is itself, and
    which the xref stream places inside object stream 5 too."""
    inner = b"<< /Type /ObjStm /N 1 /First 4 /Extends 5 0 R >>"
    payload = b"5 0 " + inner
    z = zlib.compress(payload, 9)
    body = bytearray(b'%PDF-1.7\n')
    o1 = len(body)
    body += b"1 0 obj\n<< /Type /Catalog /Pages 5 0 R >>\nendobj\n"
    o5 = len(body)
    body += (f"5 0 obj\n<< /Type /ObjStm /N 1 /First 4 /Extends 5 0 R /Filter /FlateDecode /Length {len(z)} >>\nstream\n".encode()
             + z + b"\nendstream\nendobj\n")
    o6 = len(body)
    e = bytes([0, 0, 0, 0, 0, 0xFF, 0xFF])
    e += bytes([1]) + o1.to_bytes(4, 'big') + bytes([0, 0])  # 1
    e += bytes([2]) + (5).to_bytes(4, 'big') + bytes([0, 0])  # 2: inside object stream 5
    e += bytes([2]) + (5).to_bytes(4, 'big') + bytes([0, 1])  # 3
    e += bytes([2]) + (5).to_bytes(4, 'big') + bytes([0, 2])  # 4
    e += bytes([2]) + (5).to_bytes(4, 'big') + bytes([0, 0])  # 5: inside itself
    e += bytes([1]) + o6.to_bytes(4, 'big') + bytes([0, 0])  # 6: the xref stream
    body += xref_stream_obj(6, e, 7, b"")
    body += f"startxref\n{o6}\n%%EOF\n".encode()
    return bytes(body)


def million_objects(n):
    objs = {1: b"<< /Type /Catalog /Pages 2 0 R /All [" + b' '.join(f"{i} 0 R".encode() for i in range(3, 3 + n)) + b"] >>",
            2: b"<< /Type /Pages /Kids [] /Count 0 >>"}
    for i in range(3, 3 + n):
        objs[i] = b"0"
    return make_pdf(objs)


# ----------------------------------------------------------------------------------------------------
# Damaged archives

def varied_seeds(cfg, run):
    """Files to pack: a spread of corpus sizes under archive.max_seed_bytes (how a damaged archive is
    refused does not depend on the file's size, and small packs keep the ~70 damaged versions of each
    quick), plus four generated shapes. Returns [(label, path)]."""
    a = cfg['archive']
    files = [f for f in file_list(cfg) if os.path.getsize(f) < a['max_seed_bytes']]
    files.sort(key=lambda f: (os.path.getsize(f), f))
    want = a['packs']
    gen_dir = os.path.join(run.work, 'seeds')
    os.makedirs(gen_dir, exist_ok=True)
    gen = [('empty.bin', b''),
           ('tiny.pdf', b"%PDF-1.4\n1 0 obj\n<< /Type /Catalog >>\nendobj\ntrailer\n<< /Root 1 0 R >>\n%%EOF\n"),
           ('text.bin', b"the quick brown fox " * 500),
           ('random.bin', random.Random(1).randbytes(20000))]
    picked = []
    if files:
        n = max(1, want - len(gen))
        step = max(1, len(files) // n)
        picked = [(rel(cfg, f), f) for f in files[::step][:n]]
    for name, data in gen:
        p = os.path.join(gen_dir, name)
        with open(p, 'wb') as f:
            f.write(data)
        picked.append(('generated/' + name, p))
    return picked[:want]


def damage_cases(cfg, packed):
    """Every damage the spec asks for, as (kind, bytes). The header layout is the engine's: 4 bytes of
    magic, a version byte, the original length as a base-128 varint, a 16-byte hash, then the coded body
    (the parts and their lengths are inside the coded body, so a "part count of a billion" can only come
    from a damaged body: the garbage-body and flip cases stand in for it)."""
    a = cfg['archive']
    n = len(packed)
    cases = []
    rnd = random.Random(a['rng_seed'])
    positions = sorted(set([0, 4, 5, 10, 20] + [rnd.randrange(n) for _ in range(a['flips'])])) if n else []
    for pos in positions:
        if pos >= n:
            continue
        for bit in a['flip_bits']:
            b = bytearray(packed)
            b[pos] ^= 1 << bit
            cases.append((f'flip@{pos}.{bit}', bytes(b)))
    for k in range(1, a['cuts']):
        cases.append((f'cut@{k}_{a["cuts"]}', packed[: n * k // a['cuts']]))
    cases.append(('trailing_garbage', packed + rnd.randbytes(a['garbage'])))
    if n >= 21:
        cases.append(('garbage_body', packed[:21] + rnd.randbytes(2000)))
    if n >= 5:
        b = bytearray(packed)
        b[4] = 99
        cases.append(('future_version', bytes(b)))
        # Older format versions with this body: 0 (the first model) and 2 (stored as it is).
        for v in (0, 2):
            b = bytearray(packed)
            b[4] = v
            cases.append((f'version_{v}_body', bytes(b)))
    cases.append(('declared_16gb', reset_length(packed, 16 << 30)))
    cases.append(('declared_over_max', reset_length(packed, 1 << 40)))
    cases.append(('declared_zero', reset_length(packed, 0)))
    for v in (0, 2):
        b = bytearray(reset_length(packed, 16 << 30))
        if len(b) >= 5:
            b[4] = v
            cases.append((f'version_{v}_declared_16gb', bytes(b)))
    hpos = varint_end(packed)
    if hpos is not None and hpos < n:
        b = bytearray(packed)
        b[hpos] ^= 0xFF
        cases.append(('checksum_flipped', bytes(b)))
        if hpos + 16 <= n:
            b = bytearray(packed)
            b[hpos:hpos + 16] = b'\x00' * 16
            cases.append(('checksum_zeroed', bytes(b)))
    return cases


def varint_end(packed, start=5):
    i = start
    while i < len(packed):
        if not (packed[i] & 0x80):
            return i + 1
        i += 1
    return None


def reset_length(packed, newlen):
    """The archive with its declared original length replaced by newlen."""
    end = varint_end(packed)
    if end is None:
        return packed
    v = bytearray()
    n = newlen
    while True:
        b = n & 0x7F
        n >>= 7
        if n:
            v.append(b | 0x80)
        else:
            v.append(b)
            break
    return bytes(packed[:5]) + bytes(v) + bytes(packed[end:])


def archive_verdict(cfg, tool, r, orig, outp):
    """unpack of a damaged archive must refuse, or restore the exact original. A different file is the
    worst outcome (data loss)."""
    sp = tool.special(r)
    if sp:
        return sp
    word, fail = classify(r, cfg, cfg['archive']['time'], cfg['archive']['mem_mib'] * MIB)
    if fail:
        return word, True
    if r['rc'] == 0:
        return ('restored', False) if same_bytes(outp, orig) else ('WRONG-FILE', True)
    return 'refused', False


def archives(cfg, run, tool=None, tag=''):
    tool = tool or Tool(cfg['engine'], cfg)
    seeds = varied_seeds(cfg, run)
    packs_dir = os.path.join(run.work, tag + 'packs')
    os.makedirs(packs_dir, exist_ok=True)
    header = ['seed', 'file', 'kind', 'result', 'verdict', 'wall_s', 'cpu_s', 'maxrss_mib', 'saved']
    rows = []
    nfail = 0
    ceiling = cfg['as_ceiling_mib'] * MIB
    t_start = time.monotonic()
    for i, (label, seed) in enumerate(seeds):
        pyj = os.path.join(packs_dir, f'seed{i}.pyj')
        with open(seed, 'rb') as f:
            orig = f.read()
        tb, mb = pdf_bounds(cfg, len(orig))
        r = spawn(tool.argv('pack', seed, pyj), timeout=hang_after(cfg, tb), as_limit=ceiling)
        if r['rc'] != 0 or not os.path.exists(pyj):
            word, _ = classify(r, cfg, tb, mb)
            saved = run.save_failure(tag + 'archives', f'seed{i}_pack_failed' + os.path.splitext(seed)[1], seed, r)
            rows.append([f'seed{i}', label, 'pack', 'FAIL', 'pack-failed:' + word, f"{r['wall']:.2f}", f"{r['cpu']:.2f}", r['maxrss'] // MIB, saved])
            nfail += 1
            continue
        with open(pyj, 'rb') as f:
            packed = f.read()
        for kind, bad in damage_cases(cfg, packed):
            badp = os.path.join(run.work, 'case.pyj')
            with open(badp, 'wb') as f:
                f.write(bad)
            outp = run.tmp('restore.tmp')
            r = spawn(tool.argv('unpack', badp, outp), timeout=hang_after(cfg, cfg['archive']['time']), as_limit=ceiling)
            verdict, fail = archive_verdict(cfg, tool, r, orig, outp)
            saved = ''
            if fail:
                nfail += 1
                saved = run.save_failure(tag + 'archives', f'seed{i}_{kind}.pyj', badp, r)
            rows.append([f'seed{i}', label, kind, 'FAIL' if fail else 'PASS', verdict, f"{r['wall']:.3f}", f"{r['cpu']:.3f}", r['maxrss'] // MIB, saved])
        say(f"archives: seed{i} ({label}) done, {len(rows)} cases so far, {nfail} failures, {time.monotonic() - t_start:.0f} s")
    write_tsv(run.path(tag + 'archives.tsv'), header, rows)
    say(f"archives: {len(rows)} cases from {len(seeds)} packs, {nfail} failures")
    return nfail


# ----------------------------------------------------------------------------------------------------
# Hostile PDFs

def pdf_verdict(cfg, tool, run, r, op, path, outp, tb, mb):
    sp = tool.special(r)
    if sp:
        return sp
    word, fail = classify(r, cfg, tb, mb)
    if fail:
        return word, True
    if r['rc'] != 0:
        return 'clean-error', False
    if op == 'optimize':
        # The engine's own self-check stands behind a SMALLER output (an untouched file is the input).
        head = r['stdout'].split(b'\t')
        tag = head[1].decode(errors='replace').lower() if len(head) > 1 else 'ok'
        if tag == 'untouched' and not same_bytes(outp, open(path, 'rb').read()):
            return 'UNTOUCHED-BUT-DIFFERENT', True
        return tag, False
    # pack: prove the archive restores (through unpack, measured under the same bounds).
    back = run.tmp('back.tmp')
    u = spawn(tool.argv('unpack', outp, back), timeout=hang_after(cfg, tb), as_limit=cfg['as_ceiling_mib'] * MIB)
    uword, ufail = classify(u, cfg, tb, mb)
    if ufail:
        return 'unpack-' + uword, True
    if u['rc'] != 0:
        return 'PACK-NOT-RESTORABLE', True
    return ('packed-exact', False) if same_bytes(back, open(path, 'rb').read()) else ('PACK-NOT-EXACT', True)


def pdfs(cfg, run, tool=None, tag=''):
    tool = tool or Tool(cfg['engine'], cfg)
    cases = hostile_pdfs(cfg, os.path.join(run.work, 'hostile'))
    header = ['file', 'op', 'result', 'verdict', 'wall_s', 'cpu_s', 'maxrss_mib', 'bytes_in', 'bytes_out', 'time_bound_s', 'mem_bound_mib', 'saved', 'what']
    rows = []
    nfail = 0
    for name, path, what in cases:
        nbytes = os.path.getsize(path)
        tb, mb = pdf_bounds(cfg, nbytes)
        for op in ('optimize', 'pack'):
            outp = run.tmp('out.tmp')
            r = spawn(tool.argv(op, path, outp), timeout=hang_after(cfg, tb), as_limit=cfg['as_ceiling_mib'] * MIB)
            verdict, fail = pdf_verdict(cfg, tool, run, r, op, path, outp, tb, mb)
            saved = ''
            if fail:
                nfail += 1
                saved = run.save_failure(tag + 'pdfs', name, path, r)
            out_size = os.path.getsize(outp) if os.path.exists(outp) else ''
            rows.append([name, op, 'FAIL' if fail else 'PASS', verdict, f"{r['wall']:.2f}", f"{r['cpu']:.2f}", r['maxrss'] // MIB,
                         nbytes, out_size, f"{tb:.0f}", mb // MIB, saved, what])
            say(f"pdfs: {name} {op}: {verdict} ({r['cpu']:.1f} s CPU, {r['maxrss'] // MIB} MiB)")
    write_tsv(run.path(tag + 'pdfs.tsv'), header, rows)
    say(f"pdfs: {len(rows)} runs over {len(cases)} hostile files, {nfail} failures")
    return nfail


# ----------------------------------------------------------------------------------------------------
# Mutation fuzzing

def fuzz(cfg, run, seconds=None, fresh=False):
    """Mutated corpus files through optimize, pack and unpack, one process at a time, each under the hard
    address-space limit. Every crash, allocation failure, hang or wrong file is a failure and is saved.
    Each entry point's row goes to fuzz.tsv as it finishes, so a stopped run continues with the next."""
    fz = cfg['fuzz']
    seconds = fz['seconds'] if seconds is None else seconds
    tool = Tool(cfg['engine'], cfg)
    limit = fz['mem_mib'] * MIB
    tsv = run.path('fuzz.tsv')
    header = ['entry', 'seeds', 'seconds', 'iterations', 'crashes', 'oom_aborts', 'hangs', 'wrong_files', 'panics']
    done = {} if fresh else {r['entry']: r for r in read_tsv(tsv)}

    # Seeds: small corpus files whose clean pack fits well under the limit, so a hit on the limit later is
    # the mutation's doing. The packs made here are the seeds for unpack.
    files = [f for f in file_list(cfg) if os.path.getsize(f) < fz['max_seed_bytes']][:fz['seeds']]
    random.Random(fz['pick_seed']).shuffle(files)
    seeds_dir = os.path.join(run.work, 'fuzz_seeds')
    os.makedirs(seeds_dir, exist_ok=True)
    seed_rows = []
    pdf_seeds, pyj_seeds = [], []
    for i, f in enumerate(files):
        pyj = os.path.join(seeds_dir, f'seed{i}.pyj')
        tb, mb = pdf_bounds(cfg, os.path.getsize(f))
        r = spawn(tool.argv('pack', f, pyj), timeout=hang_after(cfg, tb), as_limit=limit)
        word, fail = classify(r, cfg, tb, limit * fz['seed_mem_fraction'])
        keep = not fail and r['rc'] == 0 and os.path.exists(pyj)
        seed_rows.append([f'seed{i}', rel(cfg, f), os.path.getsize(f), 'kept' if keep else 'left out', word, f"{r['cpu']:.2f}", r['maxrss'] // MIB])
        if keep:
            pdf_seeds.append(f)
            if len(pyj_seeds) < fz['archive_seeds']:
                pyj_seeds.append((pyj, f))
    write_tsv(run.path('fuzz_seeds.tsv'), ['seed', 'file', 'bytes', 'used', 'clean_verdict', 'cpu_s', 'maxrss_mib'], seed_rows)
    say(f"fuzz: {len(pdf_seeds)} of {len(files)} seeds kept (clean pack under {limit * fz['seed_mem_fraction'] / MIB:.0f} MiB), {len(pyj_seeds)} archives")

    plans = [('optimize', [(f, None) for f in pdf_seeds], 'pdf', 1), ('pack', [(f, None) for f in pdf_seeds], 'pdf', 2), ('unpack', pyj_seeds, 'pyj', 3)]
    rows = [[d[h] for h in header] for d in done.values() if all(h in d for h in header)]
    nbad = sum(int(d['crashes']) + int(d['oom_aborts']) + int(d['hangs']) + int(d['wrong_files']) + int(d['panics']) for d in done.values() if d.get('iterations', '').isdigit())
    for op, seeds, ext, salt in plans:
        if op in done:
            say(f"fuzz {op}: already in fuzz.tsv, skipped (--fresh runs it again)")
            continue
        if not seeds:
            rows.append([op, 0, seconds, 0, 0, 0, 0, 0, 0])
            write_tsv(tsv, header, rows)
            continue
        counts = {'crashes': 0, 'oom_aborts': 0, 'hangs': 0, 'wrong_files': 0, 'panics': 0}
        iters = 0
        t_end = time.monotonic() + seconds
        t_say = time.monotonic()
        mut = os.path.join(run.work, f'mut.{ext}')
        rnd = random.Random(fz['rng_seed'] * 31 + salt)
        while time.monotonic() < t_end:
            seed, orig_path = seeds[iters % len(seeds)]
            with open(seed, 'rb') as f:
                data = bytearray(f.read())
            mutate(data, rnd, fz['edits'])
            with open(mut, 'wb') as f:
                f.write(data)
            outp = run.tmp('fuzz.out')
            r = spawn(tool.argv(op, mut, outp), timeout=cfg['hang'], as_limit=limit)
            iters += 1
            bad = None
            if r['timed_out'] or r['signal'] == signal.SIGXCPU:
                bad, key = 'hang', 'hangs'
            elif r['signal'] == signal.SIGABRT and b'memory allocation of' in r['stderr']:
                bad, key = 'oom_abort', 'oom_aborts'
            elif r['signal']:
                bad, key = 'crash_' + signal.Signals(r['signal']).name, 'crashes'
            elif r['rc'] in (101, 4):
                bad, key = ('panic' if r['rc'] == 101 else 'panic_caught'), 'panics'
            elif op == 'unpack' and r['rc'] == 0 and orig_path and not same_bytes(outp, open(orig_path, 'rb').read()):
                bad, key = 'wrong_file', 'wrong_files'
            if bad:
                counts[key] += 1
                run.save_failure('fuzz', f'{op}_{iters}_{bad}.{ext}', mut, r)
            if time.monotonic() - t_say > 60:
                t_say = time.monotonic()
                say(f"fuzz {op}: {iters} iterations, {sum(counts.values())} failures, {t_end - t_say:.0f} s left")
        rows.append([op, len(seeds), seconds, iters] + [counts[k] for k in ('crashes', 'oom_aborts', 'hangs', 'wrong_files', 'panics')])
        nbad += sum(counts.values())
        write_tsv(tsv, header, rows)
        say(f"fuzz {op}: {iters} iterations, " + ', '.join(f"{v} {k.replace('_', ' ')}" for k, v in counts.items()))
    return nbad


def mutate(data, rnd, edits):
    """A few random edits in place: bit flips, byte substitutions, a cut, a small insert, a chunk copied
    elsewhere, a run of one byte. The length changes as we go, so it is read fresh every edit."""
    if not data:
        data += b'%PDF-1.4\n'
    for _ in range(rnd.randint(1, edits)):
        n = len(data)
        if n == 0:
            data += b'%PDF-1.4\n'
            continue
        kind = rnd.random()
        if kind < 0.45:
            data[rnd.randrange(n)] ^= 1 << rnd.randrange(8)
        elif kind < 0.65:
            data[rnd.randrange(n)] = rnd.randrange(256)
        elif kind < 0.75 and n > 16:
            cut = rnd.randrange(n)
            del data[cut:cut + rnd.randint(1, 64)]
        elif kind < 0.85:
            at = rnd.randrange(n)
            data[at:at] = rnd.randbytes(rnd.randint(1, 64))
        elif kind < 0.93 and n > 32:
            a = rnd.randrange(n - 16)
            ln = rnd.randint(4, 32)
            data[rnd.randrange(n):] = data[a:a + ln]
        else:
            at = rnd.randrange(n)
            data[at:at + rnd.randint(1, 16)] = bytes([rnd.randrange(256)]) * rnd.randint(1, 256)


# ----------------------------------------------------------------------------------------------------
# Supply chain

def supply(cfg, run):
    s = cfg['supply']
    result = {'audit': run_audit(), 'licences': licences(s['features']), 'unsafe': unsafe_blocks(os.path.join(REPO, s['src']))}
    with open(run.path('supply.json'), 'w') as f:
        json.dump(result, f, indent=1)
    bad_lic = [d for d in result['licences'] if not licence_ok(d['license'], set(s['allowed']))]
    uncommented = [u for u in result['unsafe'] if not u['comment']]
    adv = result['audit'].get('vulnerabilities', [])
    fails = len(adv) + len(bad_lic) + len(uncommented) + (1 if 'error' in result['audit'] else 0)
    say(f"supply: {len(adv)} advisories" + (f" (audit error: {result['audit']['error']})" if 'error' in result['audit'] else '') +
        f", {len(bad_lic)} disallowed licences" + (': ' + ', '.join(f"{d['name']} {d['version']} ({d['license']}, via {' > '.join(d['via'])})" for d in bad_lic) if bad_lic else '') +
        f", {len(result['unsafe'])} unsafe blocks ({len(uncommented)} without a reason)")
    return fails


def run_audit():
    if not shutil.which('cargo-audit'):
        return {'error': 'cargo-audit is not installed (cargo install cargo-audit --locked)'}
    try:
        r = subprocess.run(['cargo', 'audit', '--json'], cwd=REPO, capture_output=True, text=True, timeout=600)
        data = json.loads(r.stdout or '{}')
        vulns = data.get('vulnerabilities', {}).get('list', [])
        out = {'vulnerabilities': [{'id': v['advisory']['id'], 'crate': v['package']['name'], 'title': v['advisory'].get('title', '')} for v in vulns],
               'warnings': {k: len(v) for k, v in data.get('warnings', {}).items()}}
        if not r.stdout.strip():
            out['error'] = (r.stderr or 'no output')[-300:].strip()
        return out
    except (OSError, ValueError, subprocess.TimeoutExpired) as e:
        return {'error': str(e)[:300]}


def licences(features):
    """Every dependency of the measured build with its licence and the path from the root that pulls it."""
    args = ['cargo', 'metadata', '--format-version', '1'] + (['--features', features] if features else [])
    r = subprocess.run(args, cwd=REPO, capture_output=True, text=True, timeout=300)
    data = json.loads(r.stdout)
    root = data['resolve']['root']
    deps = {n['id']: [d['pkg'] for d in n.get('deps', [])] for n in data['resolve']['nodes']}
    names = {p['id']: p['name'] for p in data['packages']}
    # Shortest path from the root to each package.
    via = {root: []}
    queue = [root]
    while queue:
        cur = queue.pop(0)
        for d in deps.get(cur, []):
            if d not in via:
                via[d] = via[cur] + [names.get(d, d)]
                queue.append(d)
    out = []
    for p in data['packages']:
        if p['id'] == root or p['id'] not in via:
            continue
        out.append({'name': p['name'], 'version': p['version'],
                    'license': p.get('license') or ('FILE:' + p['license_file'] if p.get('license_file') else 'UNKNOWN'),
                    'via': via[p['id']][:-1] or ['(direct)']})
    return sorted(out, key=lambda d: (d['name'], d['version']))


def licence_ok(expr, allowed):
    if not expr or expr.startswith('UNKNOWN') or expr.startswith('FILE:'):
        return False
    expr = expr.replace('/', ' OR ')  # the old "A/B" spelling means "A OR B"
    for alt in re.split(r'\bOR\b', expr):
        parts = [p.strip(' ()') for p in re.split(r'\bAND\b', alt)]
        if parts and all(p in allowed for p in parts):
            return True
    return False


def unsafe_blocks(src):
    """Every place in our own code that opens an unsafe context (`unsafe {`, `unsafe fn`, `unsafe impl`,
    `unsafe trait`), with the file, line and whether a reason sits next to it: a `//` comment on the line
    or one of the three lines above, or for an `unsafe fn` a `#[target_feature(...)]` attribute, which is
    exactly the condition the caller must meet."""
    found = []
    opener = re.compile(r'\bunsafe\s+(\{|fn\b|impl\b|trait\b)')
    for root, _, names in sorted(os.walk(src)):
        for name in sorted(names):
            if not name.endswith('.rs'):
                continue
            path = os.path.join(root, name)
            lines = open(path, encoding='utf-8', errors='replace').read().splitlines()
            for i, line in enumerate(lines):
                if line.lstrip().startswith('//') or not opener.search(line):
                    continue
                above = lines[max(0, i - 3):i]
                comment = '//' in line or any('//' in c for c in above)
                is_fn = bool(re.search(r'\bunsafe\s+fn\b', line))
                target_feature = is_fn and any('#[target_feature' in c for c in above)
                found.append({'file': os.path.relpath(path, REPO), 'line': i + 1, 'text': line.strip()[:120],
                              'comment': comment or target_feature})
    return found


# ----------------------------------------------------------------------------------------------------
# The suite tests itself

def selfcheck(cfg, run):
    """Plant one of each failure and show the checks catch it; show a clean run is not flagged."""
    tool = Tool(cfg['engine'], cfg)
    rows = []
    ok = True

    def record(name, caught, expect_caught, detail=''):
        nonlocal ok
        good = caught == expect_caught
        ok &= good
        rows.append([name, 'caught' if caught else 'passed', 'ok' if good else 'BAD', detail])

    orig = b'the original bytes'
    outp = run.tmp('sc.out')
    ceiling = cfg['as_ceiling_mib'] * MIB
    # A fake engine that writes different bytes and exits 0: a checker that trusted the exit code would
    # miss the data loss. The archive verdict must say WRONG-FILE.
    fake = fake_engine(run, 'wrong', 'open(sys.argv[3], "wb").write(b"not the original"); sys.exit(0)')
    r = spawn([fake, 'unpack', 'x', outp], timeout=cfg['hang'], as_limit=ceiling)
    v, fail = archive_verdict(cfg, tool, r, orig, outp)
    record('wrong-file', v == 'WRONG-FILE' and fail, True, v)
    # A crash.
    fake = fake_engine(run, 'crash', 'import os, signal; os.kill(os.getpid(), signal.SIGSEGV)')
    r = spawn([fake, 'unpack', 'x', outp], timeout=cfg['hang'], as_limit=ceiling)
    v, fail = classify(r, cfg, cfg['archive']['time'], cfg['archive']['mem_mib'] * MIB)
    record('crash', fail and v.startswith('crash:'), True, v)
    # A panic the library's guard caught: the tool exits 4 (its code for Error::Internal), a bug, not a refusal.
    fake = fake_engine(run, 'caught', 'sys.stderr.write("gpress: internal error: x\\n"); sys.exit(4)')
    r = spawn([fake, 'unpack', 'x', outp], timeout=cfg['hang'], as_limit=ceiling)
    v, fail = archive_verdict(cfg, tool, r, orig, outp)
    record('panic-caught', fail and v == 'panic-caught', True, v)
    # A hang (a short timeout keeps the check quick).
    fake = fake_engine(run, 'hang', 'import time; time.sleep(999)')
    r = spawn([fake, 'unpack', 'x', outp], timeout=2.0, as_limit=ceiling)
    v, fail = classify(r, cfg, cfg['archive']['time'], cfg['archive']['mem_mib'] * MIB)
    record('hang', fail and v == 'hang', True, v)
    # A memory blow-up: 600 MiB touched, against a 512 MiB bound.
    fake = fake_engine(run, 'memory', 'b = bytearray(600 << 20)\nfor i in range(0, len(b), 4096): b[i] = 1\nsys.exit(0)')
    r = spawn([fake, 'unpack', 'x', outp], timeout=cfg['hang'], as_limit=ceiling)
    v, fail = classify(r, cfg, cfg['archive']['time'], 512 * MIB)
    record('memory-over-bound', fail and v.startswith('over-bound:mem'), True, v)
    # An allocation failure under the address-space limit (what the fuzzer's limit produces).
    fake = fake_engine(run, 'oom', 'import os, signal; sys.stderr.write("memory allocation of 1 bytes failed\\n"); sys.stderr.flush(); os.kill(os.getpid(), signal.SIGABRT)')
    r = spawn([fake, 'unpack', 'x', outp], timeout=cfg['hang'], as_limit=ceiling)
    v, fail = classify(r, cfg, cfg['archive']['time'], 512 * MIB)
    record('oom-abort', fail and v == 'oom-abort', True, v)
    # A disallowed licence, and the dual and combined spellings.
    allowed = set(cfg['supply']['allowed'])
    record('licence-gpl', not licence_ok('GPL-3.0-only', allowed) and not licence_ok('MIT AND LGPL-3.0-or-later', allowed), True)
    record('licence-dual-ok', licence_ok('MIT OR GPL-3.0-only', allowed) and licence_ok('MIT/Apache-2.0', allowed), True)
    # An unsafe block without a reason next to it.
    d = os.path.join(run.work, 'unsafe_src')
    os.makedirs(d, exist_ok=True)
    with open(os.path.join(d, 'a.rs'), 'w') as f:
        f.write('fn f() {\n    let x = unsafe { g() };\n}\n// the reason\nunsafe fn g() {}\n')
    found = unsafe_blocks(d)
    record('unsafe-without-reason', len(found) == 2 and not found[0]['comment'] and found[1]['comment'], True, f"{len(found)} found")
    # A clean run of the real engine on a valid pack must not be flagged.
    seed = os.path.join(run.work, 'sc_seed.bin')
    with open(seed, 'wb') as f:
        f.write(b"%PDF-1.4\n% a small valid file\n" + b"content " * 100)
    pyj = run.tmp('sc_seed.pyj')
    spawn(tool.argv('pack', seed, pyj), timeout=cfg['hang'], as_limit=ceiling)
    r = spawn(tool.argv('unpack', pyj, outp), timeout=cfg['hang'], as_limit=ceiling)
    v, fail = archive_verdict(cfg, tool, r, open(seed, 'rb').read(), outp)
    record('clean-run-not-flagged', fail, False, v)

    write_tsv(run.path('selfcheck.tsv'), ['check', 'outcome', 'ok', 'detail'], rows)
    say(f"selfcheck: {'every plant was caught and the clean run passed' if ok else 'A PLANT WAS MISSED'}")
    for row in rows:
        say('  ', ' '.join(str(c) for c in row))
    return 0 if ok else 1


def fake_engine(run, name, body):
    """A tiny Python stand-in engine for the self-check; returns the path of a wrapper that runs it."""
    d = os.path.join(run.work, 'fakes')
    os.makedirs(d, exist_ok=True)
    p = os.path.join(d, f'fake_{name}.py')
    with open(p, 'w') as f:
        f.write('import sys\n' + body + '\n')
    wrap = os.path.join(d, f'fake_{name}.sh')
    with open(wrap, 'w') as f:
        f.write(f'#!/bin/sh\nexec {sys.executable} -I {p} "$@"\n')
    os.chmod(wrap, 0o755)
    return wrap


# ----------------------------------------------------------------------------------------------------
# The C library

def charness(cfg, run):
    """The damaged archives and hostile PDFs through the C library (bench/gpress_harness.c), so a panic
    that escapes the library's guard aborts and is caught, and one the guard catches is reported too.
    Skips with a message when crates/gpress-c is not in the tree."""
    c = cfg['charness']
    crate = os.path.join(REPO, c['crate'])
    header = os.path.join(crate, 'include', 'gpress.h')
    tsv = run.path('charness.tsv')
    if not os.path.isdir(crate) or not os.path.exists(header):
        say(f"charness: {c['crate']} is not in the tree; the C-library harness is skipped")
        write_tsv(tsv, ['status', 'detail'], [['skipped', f"no {c['crate']} in this tree"]])
        return 0
    binp = build_charness(crate, header, os.path.join(run.work, 'gpress_harness'), c['features'])
    if not binp:
        write_tsv(tsv, ['status', 'detail'], [['skipped', 'the harness did not build (see the output above)']])
        return 1
    tool = Tool(binp, cfg, kind='charness')
    nfail = archives(cfg, run, tool, tag='charness-') + pdfs(cfg, run, tool, tag='charness-')
    write_tsv(tsv, ['status', 'detail'], [['ran', f'{nfail} failures']])
    return nfail


def build_charness(crate, header, binp, features):
    """cargo build -p gpress-c, then cc against the static library."""
    env = dict(os.environ)
    env.setdefault('CARGO_TARGET_DIR', os.path.join(REPO, 'target'))
    cmd = ['cargo', 'build', '--release', '-p', os.path.basename(crate)] + (['--features', features] if features else [])
    r = subprocess.run(cmd, cwd=REPO, env=env, capture_output=True, text=True)
    if r.returncode != 0:
        say('charness: cargo build failed:', r.stderr[-800:])
        return None
    lib = os.path.join(env['CARGO_TARGET_DIR'], 'release', 'libgpress_c.a')
    cc = shutil.which('cc') or shutil.which('gcc')
    if not cc or not os.path.exists(lib):
        say(f"charness: no C compiler or no {lib}")
        return None
    r = subprocess.run([cc, '-O2', '-o', binp, os.path.join(HERE, 'gpress_harness.c'), '-I', os.path.dirname(header), lib, '-lpthread', '-ldl', '-lm'],
                       capture_output=True, text=True)
    if r.returncode != 0:
        say('charness: cc failed:', r.stderr[-800:])
        return None
    return binp


# ----------------------------------------------------------------------------------------------------
# Report

def report(cfg, run):
    L = ['## Security (hostile input)', '']
    meta = main_meta(run)
    a, p, fz = cfg['archive'], cfg['pdf'], cfg['fuzz']
    metric = 'CPU time' if cfg['time_metric'] == 'cpu' else 'wall time'
    load = meta.get('load_at_start')
    shared = (f" The machine was shared (load average {load:.1f} on {meta.get('cpus')} CPUs at the start), so wall "
              f"times include waiting for a CPU." if load is not None and meta.get('cpus') and load > meta['cpus'] else '')
    L += [f"Every number here comes from `bench/security.py` (run `{os.path.basename(run.dir)}`"
          + (f", build {meta['build']}, {meta['date']}" if meta else '') + f"), engine at effort {cfg['effort']} with "
          f"{cfg['threads']} thread, each command its own process measured by {meta.get('measured_by', 'wait4')}. "
          f"Time bounds apply to the child's {metric}.{shared} The bounds: a damaged archive is refused in under "
          f"{a['time']:.0f} s with a peak under {a['mem_mib']} MiB; a hostile PDF returns (untouched, or smaller "
          f"and proven by the engine's own self-check) or gives a clean error in under {p['time']:.0f} s plus "
          f"{p['time_per_mib']:.0f} s per MiB of input, with a peak under {p['mem_mib']} MiB or {p['mem_per_byte']} bytes "
          f"per input byte, whichever is more; a run past {cfg['hang']:.0f} s of wall time ({cfg['hang_factor']:.0f}x "
          f"its bound when that is more) is a hang. The fuzzer runs with a hard {fz['mem_mib']} MiB address space.", '']

    arch = read_tsv(run.path('archives.tsv'))
    if arch:
        L += archive_table(arch, cfg, 'Damaged archives', 'unpack')
    pdf = read_tsv(run.path('pdfs.tsv'))
    if pdf:
        L += pdf_table(pdf, 'Hostile PDFs')
    fzr = read_tsv(run.path('fuzz.tsv'))
    if fzr:
        seeds = read_tsv(run.path('fuzz_seeds.tsv'))
        kept = [s for s in seeds if s['used'] == 'kept']
        left = [s for s in seeds if s['used'] != 'kept']
        L += ['### Mutation fuzzing', '',
              f"Mutated corpus files ({len(kept)} seeds under {fz['max_seed_bytes'] / 1e6:.1f} MB whose clean pack peaks under "
              f"{fz['mem_mib'] * fz['seed_mem_fraction']:.0f} MiB; 1 to {fz['edits']} random edits each: bit flips, byte "
              f"changes, cuts, inserts, copied chunks, runs) through each entry point, one process at a time, with a hard "
              f"{fz['mem_mib']} MiB address space and {cfg['hang']:.0f} s. A crash, an allocation failure, a hang, a panic "
              f"(uncaught, exit 101, or caught by the library's guard, exit 4: a bug either way) or (unpack) a wrong file is a "
              f"failure, and the input is saved under `failures/fuzz/` with the engine's stderr.", '',
              '| Entry point | Seeds | Seconds | Iterations | Crashes | Allocation failures | Hangs | Wrong files | Panics |',
              '| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |']
        for r in fzr:
            L.append(f"| {r['entry']} | {r['seeds']} | {r['seconds']} | {r['iterations']} | {r['crashes']} | {r['oom_aborts']} | {r['hangs']} | {r['wrong_files']} | {r['panics']} |")
        L.append('')
        if left:
            L += [f"Seeds left out because their clean pack at effort {cfg['effort']} does not fit the limit with room to spare "
                  f"(a hit on the limit would then be legitimate work, not the mutation's doing); each is a memory finding in itself:", '']
            for s in left:
                L.append(f"- `{s['file']}` ({int(s['bytes']) / 1e6:.2f} MB): the clean pack ended as {s['clean_verdict']} "
                         f"({s['maxrss_mib']} MiB resident at that point, {s['cpu_s']} s CPU)")
            L.append('')

    sp = read_json(run.path('supply.json'))
    if sp:
        adv = sp.get('audit', {}).get('vulnerabilities', [])
        lic = sp.get('licences', [])
        uns = sp.get('unsafe', [])
        allowed = set(cfg['supply']['allowed'])
        bad_lic = [d for d in lic if not licence_ok(d['license'], allowed)]
        uncommented = [u for u in uns if not u['comment']]
        L += ['### Supply chain', '',
              f"- `cargo audit`: " + (f"could not run ({sp['audit']['error']})" if 'error' in sp.get('audit', {}) else
                                       f"{len(adv)} advisories" + (': ' + ', '.join(f"{v['id']} ({v['crate']}: {v['title']})" for v in adv) if adv else '.')),
              f"- Dependencies of the measured build (`--features {cfg['supply']['features']}`): {len(lic)}; the engine's own code has "
              f"none, these come with the optional existing libraries. Licences seen: "
              + ', '.join(sorted(set(d['license'] for d in lic))) + '.']
        if bad_lic:
            for d in bad_lic:
                L.append(f"- **FAIL: `{d['name']} {d['version']}` is `{d['license']}`** (pulled in through {' > '.join(d['via'])}). The rule allows only "
                         f"{', '.join(sorted(allowed))}; GPL, AGPL and LGPL are not allowed.")
        else:
            L.append('- Every dependency licence is on the allowed list.')
        L.append(f"- `unsafe` in our own code: {len(uns)} block{'s' if len(uns) != 1 else ''}, "
                 + ('each with a reason next to it.' if not uncommented else f"**{len(uncommented)} without a reason (FAIL)**."))
        for u in uns:
            L.append(f"  - `{u['file']}:{u['line']}` {'(reason given)' if u['comment'] else '(NO REASON)'} `{u['text']}`")
        L.append('')

    sc = read_tsv(run.path('selfcheck.tsv'))
    if sc:
        good = all(r['ok'] == 'ok' for r in sc)
        L += ['### The suite tests itself', '',
              'Planted failures (a wrong file returned with exit 0, a crash, a hang, a 600 MiB blow-up, an allocation failure, a '
              'GPL licence, an unsafe block without a reason) must each be caught, and a clean run must pass: '
              + ('all as expected.' if good else '**A PLANT WAS MISSED.**'), '']
        L += [f"- {r['check']}: {r['outcome']} ({r['ok']}{', ' + r['detail'] if r.get('detail') else ''})" for r in sc]
        L.append('')

    ch = read_tsv(run.path('charness.tsv'))
    if ch:
        if ch[0]['status'] == 'skipped':
            L += ['### C library', '', f"Skipped: {ch[0]['detail']}. The harness (`bench/gpress_harness.c`) builds against "
                  "`crates/gpress-c/include/gpress.h` and runs the same damaged archives and hostile PDFs through "
                  "`gpress_optimize`, `gpress_compress` and `gpress_expand`; a panic that escapes the library's guard "
                  "aborts it and counts as a crash, one the guard catches comes back as exit 4 and counts as a bug.", '']
        else:
            L += ['### C library', '', f"The same inputs through the C API: {ch[0]['detail']}.", '']
            ca = read_tsv(run.path('charness-archives.tsv'))
            if ca:
                L += archive_table(ca, cfg, 'Damaged archives through gpress_expand', 'expand', level='####')
            cp = read_tsv(run.path('charness-pdfs.tsv'))
            if cp:
                L += pdf_table(cp, 'Hostile PDFs through gpress_optimize and gpress_compress', level='####')

    fails = failures(run)
    L += ['### Failures to fix', '']
    if fails:
        L += ['Each input is saved under `bench/results/security/failures/` next to the engine\'s stderr (`*.stderr.txt`); the '
              'hostile PDFs are regenerated by `security.py pdfs`. The engine is not changed by this suite.', '']
        L += [f"- {f}" for f in fails]
    else:
        L.append('None.')
    L.append('')
    notes = run.path('notes.md')
    if os.path.exists(notes):
        L += ['### Notes on the failures', ''] + open(notes).read().strip().splitlines() + ['']

    write_section(os.path.join(HERE, 'RESULTS.md'), '\n'.join(L).rstrip() + '\n')
    say('report: wrote the Security section of bench/RESULTS.md')
    return 0


def archive_table(arch, cfg, title, op, level='###'):
    npass = sum(1 for r in arch if r['result'] == 'PASS')
    nfail = sum(1 for r in arch if r['result'] == 'FAIL')
    seeds = sorted(set(r['seed'] for r in arch), key=lambda s: int(s[4:]))
    metric = 'cpu_s' if cfg['time_metric'] == 'cpu' else 'wall_s'
    refusals = [r for r in arch if r['verdict'] == 'refused']
    restored = [r for r in arch if r['verdict'] == 'restored']
    slow = max((float(r[metric]) for r in refusals), default=0.0)
    slow_wall = max((float(r['wall_s']) for r in refusals), default=0.0)
    peak = max((int(r['maxrss_mib']) for r in arch), default=0)
    kinds = {}
    for r in arch:
        k = re.sub(r'@.*', '', r['kind'])
        kinds.setdefault(k, [0, 0])[0 if r['result'] == 'PASS' else 1] += 1
    L = [f'{level} {title}', '',
         f"{len(arch)} damaged versions of {len(seeds)} packs (bit flips at {len(cfg['archive']['flip_bits'])} bits of "
         f"{5 + cfg['archive']['flips']} positions, cuts at every 1/{cfg['archive']['cuts']} of the length, trailing garbage, "
         f"a garbage body, a future version and the older versions 0 and 2 with this body, a declared length of 16 GiB, over "
         f"the format's maximum, and zero, the checksum flipped and zeroed). {op} must error or restore the exact original.", '',
         '| Result | Cases | Slowest refusal | Peak memory |', '| --- | ---: | ---: | ---: |',
         f"| Passed ({len(refusals)} refused, {len(restored)} restored exactly) | {npass} | {slow:.2f} s ({slow_wall:.2f} s wall) | {peak} MiB |",
         f"| Failed (wrong file, crash, hang or over bound) | {nfail} | | |", '',
         '| Damage | Passed | Failed |', '| --- | ---: | ---: |']
    L += [f"| {k} | {v[0]} | {v[1]} |" for k, v in sorted(kinds.items())]
    L.append('')
    bad = [r for r in arch if r['result'] == 'FAIL']
    if bad:
        L += [f"- FAIL `{r['seed']}` ({r['file']}) {r['kind']}: {r['verdict']}, {r['cpu_s']} s CPU, {r['maxrss_mib']} MiB (saved: `{r['saved']}`)" for r in bad[:40]]
        if len(bad) > 40:
            L.append(f'- ... and {len(bad) - 40} more in archives.tsv')
        L.append('')
    return L


def pdf_table(pdf, title, level='###'):
    npass = sum(1 for r in pdf if r['result'] == 'PASS')
    L = [f'{level} {title}', '',
         f"{len(pdf)} runs over the files below, each through optimize and pack (pack's archive is then unpacked and "
         f"compared). Verdicts: `untouched` or `smaller` (optimize), `packed-exact`, `clean-error`, or the failure.", '',
         '| File | What it is | Bytes | optimize | pack | Bound |', '| --- | --- | ---: | --- | --- | --- |']
    by = {}
    for r in pdf:
        by.setdefault(r['file'], {'what': r['what'], 'bytes': r['bytes_in'], 'bound': f"{r['time_bound_s']} s, {r['mem_bound_mib']} MiB"})[r['op']] = r
    for name, d in by.items():
        cells = []
        for op in ('optimize', 'pack'):
            r = d.get(op)
            if not r:
                cells.append('')
                continue
            v = f"{r['verdict']} ({r['cpu_s']} s, {r['maxrss_mib']} MiB)"
            cells.append(f"**{v}**" if r['result'] == 'FAIL' else v)
        L.append(f"| `{name}` | {d['what']} | {int(d['bytes']):,} | {cells[0]} | {cells[1]} | {d['bound']} |")
    L += ['', f"Passed {npass} of {len(pdf)} runs.", '']
    return L


def failures(run):
    out = []
    for r in read_tsv(run.path('archives.tsv')):
        if r['result'] == 'FAIL':
            out.append(f"archives: `{r['seed']}` ({r['file']}) {r['kind']}: {r['verdict']} (`{r['saved']}`)")
    for r in read_tsv(run.path('pdfs.tsv')):
        if r['result'] == 'FAIL':
            out.append(f"pdfs: `{r['file']}` through {r['op']}: {r['verdict']}, {r['cpu_s']} s CPU, {r['maxrss_mib']} MiB against {r['time_bound_s']} s / {r['mem_bound_mib']} MiB (`{r['saved']}`)")
    d = os.path.join(run.failures, 'fuzz')
    if os.path.isdir(d):
        for name in sorted(os.listdir(d)):
            if not name.endswith('.stderr.txt'):
                out.append(f"fuzz: `failures/fuzz/{name}`")
    sp = read_json(run.path('supply.json'))
    if sp:
        for v in sp.get('audit', {}).get('vulnerabilities', []):
            out.append(f"supply: advisory {v['id']} in {v['crate']}")
        if 'error' in sp.get('audit', {}):
            out.append(f"supply: cargo audit could not run: {sp['audit']['error']}")
        for d in sp.get('licences', []):
            if not licence_ok(d['license'], set(DEFAULTS['supply']['allowed'])):
                out.append(f"supply: `{d['name']} {d['version']}` is {d['license']} (via {' > '.join(d['via'])})")
        for u in sp.get('unsafe', []):
            if not u['comment']:
                out.append(f"supply: unsafe block without a reason at `{u['file']}:{u['line']}`")
    for r in read_tsv(run.path('selfcheck.tsv')):
        if r['ok'] != 'ok':
            out.append(f"selfcheck: the planted {r['check']} was missed")
    for tag in ('charness-archives', 'charness-pdfs'):
        for r in read_tsv(run.path(tag + '.tsv')):
            if r['result'] == 'FAIL':
                out.append(f"{tag}: `{r.get('seed', r.get('file'))}` {r.get('kind', r.get('op'))}: {r['verdict']} (`{r['saved']}`)")
    return out


def read_json(path):
    if not os.path.exists(path):
        return None
    return json.load(open(path))


def write_section(path, section):
    """Insert or replace the '## Security (hostile input)' section, keeping the rest of the file."""
    marker = '## Security (hostile input)'
    body = open(path).read() if os.path.exists(path) else ''
    if marker in body:
        before = body[:body.index(marker)]
        after = body[body.index(marker):]
        nxt = after.find('\n## ', len(marker))
        rest = after[nxt:] if nxt != -1 else ''
        body = before.rstrip() + '\n\n' + section + ('\n' + rest.lstrip('\n') if rest else '')
    else:
        body = body.rstrip() + '\n\n' + section
    with open(path, 'w') as f:
        f.write(body)


def write_meta(cfg, run, build, mode):
    """meta.json keeps one entry per invocation (mode, date, build, engine, machine, bounds), so a run made
    section by section still says what measured what."""
    try:
        load = os.getloadavg()[0]
    except OSError:
        load = None
    tools = {}
    for name in ('cargo', 'cargo-audit', 'cc'):
        try:
            tools[name] = subprocess.run([name, '--version'], capture_output=True, text=True, timeout=30).stdout.strip().splitlines()[0]
        except (OSError, IndexError, subprocess.TimeoutExpired):
            tools[name] = None
    entry = {'mode': mode, 'date': time.strftime('%Y-%m-%d %H:%M:%S UTC', time.gmtime()), 'build': build, 'engine': cfg['engine'],
             'engine_sha256': sha256_file(cfg['engine']) if os.path.exists(cfg['engine']) else None,
             'corpus': cfg['corpus'], 'cpus': os.cpu_count(), 'load_at_start': load, 'kernel': os.uname().release,
             'python': sys.version.split()[0], 'measured_by': Launcher.note, 'tools': tools,
             'bounds': {'archive': cfg['archive'], 'pdf': cfg['pdf'], 'fuzz': cfg['fuzz'], 'hang': cfg['hang'], 'time_metric': cfg['time_metric']}}
    meta = read_json(run.path('meta.json')) or {}
    runs = meta.get('runs', [])
    runs.append(entry)
    with open(run.path('meta.json'), 'w') as f:
        json.dump({'runs': runs}, f, indent=1)


def main_meta(run):
    """The entry the report describes: the last invocation that measured something (a full run or one of
    its sections; `report` and `selfcheck` don't count), or else the last invocation."""
    meta = read_json(run.path('meta.json')) or {}
    runs = meta.get('runs', [])
    measured = [r for r in runs if r.get('mode') in ('run', 'archives', 'pdfs', 'fuzz', 'supply', 'charness')]
    return (measured or runs or [{}])[-1]


def git_label():
    try:
        branch = subprocess.run(['git', 'rev-parse', '--abbrev-ref', 'HEAD'], cwd=REPO, capture_output=True, text=True).stdout.strip()
        sha = subprocess.run(['git', 'rev-parse', '--short', 'HEAD'], cwd=REPO, capture_output=True, text=True).stdout.strip()
        return f'{branch}-{sha}' if sha else 'unknown'
    except OSError:
        return 'unknown'


# ----------------------------------------------------------------------------------------------------

def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument('mode', choices=['archives', 'pdfs', 'fuzz', 'supply', 'selfcheck', 'charness', 'run', 'report'])
    ap.add_argument('run', nargs='?', help='result directory (a name under bench/results/, or a path); default "security"')
    ap.add_argument('--config', help='JSON file whose keys override DEFAULTS')
    ap.add_argument('--engine', help='the engine binary (default: $GPRESS, $PDFE or target/release/gpress)')
    ap.add_argument('--corpus', help='root of the test files (default: $CORPUS)')
    ap.add_argument('--files', help='the corpus list, one path per line (default: bench/sets/all.txt)')
    ap.add_argument('--effort', type=int, help='engine effort for every run (default 0)')
    ap.add_argument('--threads', type=int, help='engine threads (default 1)')
    ap.add_argument('--packs', type=int, help='archives: packs to damage (default 20)')
    ap.add_argument('--archive-time', type=float, help='seconds a damaged archive may take to be refused (default 5)')
    ap.add_argument('--pdf-time', type=float, help='base seconds a hostile PDF may take (default 30)')
    ap.add_argument('--pdf-time-per-mib', type=float, help='extra seconds per MiB of input (default 5)')
    ap.add_argument('--mem', type=int, help='peak memory bound in MiB for archives and PDFs (default 512)')
    ap.add_argument('--mem-per-byte', type=int, help='hostile PDFs: alternative bound as bytes per input byte (default 16)')
    ap.add_argument('--hang', type=float, help='wall seconds after which a run is killed and is a hang (default 60)')
    ap.add_argument('--time-metric', choices=['cpu', 'wall'], help='which time the bounds apply to (default cpu)')
    ap.add_argument('--fuzz-seconds', type=int, help='fuzz: seconds per entry point (default 600; 0 skips it in run)')
    ap.add_argument('--fuzz-mem', type=int, help='fuzz: the hard address-space limit in MiB (default 512)')
    ap.add_argument('--fresh', action='store_true', help='fuzz: run entry points already in fuzz.tsv again')
    ap.add_argument('--build', help='label of the engine build written to meta.json (default: git HEAD of this tree)')
    a = ap.parse_args()

    cfg = json.loads(json.dumps(DEFAULTS))
    if a.config:
        deep_update(cfg, json.load(open(a.config)))
    for k in ('engine', 'corpus', 'files', 'effort', 'threads', 'hang', 'time_metric'):
        if getattr(a, k) is not None:
            cfg[k] = getattr(a, k)
    if a.run:
        cfg['run'] = a.run
    if a.packs is not None:
        cfg['archive']['packs'] = a.packs
    if a.archive_time is not None:
        cfg['archive']['time'] = a.archive_time
    if a.pdf_time is not None:
        cfg['pdf']['time'] = a.pdf_time
    if a.pdf_time_per_mib is not None:
        cfg['pdf']['time_per_mib'] = a.pdf_time_per_mib
    if a.mem is not None:
        cfg['archive']['mem_mib'] = cfg['pdf']['mem_mib'] = a.mem
    if a.mem_per_byte is not None:
        cfg['pdf']['mem_per_byte'] = a.mem_per_byte
    if a.fuzz_seconds is not None:
        cfg['fuzz']['seconds'] = a.fuzz_seconds
    if a.fuzz_mem is not None:
        cfg['fuzz']['mem_mib'] = a.fuzz_mem
    cfg['engine'] = os.path.abspath(cfg['engine'])
    run = Run(cfg)
    if a.mode != 'report':
        if a.mode != 'supply' and not os.path.exists(cfg['engine']):
            sys.exit(f"engine not found: {cfg['engine']} (set PDFE or --engine)")
        Launcher.build()
        write_meta(cfg, run, a.build or git_label(), a.mode)
    fails = 0
    if a.mode == 'archives':
        fails = archives(cfg, run)
    elif a.mode == 'pdfs':
        fails = pdfs(cfg, run)
    elif a.mode == 'fuzz':
        fails = fuzz(cfg, run, fresh=a.fresh)
    elif a.mode == 'supply':
        fails = supply(cfg, run)
    elif a.mode == 'selfcheck':
        fails = selfcheck(cfg, run)
    elif a.mode == 'charness':
        fails = charness(cfg, run)
    elif a.mode == 'run':
        fails += archives(cfg, run)
        fails += pdfs(cfg, run)
        if cfg['fuzz']['seconds'] > 0:
            fails += fuzz(cfg, run, fresh=a.fresh)
        fails += supply(cfg, run)
        fails += selfcheck(cfg, run)
        fails += charness(cfg, run)
        report(cfg, run)
    elif a.mode == 'report':
        report(cfg, run)
    say(f"{a.mode}: {fails} failure{'s' if fails != 1 else ''}")
    sys.exit(1 if fails else 0)


if __name__ == '__main__':
    main()
