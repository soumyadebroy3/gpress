#!/usr/bin/env python3
"""The standard measurement of the engine: time, space, and a pass/fail check against a stored result.

  benchmark.py time    RUN [options]     measure every file: wall and CPU time, peak memory, bytes, exactness
  benchmark.py space   RUN [options]     the same rows (memory is taken in every run) plus the fixed-cost probe
  benchmark.py report  RUN [options]     tables (text, --markdown, and RUN/*.tsv) from the rows of a run
  benchmark.py compare BASE NEW [opts]   pass/fail: slower (CPU time), larger or more memory than the stored run?
  benchmark.py sets    [options]         write the file lists in bench/sets/ from the corpus and earlier results
  benchmark.py selftest                  plant a slower file, a larger output and more memory; compare must fail
  benchmark.py security ...              part 3, in bench/security.py when it exists

RUN is a directory (made under bench/results/ when the name has no slash). Every measurement is one row of
JSON in RUN/<tool>-<mode>[-e<effort>-t<threads>].jsonl, written as soon as the file is done, so a run that
is stopped continues where it left off, and a second run with other options only does what is missing.
`report` and `compare` take `--with OTHER_RUN`: settings that RUN lacks (the competitors, measured once and
stored) are read from there, on the files RUN measured, so a run of ours alone is cheap and still compared.
Every run starts by timing a fixed job (xz -9e on 4 MiB of fixed text) and stores it in RUN/meta.json as the
machine's calibration: a CPU second is not the same work on every host, and `compare` says when the two
runs' machines differ (`--scale` divides the new run's times by the ratio).

What is measured, file by file, one process at a time, with the child's own `wait4` figures (wall time,
user + system CPU time, peak resident set):
  ours, normal PDF:  `optimize` at each effort and thread count; the output must open (page count and every
                     page's content decoded, with pikepdf) and an untouched file must be byte for byte the
                     input; a sample of outputs goes through bench/verify_pdf.py (qpdf --check, the object
                     graph by value, PDFium pixels and text);
  ours, archive:     `pack`, then a separate `unpack` (measured too) compared byte for byte with the input;
  qpdf:              normal PDF, the setting used since the first comparison (object streams, Flate
                     recompressed at level 9);
  qpdf-max:          normal PDF, qpdf's strongest lossless setting (as qpdf, with the specialized filters
                     decoded too);
  xz, brotli, zstd:  archive, the command-line tools (xz -9e, Brotli 11 with a 16 MiB window, Zstandard 19
                     with long matching), each its own process so its memory is seen as well;
  expandxz:          our preparation of the file (`expand`: every rebuildable stream decoded in place) then
                     xz -9e, plus the bytes a restore needs: the fair "same input, general compressor" line.

The corpus root, the engine, the competitors' commands and every threshold come from DEFAULTS below, the
environment (GPRESS or PDFE for the engine, CORPUS for the corpus), a JSON file (--config) and the options.
Run with python3 -I: the test files are untrusted and nothing from them is ever executed. The row that
describes a file holds its path relative to the corpus root, so runs made on different machines compare."""
import argparse, hashlib, json, os, random, re, select, shutil, statistics, subprocess, sys, tempfile, threading, time

HERE = os.path.dirname(os.path.abspath(__file__))

DEFAULTS = {
    'engine': os.environ.get('GPRESS') or os.environ.get('PDFE') or os.path.join(HERE, '..', 'target', 'release', 'gpress'),
    # Where bench/fetch_corpus.sh puts the files unless told otherwise.
    'corpus': os.environ.get('CORPUS') or os.path.join(HERE, 'corpus'),
    'results': os.path.join(HERE, 'results'),
    'sets': os.path.join(HERE, 'sets'),
    'set': 'sample120',  # a name in bench/sets/ or a path to a list of files
    'who': ['ours', 'qpdf', 'xz', 'brotli', 'zstd'],
    'modes': ['pdf', 'archive'],
    'efforts': [0, 20, 50, 80, 100],
    'threads': [1, 2],
    'timeout': 1800.0,  # seconds per process; a file over it is a TIMEOUT row, never a hang
    'check_timeout': 300.0,  # seconds for one opens-check or one unpack compare
    'verify': 10,  # outputs per normal-PDF setting that go through verify_pdf.py (spread over the list)
    'verify_pages': 30,
    'repeat': 5,  # runs of the fixed-cost probe (the empty page); the smallest wall time counts
    'ratio_min_bytes': 1 << 20,  # peak memory as a multiple of the input is reported for files this big
    'fast_marks': [0.1, 1.0, 10.0],  # "files under N s"
    'largest': 10,  # files in the "memory against input size" table
    'opens_pages': 50,  # pages whose content is decoded by the opens-check
    # The engine's commands. {engine} {in} {out} {effort} {threads} are filled in.
    'ours': {
        'pdf': ['{engine}', 'optimize', '{in}', '{out}', '--effort', '{effort}', '--threads', '{threads}'],
        'archive': ['{engine}', 'pack', '{in}', '{out}', '--effort', '{effort}', '--threads', '{threads}'],
        'unpack': ['{engine}', 'unpack', '{in}', '{out}'],
        'expand': ['{engine}', 'expand', '{in}', '{out}'],
    },
    # The competitors. 'stdout': the tool writes the result to standard output, which goes to {out}.
    'competitors': {
        'qpdf': {'mode': 'pdf', 'ok_rc': [0, 3],
                 'cmd': ['qpdf', '--object-streams=generate', '--compress-streams=y', '--recompress-flate',
                         '--compression-level=9', '--decode-level=generalized', '{in}', '{out}'],
                 'version': ['qpdf', '--version']},
        # qpdf's strongest lossless setting: also the specialized filters decoded and recompressed.
        'qpdf-max': {'mode': 'pdf', 'ok_rc': [0, 3],
                     'cmd': ['qpdf', '--object-streams=generate', '--compress-streams=y', '--recompress-flate',
                             '--compression-level=9', '--decode-level=specialized', '{in}', '{out}'],
                     'version': ['qpdf', '--version']},
        'xz': {'mode': 'archive', 'stdout': True, 'cmd': ['xz', '-9e', '-k', '-c', '-T1', '{in}'], 'version': ['xz', '--version']},
        'brotli': {'mode': 'archive', 'stdout': True, 'cmd': ['brotli', '-q', '11', '-w', '24', '-c', '{in}'], 'version': ['brotli', '--version']},
        'zstd': {'mode': 'archive', 'stdout': True, 'cmd': ['zstd', '-19', '--long=27', '-T1', '-q', '-c', '{in}'], 'version': ['zstd', '--version']},
        'expandxz': {'mode': 'archive', 'cmd': ['xz', '-9e', '-k', '-c', '-T1', '{in}'], 'stdout': True, 'expand': True},
    },
    # The machine's speed, measured at the start of every run and stored in meta.json: a fixed job's CPU time.
    'calibration': {'cmd': ['xz', '-9e', '-c', '-T1', '{in}'], 'mb': 4, 'seed': 2026, 'runs': 3},  # runs 0: skip it
    'compare': {
        'metric': 'cpu',  # the child's user + system time: steady on a shared machine; 'wall' is what a user waits
        'total_tolerance': 0.10,  # a setting may not get more than 10% slower in total
        'file_factor': 2.0,  # no file may get more than 2x slower ...
        'min_seconds': 0.05,  # ... counted on files that took at least this long before (process start noise)
        'mem_tolerance': 0.10,  # the peak memory of a setting may not grow more than 10%
        'mem_floor_kb': 4096,  # ... beyond this many KB (page-level noise on tiny files)
        'load_gap': 1.0,  # a note when the machine's load differed by more than this between the two runs
        'calibration_gap': 0.10,  # a warning when the two runs' machines differ by more than this in the calibration job
        'scale': False,  # scale the new run's times by the calibration ratio before the time rules (--scale)
    },
}


def deep_update(base, over):
    for k, v in over.items():
        if isinstance(v, dict) and isinstance(base.get(k), dict):
            deep_update(base[k], v)
        else:
            base[k] = v
    return base


# ----------------------------------------------------------------------------------------------------
# Running one process and reading its cost

class Ran:
    """One finished child: return code, wall seconds, CPU seconds, peak resident set (KB), and its output."""

    def __init__(self, rc, wall, user, system, maxrss_kb, stdout, stderr, timed_out):
        self.rc, self.wall, self.user, self.system, self.maxrss_kb = rc, wall, user, system, maxrss_kb
        self.stdout, self.stderr, self.timed_out = stdout, stderr, timed_out

    @property
    def cpu(self):
        return self.user + self.system


class Launcher:
    """bench/measure.c, compiled once per harness run: a parent under 1 MB, so that the peak memory it
    reports is the command's own. Linux starts a process with the peak memory of the process that forked
    it, so measuring straight from this 20 MB interpreter would put a 20 MB floor under every reading.
    Without a C compiler the harness measures from here and says so in meta.json."""

    path = None
    note = 'wait4 from the Python process: peak memory readings have a floor of about 20 MB'

    @classmethod
    def build(cls, tmp):
        exe = os.path.join(tmp, 'measure')
        try:
            r = subprocess.run(['cc', '-O2', '-o', exe, os.path.join(HERE, 'measure.c')], capture_output=True, timeout=120)
            if r.returncode == 0:
                cls.path, cls.note = exe, 'bench/measure.c (parent under 1 MB)'
        except (OSError, subprocess.TimeoutExpired):
            pass
        cls.result = os.path.join(tmp, 'measure-result.txt')
        return cls.path is not None


def spawn(cmd, timeout, stdout_path=None, env=None):
    """Run cmd and wait for it; the child's own wall time, CPU time and peak resident set come back.
    stdout goes to stdout_path when given. A child over the timeout is killed and marked."""
    if Launcher.path:
        args = [Launcher.path, '-r', Launcher.result, '-t', str(int(timeout + 0.999))] + (['-o', stdout_path] if stdout_path else []) + ['--'] + cmd
        p = subprocess.Popen(args, stdin=subprocess.DEVNULL, stdout=subprocess.PIPE, stderr=subprocess.PIPE, env=env, start_new_session=True)
        try:
            out, err = p.communicate(timeout=timeout + 60)
        except subprocess.TimeoutExpired:  # the launcher itself is stuck: kill its whole group
            os.killpg(p.pid, 9)
            out, err = p.communicate()
            return Ran(-9, timeout, 0.0, 0.0, 0, out, err, True)
        try:
            with open(Launcher.result) as f:
                rc, wall, user, system, maxrss, timed_out = f.read().split()
            os.unlink(Launcher.result)
        except (OSError, ValueError):
            return Ran(p.returncode or 2, 0.0, 0.0, 0.0, 0, out, err + b'\nmeasure: no result', False)
        return Ran(int(rc), float(wall), float(user), float(system), int(maxrss), out, err, timed_out == '1')
    err = tempfile.TemporaryFile()
    out = open(stdout_path, 'wb') if stdout_path else tempfile.TemporaryFile()
    timed_out = []
    t0 = time.perf_counter()
    try:
        p = subprocess.Popen(cmd, stdin=subprocess.DEVNULL, stdout=out, stderr=err, env=env)
    except OSError as e:
        out.close(); err.close()
        return Ran(127, 0.0, 0.0, 0.0, 0, b'', str(e).encode(), False)

    def kill():
        timed_out.append(True)
        try:
            p.kill()
        except OSError:
            pass

    timer = threading.Timer(timeout, kill)
    timer.start()
    try:
        _, status, ru = os.wait4(p.pid, 0)
        rc = os.waitstatus_to_exitcode(status)
        user, system, maxrss = ru.ru_utime, ru.ru_stime, ru.ru_maxrss
    except ChildProcessError:  # reaped by the kill at the very moment it exited
        rc, user, system, maxrss = -9, 0.0, 0.0, 0
    finally:
        timer.cancel()
    wall = time.perf_counter() - t0
    p.returncode = rc  # so Popen does not wait a second time
    out.seek(0)
    stdout = b'' if stdout_path else out.read()
    err.seek(0)
    stderr = err.read()
    out.close(); err.close()
    return Ran(rc, wall, user, system, maxrss, stdout, stderr, bool(timed_out))


def fill(cmd, **kw):
    return [c.format(**kw) for c in cmd]


def sha256_file(path):
    h = hashlib.sha256()
    with open(path, 'rb') as f:
        for block in iter(lambda: f.read(1 << 20), b''):
            h.update(block)
    return h.hexdigest()


def same_bytes(a, b):
    if os.path.getsize(a) != os.path.getsize(b):
        return False
    with open(a, 'rb') as fa, open(b, 'rb') as fb:
        while True:
            x, y = fa.read(1 << 20), fb.read(1 << 20)
            if x != y:
                return False
            if not x:
                return True


# ----------------------------------------------------------------------------------------------------
# The opens-check: a long-lived child so pikepdf is imported once, restarted if it dies

class Opens:
    """Asks a child (this file, mode _opens) whether an output PDF opens: page count as in the original and
    every page's content decodes. A child that crashes on a hostile file is replaced."""

    def __init__(self, cfg):
        self.cfg, self.p = cfg, None

    def start(self):
        self.p = subprocess.Popen([sys.executable, '-I', os.path.abspath(__file__), '_opens', str(self.cfg['opens_pages'])],
                                  stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL)

    def check(self, original, output):
        if self.p is None or self.p.poll() is not None:
            self.start()
        try:
            self.p.stdin.write((original + '\t' + output + '\n').encode())
            self.p.stdin.flush()
            ready, _, _ = select.select([self.p.stdout], [], [], self.cfg['check_timeout'])
            if not ready:
                self.p.kill(); self.p.wait(); self.p = None
                return {'ok': False, 'error': 'opens-check timeout'}
            line = self.p.stdout.readline()
            if not line:
                self.p.wait(); self.p = None
                return {'ok': False, 'error': 'opens-check died'}
            return json.loads(line)
        except (OSError, ValueError) as e:
            self.p = None
            return {'ok': False, 'error': 'opens-check: ' + str(e)[:200]}

    def close(self):
        if self.p is not None:
            try:
                self.p.stdin.close(); self.p.wait(timeout=10)
            except (OSError, subprocess.TimeoutExpired):
                self.p.kill()
            self.p = None


def opens_child(max_pages):
    """The child side of Opens: one line "ORIGINAL<TAB>OUTPUT" in, one JSON line out."""
    import pikepdf

    def look(path):
        with pikepdf.open(path) as pdf:
            pages = len(pdf.pages)
            problems = []
            for i, page in enumerate(pdf.pages):
                if i >= max_pages:
                    break
                try:
                    c = page.obj.get('/Contents')
                    if c is None:
                        continue
                    for s in (c if isinstance(c, pikepdf.Array) else [c]):
                        s.read_bytes()
                except Exception as e:
                    problems.append(f'page {i + 1}: {type(e).__name__}')
            return pages, problems

    for line in sys.stdin:
        original, output = line.rstrip('\n').split('\t')
        res = {'ok': False}
        try:
            try:
                pa, _ = look(original)
            except Exception as e:
                pa = 'unreadable: ' + type(e).__name__
            try:
                pb, problems = look(output)
                res = {'ok': pa == pb and not problems, 'pages': pb, 'pages_in': pa, 'problems': problems[:3]}
            except Exception as e:
                res = {'ok': False, 'error': 'output: ' + type(e).__name__ + ': ' + str(e)[:120], 'pages_in': pa}
        except Exception as e:  # anything else is a failure of the check itself
            res = {'ok': False, 'error': 'check: ' + str(e)[:120]}
        sys.stdout.write(json.dumps(res) + '\n')
        sys.stdout.flush()


# ----------------------------------------------------------------------------------------------------
# Sets of files

def read_set(cfg, name):
    """The files of a list: a name in bench/sets/ or a path. `NAME:SECTION` takes one section of the list, the
    lines after a marker line `## SECTION` up to the next marker (sample120 has `## slowest-and-largest` and
    `## random`); a marker is a comment to everything else."""
    name, _, section = name.rpartition(':') if ':' in name and '/' not in name.rpartition(':')[2] else (name, '', '')
    path = name if os.path.sep in name or os.path.exists(name) else os.path.join(cfg['sets'], name + '.txt')
    files, current, seen = [], '', set()
    for l in open(path):
        l = l.strip()
        if l.startswith('## '):
            current = l[3:].strip()
            seen.add(current)
        elif l and not l.startswith('#') and (not section or current == section):
            files.append(l)
    if section and section not in seen:
        sys.exit(f'{path} has no section "## {section}" (sections: {", ".join(sorted(seen)) or "none"})')
    return files


def set_name(name):
    name, _, section = name.rpartition(':') if ':' in name and '/' not in name.rpartition(':')[2] else (name, '', '')
    base = os.path.splitext(os.path.basename(name))[0]
    return f'{base}:{section}' if section else base


def make_sets(cfg, a):
    """bench/sets/all.txt: every file of the corpus list; bench/sets/sample120.txt: the slowest, the largest
    and a seeded random rest. Paths are relative to the corpus root."""
    root = os.path.abspath(cfg['corpus']) + os.sep
    files = [l.strip() for l in open(a.files) if l.strip()]
    rel = {}
    for f in files:
        f = os.path.abspath(f) if os.path.isabs(f) else os.path.join(root, f)
        if not f.startswith(root):
            sys.exit(f'{f} is not under the corpus root {root}')
        rel[f] = f[len(root):]
    os.makedirs(cfg['sets'], exist_ok=True)
    with open(os.path.join(cfg['sets'], 'all.txt'), 'w') as w:
        w.write(f'# every file of the corpus list ({len(rel)} files), relative to the corpus root\n')
        w.writelines(r + '\n' for r in sorted(rel.values()))
    times = {}
    if a.times:
        for l in open(a.times):
            p = l.rstrip('\n').split('\t')
            if len(p) >= 5:
                k = os.path.abspath(p[0])
                if k in rel:
                    times[rel[k]] = float(p[4])
    chosen = []
    slow = sorted(times, key=lambda r: (-times[r], r))[:a.slowest]
    large = sorted(rel.values(), key=lambda r: (-os.path.getsize(os.path.join(root, r)), r))[:a.largest]
    for r in slow + large:
        if r not in chosen:
            chosen.append(r)
    picked = len(chosen)
    rest = sorted(rel.values())
    random.Random(a.seed).shuffle(rest)
    for r in rest:
        if len(chosen) >= a.n:
            break
        if r not in chosen:
            chosen.append(r)
    name = f'sample{a.n}'
    with open(os.path.join(cfg['sets'], name + '.txt'), 'w') as w:
        w.write(f'# {len(chosen)} files: the {a.slowest} slowest of {os.path.basename(a.times) if a.times else "-"} at '
                f'maximum effort, the {a.largest} largest ({picked} files, some in both lists), then {len(chosen) - picked} '
                f'random (seed {a.seed}); relative to the corpus root. A "## name" line starts a section that --set '
                f'{name}:name selects.\n')
        w.write('## slowest-and-largest\n')
        w.writelines(r + '\n' for r in chosen[:picked])
        w.write('## random\n')
        w.writelines(r + '\n' for r in chosen[picked:])
    print(f'{cfg["sets"]}: all.txt ({len(rel)} files), {name}.txt ({len(chosen)} files, '
          f'{sum(os.path.getsize(os.path.join(root, r)) for r in chosen) / 1e6:.2f} MB)')


# ----------------------------------------------------------------------------------------------------
# Measuring

def config_name(tool, mode, effort=None, threads=None):
    return f'{tool}-{mode}' + ('' if effort is None else f'-e{effort}-t{threads}')


def load_rows(path):
    rows = []
    if os.path.exists(path):
        for l in open(path):
            l = l.strip()
            if l:
                try:
                    rows.append(json.loads(l))
                except ValueError:
                    pass  # a row cut off when the run was stopped: it is measured again
    return rows


def append_row(path, row):
    with open(path, 'a') as w:
        w.write(json.dumps(row, separators=(',', ':')) + '\n')


def tool_version(cmd):
    try:
        r = subprocess.run(cmd, capture_output=True, text=True, timeout=20)
        return (r.stdout or r.stderr).strip().splitlines()[0][:80]
    except (OSError, subprocess.TimeoutExpired, IndexError):
        return 'missing'


def git_short(path):
    try:
        return subprocess.run(['git', '-C', path, 'rev-parse', '--short', 'HEAD'], capture_output=True, text=True, timeout=20).stdout.strip() or '?'
    except OSError:
        return '?'


def cpu_model():
    try:
        for l in open('/proc/cpuinfo'):
            if l.startswith('model name'):
                return l.split(':', 1)[1].strip()
    except OSError:
        pass
    return None


def calibration_text(mb, seed):
    """A fixed, compressible text of about mb MiB: the same bytes on every machine."""
    words = ('the', 'of', 'and', 'to', 'in', 'stream', 'endstream', 'obj', 'endobj', 'xref', 'trailer', 'Length', 'Filter', 'FlateDecode',
             'Type', 'Page', 'Font', 'Contents', 'Resources', 'MediaBox', 'Parent', 'Kids', 'Count', 'Root', 'Size', 'Width', 'Height')
    rng = random.Random(seed)
    parts, n = [], 0
    while n < mb << 20:
        line = ' '.join(rng.choice(words) + (str(rng.randrange(1000)) if rng.random() < 0.3 else '') for _ in range(12)) + '\n'
        parts.append(line)
        n += len(line)
    return ''.join(parts).encode()


def calibrate(cfg, tmp):
    """How fast this machine is: the CPU seconds of a fixed job (xz -9e on a fixed text, the smallest of a
    few runs). Stored in meta.json, so `compare` can tell a slower build from a slower machine: a CPU second
    is not the same amount of work on every host a container lands on (seen here: the same engine work took
    1.85x the CPU time after a restart)."""
    c = cfg['calibration']
    if not c['runs']:
        return None
    src = os.path.join(tmp, 'calibration.txt')
    with open(src, 'wb') as w:
        w.write(calibration_text(c['mb'], c['seed']))
    best = None
    for _ in range(c['runs']):
        ran = spawn(fill(c['cmd'], **{'in': src}), 600.0, stdout_path=os.path.join(tmp, 'calibration.out'))
        if ran.rc != 0:
            return {'what': ' '.join(c['cmd']), 'error': ran.stderr[-200:].decode('utf-8', 'replace')}
        if best is None or ran.cpu < best.cpu:
            best = ran
    os.unlink(src)
    return {'what': f"{' '.join(c['cmd'][:-1])} on {c['mb']} MiB of fixed text, smallest of {c['runs']}", 'cpu_s': round(best.cpu, 3), 'wall_s': round(best.wall, 3)}


def missing_tools(cfg):
    """The tools a run's checks need but this machine lacks, each with what is lost without it. A run without
    them still measures time and size, but its rows say `exact: false` (or `opens-check died`) for outputs the
    checks could not pass, so `compare` can't tell a lost byte from a missing tool: the list is printed at the
    start and stored in meta.json."""
    out = {}
    try:
        import pikepdf  # noqa: F401
    except ImportError:
        out['pikepdf'] = 'every normal-PDF row becomes "opens-check died" (pip install pikepdf)'
    try:
        import pypdfium2  # noqa: F401
    except ImportError:
        out['pypdfium2'] = 'verify_pdf.py cannot run (pip install pypdfium2)'
    try:
        import PIL  # noqa: F401
    except ImportError:
        out['Pillow'] = 'verify_pdf.py cannot compare JPEG pixels (pip install Pillow)'
    if not (os.environ.get('JPEGTRAN') or shutil.which('jpegtran')):
        out['jpegtran'] = 'every output with a changed JPEG is reported as not identical (libjpeg-turbo-progs, or JPEGTRAN=path)'
    mods = os.environ.get('PDFJS_MODULES')
    if not mods or not os.path.isdir(os.path.join(mods, 'pdfjs-dist')):
        out['pdf.js'] = 'changed JPEGs are not compared in pdf.js (npm install pdfjs-dist; PDFJS_MODULES=its node_modules)'
    if not shutil.which('qpdf'):
        out['qpdf'] = 'verify_pdf.py cannot run qpdf --check, and qpdf cannot be measured'
    if not shutil.which('cc'):
        out['cc'] = 'peak memory readings have a floor of about 20 MB (bench/measure.c is not built)'
    for t in ('xz', 'brotli', 'zstd'):
        if not shutil.which(t):
            out[t] = 'no calibration' if t == 'xz' else f'{t} cannot be measured'
    return out


def write_meta(cfg, run, a, calibration):
    meta = {
        'date': time.strftime('%Y-%m-%d %H:%M:%S UTC', time.gmtime()),
        'build': a.build or git_short(HERE),
        'engine': os.path.abspath(cfg['engine']),
        'engine_sha256': sha256_file(cfg['engine']) if os.path.exists(cfg['engine']) else None,
        'set': set_name(a.set),
        'corpus': os.path.abspath(cfg['corpus']),
        'cpus': os.cpu_count(),
        'cpu_model': cpu_model(),
        'mem_total_kb': next((int(l.split()[1]) for l in open('/proc/meminfo') if l.startswith('MemTotal')), None),
        'kernel': os.uname().release,
        'python': sys.version.split()[0],
        'tools': {k: tool_version(v['version']) for k, v in cfg['competitors'].items() if 'version' in v},
        'missing_tools': missing_tools(cfg),
        'measured_by': Launcher.note,
        'calibration': calibration,
    }
    path = os.path.join(run, 'meta.json')
    old = json.load(open(path)) if os.path.exists(path) else {}
    old.setdefault('runs', []).append(meta)
    for k in ('date', 'build', 'engine', 'engine_sha256', 'set', 'corpus', 'cpus', 'cpu_model', 'mem_total_kb', 'kernel', 'tools', 'missing_tools', 'measured_by', 'calibration'):
        old[k] = meta[k]
    json.dump(old, open(path, 'w'), indent=1)


class Runner:
    def __init__(self, cfg, run, a):
        self.cfg, self.run, self.a = cfg, run, a
        self.tmp = tempfile.mkdtemp(prefix='bench-', dir=a.tmp)
        Launcher.build(self.tmp)
        self.opens = Opens(cfg)
        self.verify_script = os.path.join(HERE, 'verify_pdf.py')

    def close(self):
        self.opens.close()
        shutil.rmtree(self.tmp, ignore_errors=True)

    def base_row(self, rel, tool, mode, effort=None, threads=None):
        src = os.path.join(self.cfg['corpus'], rel)
        row = {'file': rel, 'tool': tool, 'mode': mode, 'bytes_in': os.path.getsize(src)}
        if effort is not None:
            row['effort'], row['threads'] = effort, threads
        return row

    def finish(self, row, ran):
        row.update(wall=round(ran.wall, 4), cpu=round(ran.cpu, 4), user=round(ran.user, 4), sys=round(ran.system, 4),
                   maxrss_kb=ran.maxrss_kb, rc=ran.rc, load1=round(os.getloadavg()[0], 2),
                   at=time.strftime('%Y-%m-%dT%H:%M:%SZ', time.gmtime()))
        if ran.timed_out:
            row['status'] = 'TIMEOUT'
        elif ran.rc != 0 and row.get('status') not in ('OK', 'SMALLER', 'UNTOUCHED'):
            row['status'] = 'PANIC' if b'panicked' in ran.stderr else row.get('status', 'ERROR')
        if ran.stderr and row.get('status') in ('ERROR', 'PANIC', 'TIMEOUT'):
            row['stderr'] = ran.stderr[-400:].decode('utf-8', 'replace')
        return row

    # --- ours, normal PDF
    def ours_pdf(self, rel, effort, threads, verify_this):
        cfg, src = self.cfg, os.path.join(self.cfg['corpus'], rel)
        out = os.path.join(self.tmp, 'out.pdf')
        row = self.base_row(rel, 'ours', 'pdf', effort, threads)
        ran = spawn(fill(cfg['ours']['pdf'], engine=cfg['engine'], **{'in': src}, out=out, effort=effort, threads=threads), cfg['timeout'])
        parts = ran.stdout.decode('utf-8', 'replace').strip().split('\t')
        if ran.rc == 0 and len(parts) >= 5 and parts[1] in ('SMALLER', 'UNTOUCHED') and os.path.exists(out):
            row['status'] = parts[1]
            row['details'] = parts[5] if len(parts) > 5 else ''
            row['bytes_out'] = os.path.getsize(out)
            row['sha256'] = sha256_file(out)
            if parts[1] == 'UNTOUCHED':
                row['exact'] = same_bytes(src, out)
                row['check'] = 'identical' if row['exact'] else 'untouched output differs from the input'
            else:
                o = self.opens.check(src, out)
                row['exact'] = bool(o.get('ok'))
                row['check'] = 'opens' if row['exact'] else 'opens-check: ' + json.dumps(o)[:300]
                if verify_this and row['exact']:
                    v = spawn([sys.executable, '-I', self.verify_script, src, out, '--pages', str(cfg['verify_pages'])], cfg['check_timeout'] * 4)
                    try:
                        res = json.loads(v.stdout.decode('utf-8', 'replace').strip().splitlines()[-1])
                    except (ValueError, IndexError):
                        res = {'identical': False, 'error': (v.stderr[-300:].decode('utf-8', 'replace') or 'no output')}
                    row['verify'] = {k: res.get(k) for k in ('identical', 'qpdf_check', 'object_problems', 'render_problems', 'pages_checked', 'error') if k in res}
                    row['exact'] = row['exact'] and bool(res.get('identical'))
        else:
            row['status'] = 'ERROR'
            row['exact'] = False
        self.finish(row, ran)
        if os.path.exists(out):
            os.unlink(out)
        return row

    # --- ours, archive
    def ours_archive(self, rel, effort, threads):
        cfg, src = self.cfg, os.path.join(self.cfg['corpus'], rel)
        out, back = os.path.join(self.tmp, 'out.pyj'), os.path.join(self.tmp, 'back.pdf')
        row = self.base_row(rel, 'ours', 'archive', effort, threads)
        ran = spawn(fill(cfg['ours']['archive'], engine=cfg['engine'], **{'in': src}, out=out, effort=effort, threads=threads), cfg['timeout'])
        parts = ran.stdout.decode('utf-8', 'replace').strip().split('\t')
        if ran.rc == 0 and os.path.exists(out):
            row['status'] = 'OK'
            row['details'] = parts[4] if len(parts) > 4 else ''
            row['bytes_out'] = os.path.getsize(out)
            row['sha256'] = sha256_file(out)
            u = spawn(fill(cfg['ours']['unpack'], engine=cfg['engine'], **{'in': out}, out=back), cfg['timeout'])
            row['unpack'] = {'wall': round(u.wall, 4), 'cpu': round(u.cpu, 4), 'maxrss_kb': u.maxrss_kb, 'rc': u.rc}
            row['exact'] = u.rc == 0 and os.path.exists(back) and same_bytes(src, back)
            row['check'] = 'unpack identical' if row['exact'] else ('unpack failed: ' + u.stderr[-200:].decode('utf-8', 'replace') if u.rc else 'RESTORE MISMATCH')
        else:
            row['status'] = 'ERROR'
            row['exact'] = False
        self.finish(row, ran)
        for p in (out, back):
            if os.path.exists(p):
                os.unlink(p)
        return row

    # --- a competitor
    def competitor(self, name, rel):
        cfg, src = self.cfg, os.path.join(self.cfg['corpus'], rel)
        c = cfg['competitors'][name]
        out = os.path.join(self.tmp, 'out.' + name)
        row = self.base_row(rel, name, c['mode'])
        extra, prep = 0, None
        if c.get('expand'):
            # Our preparation first (its time and memory count), then the general compressor on the result.
            x = os.path.join(self.tmp, 'expanded')
            prep = spawn(fill(cfg['ours']['expand'], engine=cfg['engine'], **{'in': src}, out=x), cfg['timeout'])
            parts = prep.stdout.decode('utf-8', 'replace').strip().split('\t')
            if prep.rc != 0 or len(parts) < 4 or not os.path.exists(x):
                row['status'] = 'ERROR'
                return self.finish(row, prep)
            extra = int(parts[3])
            row['expanded_bytes'] = int(parts[2])
            src = x
        cmd = fill(c['cmd'], engine=cfg['engine'], **{'in': src}, out=out)
        ran = spawn(cmd, cfg['timeout'], stdout_path=out if c.get('stdout') else None)
        ok = ran.rc in c.get('ok_rc', [0]) and os.path.exists(out) and os.path.getsize(out) > 0
        if prep is not None:
            ran = Ran(ran.rc, ran.wall + prep.wall, ran.user + prep.user, ran.system + prep.system,
                      max(ran.maxrss_kb, prep.maxrss_kb), ran.stdout, ran.stderr, ran.timed_out or prep.timed_out)
        if ok:
            row['status'] = 'OK'
            row['bytes_out'] = os.path.getsize(out) + extra
            if extra:
                row['restore_bytes'] = extra
        else:
            row['status'] = 'ERROR'
        self.finish(row, ran)
        for p in (out, os.path.join(self.tmp, 'expanded')):
            if os.path.exists(p):
                os.unlink(p)
        return row

    # --- the fixed-cost probe: an empty page, repeated
    def fixed(self, label, src, tool, mode, effort, threads):
        cfg = self.cfg
        out = os.path.join(self.tmp, 'fixed.out')
        best = None
        for _ in range(cfg['repeat']):
            if tool == 'ours':
                cmd = fill(cfg['ours'][mode], engine=cfg['engine'], **{'in': src}, out=out, effort=effort, threads=threads)
                ran = spawn(cmd, cfg['timeout'])
            else:
                c = cfg['competitors'][tool]
                ran = spawn(fill(c['cmd'], engine=cfg['engine'], **{'in': src}, out=out), cfg['timeout'], stdout_path=out if c.get('stdout') else None)
            if best is None or ran.wall < best.wall:
                best = Ran(ran.rc, ran.wall, ran.user, ran.system, max(ran.maxrss_kb, best.maxrss_kb if best else 0), ran.stdout, ran.stderr, ran.timed_out)
            size = os.path.getsize(out) if os.path.exists(out) else None
            if os.path.exists(out):
                os.unlink(out)
        row = {'file': label, 'tool': tool, 'mode': mode, 'bytes_in': os.path.getsize(src), 'status': 'OK' if best.rc == 0 else 'ERROR', 'repeat': cfg['repeat']}
        if size is not None:
            row['bytes_out'] = size
        if effort is not None:
            row['effort'], row['threads'] = effort, threads
        return self.finish(row, best)


EMPTY_PAGE = (b'%PDF-1.4\n%\xe2\xe3\xcf\xd3\n'
              b'1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n'
              b'2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n'
              b'3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R >>\nendobj\n'
              b'4 0 obj\n<< /Length 0 >>\nstream\n\nendstream\nendobj\n')


def empty_page_pdf(path):
    """A valid one-page PDF with nothing on the page, with a correct cross-reference table."""
    body = EMPTY_PAGE
    offsets = [body.index(b'%d 0 obj' % n) for n in (1, 2, 3, 4)]
    xref = b'xref\n0 5\n0000000000 65535 f \n' + b''.join(b'%010d 00000 n \n' % o for o in offsets)
    trailer = b'trailer\n<< /Size 5 /Root 1 0 R >>\nstartxref\n%d\n%%%%EOF\n' % len(body)
    with open(path, 'wb') as w:
        w.write(body + xref + trailer)


def plan(cfg, a):
    """The list of (name, tool, mode, effort, threads) to measure."""
    jobs = []
    for who in cfg['who']:
        if who == 'ours':
            for mode in cfg['modes']:
                for e in cfg['efforts']:
                    for t in cfg['threads']:
                        jobs.append((config_name('ours', mode, e, t), 'ours', mode, e, t))
        else:
            c = cfg['competitors'].get(who)
            if c is None:
                sys.exit(f'unknown competitor {who}; known: {", ".join(cfg["competitors"])}')
            if c['mode'] in cfg['modes']:
                jobs.append((config_name(who, c['mode']), who, c['mode'], None, None))
    return jobs


def measure(cfg, a, with_fixed):
    run = run_dir(cfg, a.run)
    os.makedirs(run, exist_ok=True)
    files = read_set(cfg, a.set)
    if a.limit:
        files = files[:a.limit]
    jobs = plan(cfg, a)
    if a.dry_run:
        print(f'{run}: {len(files)} files of {set_name(a.set)}, {len(jobs)} settings:')
        for j in jobs:
            print('  ' + j[0])
        return
    if not os.path.exists(cfg['engine']) and 'ours' in cfg['who']:
        sys.exit(f'engine not found: {cfg["engine"]} (set PDFE or --engine)')
    if not os.path.isdir(cfg['corpus']):
        sys.exit(f'corpus not found: {cfg["corpus"]} (bench/fetch_corpus.sh fetches it; set CORPUS or --corpus for another place)')
    for tool, what in missing_tools(cfg).items():
        print(f'WARNING: {tool} is missing: {what}', flush=True)
    r = Runner(cfg, run, a)
    write_meta(cfg, run, a, calibrate(cfg, r.tmp))
    try:
        for name, tool, mode, effort, threads in jobs:
            path = os.path.join(run, name + '.jsonl')
            done = {row['file'] for row in load_rows(path)}
            todo = [f for f in files if f not in done]
            print(f'{name}: {len(todo)} of {len(files)} files to do', flush=True)
            stride = max(1, len(files) // cfg['verify']) if cfg['verify'] else 0
            t0 = time.time()
            for i, rel in enumerate(files):
                if rel in done:
                    continue
                if tool == 'ours' and mode == 'pdf':
                    row = r.ours_pdf(rel, effort, threads, bool(stride) and i % stride == 0)
                elif tool == 'ours':
                    row = r.ours_archive(rel, effort, threads)
                else:
                    row = r.competitor(tool, rel)
                append_row(path, row)
                if a.verbose:
                    print(f'  {rel}: {row.get("status")} {row.get("wall")} s {row.get("maxrss_kb", 0) // 1024} MB exact={row.get("exact")}', flush=True)
            print(f'{name}: done in {time.time() - t0:.0f} s', flush=True)
        if with_fixed:
            path = os.path.join(run, 'fixed.jsonl')
            done = {(row['tool'], row['mode'], row.get('effort'), row.get('threads')) for row in load_rows(path)}
            src = os.path.join(run, 'empty-page.pdf')
            empty_page_pdf(src)
            for name, tool, mode, effort, threads in jobs:
                if (tool, mode, effort, threads) in done or cfg['competitors'].get(tool, {}).get('expand'):
                    continue
                append_row(path, r.fixed('empty-page.pdf', src, tool, mode, effort, threads))
            print(f'fixed cost: {len(jobs)} settings on the empty page, {cfg["repeat"]} runs each', flush=True)
    finally:
        r.close()


def run_dir(cfg, name):
    return name if os.path.sep in name else os.path.join(cfg['results'], name)


# ----------------------------------------------------------------------------------------------------
# Report

def pct(x, n):
    return 100.0 * x / n if n else 0.0


def quantile(values, q):
    if not values:
        return 0.0
    s = sorted(values)
    k = (len(s) - 1) * q
    lo, hi = int(k), min(int(k) + 1, len(s) - 1)
    return s[lo] + (s[hi] - s[lo]) * (k - lo)


def mb(kb):
    return kb / 1024.0


def fit_line(xs, ys):
    """Least squares y = a + b x; returns (a, b)."""
    n = len(xs)
    if n < 2:
        return (ys[0] if ys else 0.0, 0.0)
    mx, my = sum(xs) / n, sum(ys) / n
    sxx = sum((x - mx) ** 2 for x in xs)
    if sxx == 0:
        return (my, 0.0)
    b = sum((x - mx) * (y - my) for x, y in zip(xs, ys)) / sxx
    return (my - b * mx, b)


class Run:
    """The rows of one run directory, grouped by setting. `only` restricts the files; `extra` names other run
    directories whose settings this run lacks (the stored competitor rows, say) and are merged in, on the
    files this run measured, so a run of ours alone still reports against the baselines."""

    def __init__(self, cfg, path, only=None, extra=()):
        self.path = path
        self.meta = json.load(open(os.path.join(path, 'meta.json'))) if os.path.exists(os.path.join(path, 'meta.json')) else {}
        groups = self.read_groups(path, only)
        self.borrowed = {}
        if extra:
            files = {r['file'] for rows in groups.values() for r in rows}
            for other in extra:
                for n, rows in self.read_groups(other, files).items():
                    if n not in groups:
                        groups[n] = rows
                        self.borrowed[n] = other
        # Normal PDF before archive, ours before the competitors, by effort and thread count.
        order = lambda n: (groups[n][0]['mode'] != 'pdf', groups[n][0]['tool'] != 'ours', groups[n][0].get('effort', -1), groups[n][0].get('threads', -1), n)
        self.groups = {n: groups[n] for n in sorted(groups, key=order)}
        self.fixed = load_rows(os.path.join(path, 'fixed.jsonl'))

    @staticmethod
    def read_groups(path, only):
        groups = {}
        for fn in sorted(os.listdir(path)):
            if fn.endswith('.jsonl') and fn != 'fixed.jsonl':
                rows = load_rows(os.path.join(path, fn))
                if only is not None:
                    rows = [r for r in rows if r['file'] in only]
                if rows:
                    groups[fn[:-6]] = rows
        return groups

    def setting(self, name):
        rows = self.groups[name]
        r = rows[0]
        return r['tool'], r['mode'], r.get('effort'), r.get('threads')

    def label(self, name):
        tool, mode, e, t = self.setting(name)
        return f'{tool} e{e} t{t}' if e is not None else tool


def time_stats(cfg, rows):
    ok = [r for r in rows if r.get('status') in ('OK', 'SMALLER', 'UNTOUCHED')]
    walls = [r['wall'] for r in ok]
    total_wall = sum(walls)
    slowest = max(ok, key=lambda r: r['wall']) if ok else None
    exact = [r for r in rows if 'exact' in r]
    st = {
        'files': len(rows), 'ok': len(ok), 'bytes_in': sum(r['bytes_in'] for r in rows),
        'wall': total_wall, 'cpu': sum(r['cpu'] for r in ok),
        'load': statistics.mean(r.get('load1', 0.0) for r in rows) if rows else 0.0,  # the machine's 1-minute load while measuring: wall times under a load above the CPU count are inflated
        'median': statistics.median(walls) if walls else 0.0, 'p90': quantile(walls, 0.9), 'p99': quantile(walls, 0.99),
        'slowest': (slowest['wall'], slowest['file']) if slowest else (0.0, '-'),
        'mbps': (sum(r['bytes_in'] for r in ok) / 1e6 / total_wall) if total_wall else 0.0,
        'under': [sum(1 for w in walls if w < m) for m in cfg['fast_marks']],
        'exact': (sum(1 for r in exact if r['exact']), len(exact)),
        'failed': [r for r in rows if r.get('status') not in ('OK', 'SMALLER', 'UNTOUCHED')],
        'verified': sum(1 for r in rows if r.get('verify', {}).get('identical')),
        'verify_failed': sum(1 for r in rows if 'verify' in r and not r['verify'].get('identical')),
    }
    if any('unpack' in r for r in ok):
        u = [r['unpack'] for r in ok if 'unpack' in r]
        st['unpack_wall'] = sum(x['wall'] for x in u)
        st['unpack_peak_kb'] = max(x['maxrss_kb'] for x in u)
    return st


def memory_stats(cfg, rows):
    ok = [r for r in rows if r.get('maxrss_kb')]
    if not ok:
        return None
    rss = [r['maxrss_kb'] for r in ok]
    peak = max(ok, key=lambda r: r['maxrss_kb'])
    big = [r for r in ok if r['bytes_in'] >= cfg['ratio_min_bytes']]
    ratios = [r['maxrss_kb'] * 1024.0 / r['bytes_in'] for r in big]
    a, b = fit_line([r['bytes_in'] for r in ok], [r['maxrss_kb'] * 1024.0 for r in ok])
    return {
        'median': statistics.median(rss), 'p90': quantile(rss, 0.9), 'peak': (peak['maxrss_kb'], peak['file'], peak['bytes_in']),
        'ratio_median': statistics.median(ratios) if ratios else None, 'ratio_max': max(ratios) if ratios else None, 'ratio_n': len(ratios),
        'fit_fixed_mb': a / 1048576.0, 'fit_slope': b,
    }


def output_stats(cfg, run):
    """Bytes out against the competitors, per mode, on the files every tool of that mode handled."""
    out = {}
    for mode in ('pdf', 'archive'):
        names = [n for n in run.groups if run.setting(n)[1] == mode]
        if not names:
            continue
        by = {n: {r['file']: r for r in run.groups[n] if r.get('bytes_out') and r.get('status') in ('OK', 'SMALLER', 'UNTOUCHED')} for n in names}
        common = set.intersection(*(set(v) for v in by.values())) if by else set()
        res = {'files': len(common), 'bytes_in': sum(by[names[0]][f]['bytes_in'] for f in common), 'tools': {}}
        comps = [n for n in names if run.setting(n)[0] != 'ours']
        for n in names:
            tot = sum(by[n][f]['bytes_out'] for f in common)
            wins = sum(1 for f in common if all(by[n][f]['bytes_out'] < by[m][f]['bytes_out'] for m in comps if m != n)) if run.setting(n)[0] == 'ours' and comps else None
            res['tools'][n] = {'bytes_out': tot, 'wins': wins}
        plain = [m for m in comps if not cfg['competitors'].get(run.setting(m)[0], {}).get('expand')]
        if len(plain) > 1:  # the best general compressor for every file, as if one could pick after the fact
            res['best_of'] = (sum(min(by[m][f]['bytes_out'] for m in plain) for f in common), [run.label(m) for m in plain])
        if mode == 'pdf':
            for n in names:
                if run.setting(n)[0] == 'ours':
                    rows = run.groups[n]
                    smaller = [r for r in rows if r.get('status') == 'SMALLER']
                    reasons = {}
                    for r in rows:
                        if r.get('status') == 'UNTOUCHED':
                            why = re.sub(r'\d+', 'N', (r.get('details') or '?').split(':')[0])  # "unreadable object 65560" -> one reason
                            reasons[why] = reasons.get(why, 0) + 1
                    q = {r['file']: r for n2 in comps for r in run.groups[n2] if r.get('bytes_out')}
                    both = [r for r in smaller if r['file'] in q]
                    res['tools'][n].update(smaller=len(smaller), untouched=len(rows) - len(smaller), reasons=reasons,
                                           smaller_in=sum(r['bytes_in'] for r in both), smaller_out=sum(r['bytes_out'] for r in both),
                                           smaller_qpdf=sum(min(q[r['file']]['bytes_out'], r['bytes_in']) for r in both), smaller_n=len(both))
        out[mode] = res
    return out


def determinism(run):
    """Outputs of the same effort with different thread counts must be byte-identical."""
    res = {}
    by = {}
    for n in run.groups:
        tool, mode, e, t = run.setting(n)
        if tool == 'ours':
            by.setdefault((mode, e), {})[t] = {r['file']: r.get('sha256') for r in run.groups[n] if r.get('sha256')}
    for (mode, e), ts in by.items():
        if len(ts) < 2:
            continue
        keys = sorted(ts)
        files = set.intersection(*(set(ts[k]) for k in keys))
        same = sum(1 for f in files if len({ts[k][f] for k in keys}) == 1)
        res[f'{mode} e{e}'] = (same, len(files), keys)
    return res


def extra_runs(cfg, a):
    return [run_dir(cfg, x) for x in a.with_runs.split(',')] if a.with_runs else []


def short_error(cfg, text):
    """The end of a stderr text (where the error is) without the corpus path, on one line."""
    return ' '.join(text.replace(os.path.abspath(cfg['corpus']) + os.sep, '').split())[-160:]


def report(cfg, a):
    run = Run(cfg, run_dir(cfg, a.run), set(read_set(cfg, a.set)) if a.set else None, extra_runs(cfg, a))
    md = a.markdown
    lines = []
    P = lines.append
    build = run.meta.get('build', '?')
    P(f'Run {os.path.basename(run.path.rstrip(os.sep))}: build {build}, {run.meta.get("cpus", "?")} CPUs, '
      f'{(run.meta.get("mem_total_kb") or 0) // 1024} MB, {run.meta.get("date", "?")}' + (f', files of {set_name(a.set)}' if a.set else ''))
    cal = run.meta.get('calibration') or {}
    if run.meta.get('cpu_model') or cal.get('cpu_s'):
        P(f"Machine: {run.meta.get('cpu_model') or '?'}" + (f"; calibration: {cal['what']} {cal['cpu_s']:.2f} s of CPU" if cal.get('cpu_s') else ''))
    tools = run.meta.get('tools', {})
    if tools:
        P('Competitors: ' + '; '.join(f'{k}: {v}' for k, v in tools.items()))
    if run.borrowed:
        P('Taken from other runs, on the files of this one: ' + '; '.join(f'{n} from {os.path.basename(p.rstrip(os.sep))}' for n, p in run.borrowed.items()))
    if run.meta.get('note'):
        P('Note: ' + run.meta['note'])
    P('')

    # --- time
    P('## Time' if md else 'TIME')
    head = ['Tool', 'Files', 'MB', 'Total wall s', 'CPU s', 'Load', 'Restore s', 'Median s', 'p90 s', 'p99 s', 'Slowest s', 'Slowest file', 'MB/s'] + \
           [f'< {m:g} s' for m in cfg['fast_marks']] + ['Exact', 'verify_pdf', 'Failed']
    table = []
    tsv_time = []
    loads = []
    for n in run.groups:
        st = time_stats(cfg, run.groups[n])
        tool, mode, e, t = run.setting(n)
        loads.append(st['load'])
        row = [run.label(n) + f' ({mode})', st['files'], f"{st['bytes_in'] / 1e6:.2f}", f"{st['wall']:.1f}", f"{st['cpu']:.1f}", f"{st['load']:.1f}",
               f"{st['unpack_wall']:.1f}" if 'unpack_wall' in st else '', f"{st['median']:.3f}", f"{st['p90']:.2f}",
               f"{st['p99']:.1f}", f"{st['slowest'][0]:.1f}", os.path.basename(st['slowest'][1]), f"{st['mbps']:.2f}"] + \
              [str(u) if tool == 'ours' else '' for u in st['under']] + \
              [f"{st['exact'][0]}/{st['exact'][1]}" if st['exact'][1] else '-', f"{st['verified']}" + (f" ({st['verify_failed']} failed)" if st['verify_failed'] else '') if tool == 'ours' and mode == 'pdf' else '',
               str(len(st['failed'])) if st['failed'] else '']
        table.append(row)
        tsv_time.append([n, tool, mode, e, t, st['files'], st['ok'], st['bytes_in'], f"{st['wall']:.3f}", f"{st['cpu']:.3f}", f"{st['load']:.2f}", f"{st['median']:.4f}", f"{st['p90']:.4f}", f"{st['p99']:.4f}",
                         f"{st['slowest'][0]:.3f}", st['slowest'][1], f"{st['mbps']:.3f}"] + st['under'] + [st['exact'][0], st['exact'][1], st['verified'], st['verify_failed'], len(st['failed']),
                         f"{st.get('unpack_wall', 0):.3f}"])
    P(fmt_table(head, table, md, ('Files', 'MB', 'verify_pdf', 'Failed') if a.brief else ()))
    cpus = run.meta.get('cpus') or os.cpu_count() or 1
    if loads and max(loads) > cpus:
        P('')
        P(f'Load: the 1-minute load average while each setting was measured (mean over its files). This machine has {cpus} CPUs and was '
          f'shared; a load above that inflates wall times (what a user waited) but not CPU times, which is why the regression check uses CPU time.')
    failed = [(n, r) for n in run.groups for r in time_stats(cfg, run.groups[n])['failed']]
    if failed:
        P('')
        P('Failed files:')
        for n, r in failed[:40]:
            P(f"  {n} {r['file']}: {r.get('status')} {r.get('check', '')} {short_error(cfg, r.get('stderr', ''))}")
        if len(failed) > 40:
            P(f'  ... {len(failed) - 40} more')
    not_exact = [(n, r) for n in run.groups for r in run.groups[n] if 'exact' in r and not r['exact'] and r.get('status') in ('OK', 'SMALLER', 'UNTOUCHED')]
    if not_exact:
        P('')
        P('NOT EXACT:')
        for n, r in not_exact[:40]:
            P(f"  {n} {r['file']}: {r.get('check', '')} {json.dumps(r.get('verify', {}))[:300]}")
    det = determinism(run)
    if det:
        P('')
        P('Same output for every thread count: ' + '; '.join(f'{k}: {s} of {m} files identical (threads {", ".join(map(str, ks))})' for k, (s, m, ks) in det.items()))
    P('')

    # --- memory
    P('## Space: memory' if md else 'SPACE: MEMORY')
    head = ['Tool', 'Median MB', 'p90 MB', 'Peak MB', 'Peak file (MB in)', f'Peak / input (files >= {cfg["ratio_min_bytes"] >> 20} MB): median', 'max', 'n', 'Fit: fixed MB', 'Fit: bytes per input byte']
    table, tsv_mem = [], []
    for n in run.groups:
        m = memory_stats(cfg, run.groups[n])
        if not m:
            continue
        tool, mode, e, t = run.setting(n)
        table.append([run.label(n) + f' ({mode})', f"{mb(m['median']):.1f}", f"{mb(m['p90']):.1f}", f"{mb(m['peak'][0]):.1f}",
                      f"{os.path.basename(m['peak'][1])} ({m['peak'][2] / 1e6:.2f})",
                      f"{m['ratio_median']:.1f}" if m['ratio_median'] is not None else '-', f"{m['ratio_max']:.1f}" if m['ratio_max'] is not None else '-', str(m['ratio_n']),
                      f"{m['fit_fixed_mb']:.1f}", f"{m['fit_slope']:.1f}"])
        tsv_mem.append([n, tool, mode, e, t, m['median'], m['p90'], m['peak'][0], m['peak'][1], m['peak'][2],
                        f"{m['ratio_median'] or 0:.3f}", f"{m['ratio_max'] or 0:.3f}", m['ratio_n'], f"{m['fit_fixed_mb']:.3f}", f"{m['fit_slope']:.3f}"])
    P(fmt_table(head, table, md, ('n', 'p90 MB') if a.brief else ()))
    unpack = [(n, time_stats(cfg, run.groups[n])) for n in run.groups if 'unpack' in run.groups[n][0]]
    if unpack:
        P('')
        P('Restore (unpack) peak memory: ' + '; '.join(f"{run.label(n)}: {mb(st['unpack_peak_kb']):.1f} MB" for n, st in unpack))
    if run.fixed:
        P('')
        P('Fixed cost on an empty one-page PDF (smallest wall time of %d runs; peak memory):' % run.fixed[0].get('repeat', 1))
        head = ['Tool', 'Wall ms', 'CPU ms', 'Peak MB', 'Bytes out']
        table = []
        for r in run.fixed:
            lab = f"{r['tool']} e{r['effort']} t{r['threads']}" if 'effort' in r else r['tool']
            table.append([lab + f" ({r['mode']})", f"{r['wall'] * 1000:.1f}", f"{r['cpu'] * 1000:.1f}", f"{mb(r['maxrss_kb']):.1f}", str(r.get('bytes_out', ''))])
        P(fmt_table(head, table, md, ('CPU ms',) if a.brief else ()))
    # memory against input size on the largest files
    largest = {}
    for n in run.groups:
        for r in run.groups[n]:
            largest.setdefault(r['file'], r['bytes_in'])
    top = sorted(largest, key=lambda f: -largest[f])[:cfg['largest']]
    if top:
        P('')
        P(f'Peak memory (MB) against input size on the {len(top)} largest files:')
        names = list(run.groups)
        head = ['File', 'MB in'] + [f'{run.label(n)} {run.setting(n)[1]}' for n in names]
        table = []
        for f in top:
            row = [os.path.basename(f), f'{largest[f] / 1e6:.2f}']
            for n in names:
                r = next((x for x in run.groups[n] if x['file'] == f), None)
                row.append(f"{mb(r['maxrss_kb']):.0f}" if r and r.get('maxrss_kb') else '-')
            table.append(row)
        P(fmt_table(head, table, md))
    P('')

    # --- output size
    P('## Space: output' if md else 'SPACE: OUTPUT')
    os_ = output_stats(cfg, run)
    tsv_size = []
    for mode, res in os_.items():
        fair = [(n, t) for n, t in res['tools'].items() if t.get('smaller_n')]
        P(f"{mode}: {res['files']} files every tool handled, {res['bytes_in'] / 1e6:.2f} MB" +
          (' (an untouched file counts as it is; qpdf rewrites every file and drops what it cannot read, earlier revisions and unreferenced objects, '
           'so its total is not a like-for-like figure: the second table is)' if fair else ''))
        head = ['Tool', 'Bytes out', '% of original', 'Saved %', 'Smallest on']
        table = []
        for n, t in res['tools'].items():
            table.append([run.label(n), f"{t['bytes_out']:,}", f"{pct(t['bytes_out'], res['bytes_in']):.1f}", f"{100 - pct(t['bytes_out'], res['bytes_in']):.1f}",
                          f"{t['wins']} of {res['files']}" if t['wins'] is not None else ''])
            tsv_size.append([mode, n, res['files'], res['bytes_in'], t['bytes_out'], f"{pct(t['bytes_out'], res['bytes_in']):.3f}", t['wins'] if t['wins'] is not None else ''])
        if 'best_of' in res:
            b, who = res['best_of']
            table.append([f"best of {', '.join(who)}, file by file", f'{b:,}', f"{pct(b, res['bytes_in']):.1f}", f"{100 - pct(b, res['bytes_in']):.1f}", ''])
            tsv_size.append([mode, 'best-of-competitors', res['files'], res['bytes_in'], b, f"{pct(b, res['bytes_in']):.3f}", ''])
        P(fmt_table(head, table, md))
        # Normal PDF, the like-for-like table: the files ours made smaller, against qpdf on the same files.
        if fair:
            P('')
            P('The files ours made smaller, against qpdf on the same files (a qpdf output larger than the input counts as the input):')
            head = ['Tool', 'Files smaller', 'Untouched', 'Before', 'Ours', 'Saved %', 'qpdf', 'qpdf saved %']
            table = []
            for n, t in fair:
                table.append([run.label(n), t['smaller_n'], t['untouched'], f"{t['smaller_in'] / 1e6:.2f} MB", f"{t['smaller_out'] / 1e6:.2f} MB", f"{100 - pct(t['smaller_out'], t['smaller_in']):.1f}",
                              f"{t['smaller_qpdf'] / 1e6:.2f} MB", f"{100 - pct(t['smaller_qpdf'], t['smaller_in']):.1f}"])
                tsv_size.append([mode, n + ':smaller', t['smaller_n'], t['smaller_in'], t['smaller_out'], f"{pct(t['smaller_out'], t['smaller_in']):.3f}", ''])
                tsv_size.append([mode, n + ':qpdf-on-the-same', t['smaller_n'], t['smaller_in'], t['smaller_qpdf'], f"{pct(t['smaller_qpdf'], t['smaller_in']):.3f}", ''])
            P(fmt_table(head, table, md))
            shown = None
            for n, t in fair:
                if t['reasons'] and t['reasons'] != shown:  # the gate does not depend on the effort, so this prints once
                    P(f"  {run.label(n)} left {t['untouched']} files untouched because: " + ', '.join(f'{k} {v}' for k, v in sorted(t['reasons'].items(), key=lambda x: -x[1])))
                    shown = t['reasons']
        P('')

    text = '\n'.join(lines)
    print(text)
    if a.tsv:
        w = lambda name, head, rows: open(os.path.join(run.path, name), 'w').write('\t'.join(head) + '\n' + ''.join('\t'.join(str(x) for x in r) + '\n' for r in rows))
        w('time.tsv', ['setting', 'tool', 'mode', 'effort', 'threads', 'files', 'ok', 'bytes_in', 'wall_s', 'cpu_s', 'load1', 'median_s', 'p90_s', 'p99_s', 'slowest_s', 'slowest_file', 'mb_per_s'] +
          [f'under_{m:g}s' for m in cfg['fast_marks']] + ['exact', 'checked', 'verified', 'verify_failed', 'failed', 'unpack_wall_s'], tsv_time)
        w('memory.tsv', ['setting', 'tool', 'mode', 'effort', 'threads', 'median_kb', 'p90_kb', 'peak_kb', 'peak_file', 'peak_file_bytes', 'ratio_median', 'ratio_max', 'ratio_n', 'fit_fixed_mb', 'fit_bytes_per_byte'], tsv_mem)
        w('size.tsv', ['mode', 'setting', 'files', 'bytes_in', 'bytes_out', 'pct_of_original', 'smallest_on'], tsv_size)
        # per file: every setting's bytes out, wall and memory, side by side
        names = list(run.groups)
        files = sorted(largest, key=lambda f: -largest[f])
        rows = []
        for f in files:
            row = [f, largest[f]]
            for n in names:
                r = next((x for x in run.groups[n] if x['file'] == f), None)
                row += [r.get('bytes_out', '') if r else '', r.get('wall', '') if r else '', r.get('maxrss_kb', '') if r else '']
            rows.append(row)
        w('files.tsv', ['file', 'bytes_in'] + [f'{n}:{k}' for n in names for k in ('bytes_out', 'wall_s', 'maxrss_kb')], rows)
        print(f'written: {run.path}/time.tsv memory.tsv size.tsv files.tsv')


def fmt_table(head, rows, md, drop=()):
    """A text or Markdown table; drop names the columns left out (the brief report)."""
    if drop:
        keep = [i for i, h in enumerate(head) if h not in drop]
        head, rows = [head[i] for i in keep], [[r[i] for i in keep] for r in rows]
    if md:
        return '| ' + ' | '.join(head) + ' |\n|' + '|'.join(' ---: ' if i else ' --- ' for i in range(len(head))) + '|\n' + \
            '\n'.join('| ' + ' | '.join(str(x) for x in r) + ' |' for r in rows)
    widths = [max(len(str(x)) for x in col) for col in zip(head, *rows)] if rows else [len(h) for h in head]
    line = lambda r: '  '.join(str(x).ljust(w) if i == 0 else str(x).rjust(w) for i, (x, w) in enumerate(zip(r, widths)))
    return '\n'.join([line(head)] + [line(r) for r in rows])


# ----------------------------------------------------------------------------------------------------
# Compare: the regression check

def compare(cfg, a):
    c = dict(cfg['compare'])
    if a.metric:
        c['metric'] = a.metric
    if a.tolerance is not None:
        c['total_tolerance'] = a.tolerance
    if a.scale:
        c['scale'] = True
    extra = extra_runs(cfg, a)
    base, new = Run(cfg, run_dir(cfg, a.base), extra=extra), Run(cfg, run_dir(cfg, a.new), extra=extra)
    problems, notes = [], []
    common = [n for n in base.groups if n in new.groups]
    if not common:
        sys.exit('nothing to compare: no setting is in both runs')
    # The machines: a CPU second is not the same amount of work on every host.
    scale = 1.0
    cal = [(x.meta.get('calibration') or {}).get('cpu_s') for x in (base, new)]
    if cal[0] and cal[1]:
        ratio = cal[1] / cal[0]
        if abs(ratio - 1) > c['calibration_gap']:
            notes.append(f'the machines differ: the calibration job took {cal[0]:.2f} s of CPU for BASE and {cal[1]:.2f} s for NEW ({ratio:.2f}x)' +
                         (f'; NEW times are divided by {ratio:.2f} before the time rules (--scale)' if c['scale'] else
                          '; a time difference up to that much may be the machine, not the code (--scale takes it out)'))
            if c['scale']:
                scale = 1.0 / ratio
    elif not cal[0] or not cal[1]:
        notes.append('no calibration in ' + ' and '.join(x for x, v in zip(('BASE', 'NEW'), cal) if not v) +
                     ': whether both runs were made on the same machine is not known, and the time rules assume it')
    # The checks behind "exact": a run made without the tools they need has rows that say exact: false for
    # outputs the checks could not pass, and this comparison can't tell those from a lost byte.
    for label, x in (('BASE', base), ('NEW', new)):
        missing = x.meta.get('missing_tools')
        if missing:
            notes.append(f'{label} was measured without {", ".join(sorted(missing))}: the checks behind "exact" did not all run '
                         f'(meta.json missing_tools says what each one lost), so "no longer exact" and "exact" rows prove less')
        elif 'missing_tools' not in x.meta:
            notes.append(f'{label} does not record which checking tools were present (an older benchmark.py made it)')
    for n in common:
        b = {r['file']: r for r in base.groups[n] if r.get('status') in ('OK', 'SMALLER', 'UNTOUCHED')}
        w = {r['file']: r for r in new.groups[n] if r.get('status') in ('OK', 'SMALLER', 'UNTOUCHED')}
        files = sorted(set(b) & set(w))
        if not files:
            continue
        m = c['metric']
        tb, tw = sum(b[f][m] for f in files), sum(w[f][m] for f in files) * scale
        if tw > tb * (1 + c['total_tolerance']):
            problems.append(f'{n}: total {m} {tb:.1f} s -> {tw:.1f} s (+{pct(tw - tb, tb):.0f}%, allowed {c["total_tolerance"] * 100:.0f}%)')
        slow = [(f, b[f][m], w[f][m] * scale) for f in files if b[f][m] >= c['min_seconds'] and w[f][m] * scale > b[f][m] * c['file_factor']]
        for f, x, y in slow[:10]:
            problems.append(f'{n}: {f} {x:.2f} s -> {y:.2f} s ({y / x:.1f}x)')
        if len(slow) > 10:
            problems.append(f'{n}: ... {len(slow) - 10} more files over {c["file_factor"]}x slower')
        larger = [(f, b[f]['bytes_out'], w[f]['bytes_out']) for f in files if 'bytes_out' in b[f] and 'bytes_out' in w[f] and w[f]['bytes_out'] > b[f]['bytes_out']]
        for f, x, y in larger[:10]:
            problems.append(f'{n}: {f} output {x} -> {y} bytes (+{y - x})')
        if len(larger) > 10:
            problems.append(f'{n}: ... {len(larger) - 10} more outputs larger')
        pb, pw = max(b[f].get('maxrss_kb', 0) for f in files), max(w[f].get('maxrss_kb', 0) for f in files)
        if pw > pb * (1 + c['mem_tolerance']) and pw - pb > c['mem_floor_kb']:
            problems.append(f'{n}: peak memory {mb(pb):.1f} MB -> {mb(pw):.1f} MB (+{pct(pw - pb, pb):.0f}%, allowed {c["mem_tolerance"] * 100:.0f}%)')
        lost = [f for f in files if b[f].get('exact') and 'exact' in w[f] and not w[f]['exact']]
        for f in lost[:10]:
            problems.append(f'{n}: {f} is no longer exact')
        broke = [f for f in b if f not in w and any(r['file'] == f for r in new.groups[n])]
        for f in broke[:10]:
            problems.append(f'{n}: {f} failed in the new run')
        same = sum(1 for f in files if b[f].get('sha256') and b[f].get('sha256') == w[f].get('sha256'))
        ob, ow = sum(b[f].get('bytes_out', 0) for f in files), sum(w[f].get('bytes_out', 0) for f in files)
        load = [statistics.mean(x[f].get('load1', 0) for f in files) for x in (b, w)]  # a slow-down under a higher load is suspect
        notes.append(f'{n}: {len(files)} files, {m} {tb:.1f} s -> {tw:.1f} s ({pct(tw - tb, tb):+.1f}%), bytes out {ob:,} -> {ow:,} ({pct(ow - ob, ob):+.2f}%), '
                     f'peak memory {mb(pb):.0f} -> {mb(pw):.0f} MB' + (f', {same} outputs byte-identical' if any(b[f].get('sha256') for f in files) else '') +
                     f', load {load[0]:.1f} -> {load[1]:.1f}')
        if abs(load[0] - load[1]) > c['load_gap']:
            notes.append(f'  (the machine load differed, {load[0]:.1f} against {load[1]:.1f}: a time difference may be the machine, not the code; measure both on a quiet machine to be sure)')
        new_files = {r['file'] for r in new.groups[n]}
        unmeasured = [f for f in b if f not in new_files]  # a partial NEW run: those files are not compared at all
        if unmeasured:
            notes.append(f'  ({len(unmeasured)} of the {len(b)} files of BASE are not in NEW and were not compared; the run is not finished)')
    for s in notes:
        print(s)
    if problems:
        print('\nFAIL')
        for p in problems:
            print('  ' + p)
        sys.exit(1)
    print('\nPASS')


def selftest(cfg, a):
    """The checker must catch a planted slowdown, a larger output and more memory, and pass a copy."""
    tmp = tempfile.mkdtemp(prefix='bench-selftest-')
    base, new = os.path.join(tmp, 'base'), os.path.join(tmp, 'new')
    rows = [{'file': f'f{i}', 'tool': 'ours', 'mode': 'pdf', 'effort': 100, 'threads': 1, 'bytes_in': 1000 * (i + 1), 'bytes_out': 500 * (i + 1),
             'status': 'SMALLER', 'wall': 1.0, 'cpu': 1.0, 'maxrss_kb': 20000, 'exact': True, 'sha256': str(i)} for i in range(10)]
    os.makedirs(base); os.makedirs(new)
    for r in rows:
        append_row(os.path.join(base, 'ours-pdf-e100-t1.jsonl'), r)
        append_row(os.path.join(new, 'ours-pdf-e100-t1.jsonl'), r)
    args = argparse.Namespace(base=base, new=new, metric=None, tolerance=None, with_runs=None, scale=False)
    try:
        compare(cfg, args)
    except SystemExit as e:
        sys.exit(f'selftest: an unchanged run was refused ({e})')
    cases = {'a file 3x slower': dict(wall=3.0, cpu=3.0), 'a larger output': dict(bytes_out=501), 'more memory': dict(maxrss_kb=40000), 'not exact': dict(exact=False)}
    for name, change in cases.items():
        shutil.rmtree(new); os.makedirs(new)
        for r in rows:
            append_row(os.path.join(new, 'ours-pdf-e100-t1.jsonl'), dict(r, **change) if r['file'] == 'f0' else r)
        try:
            compare(cfg, args)
        except SystemExit as e:
            if e.code != 1:
                sys.exit(f'selftest: {name}: wrong exit {e.code}')
            print(f'selftest: {name}: caught')
            continue
        sys.exit(f'selftest: {name} was NOT caught')
    # 15% slower in total, spread over every file: caught by the total rule
    shutil.rmtree(new); os.makedirs(new)
    for r in rows:
        append_row(os.path.join(new, 'ours-pdf-e100-t1.jsonl'), dict(r, wall=1.15, cpu=1.15))
    try:
        compare(cfg, args)
    except SystemExit as e:
        print('selftest: 15% slower in total: caught')
    else:
        sys.exit('selftest: 15% slower in total was NOT caught')
    # 80% slower in total on a machine whose calibration job is 80% slower: refused as it is, passed with --scale
    shutil.rmtree(new); os.makedirs(new)
    for r in rows:
        append_row(os.path.join(new, 'ours-pdf-e100-t1.jsonl'), dict(r, wall=1.8, cpu=1.8))
    json.dump({'calibration': {'what': 'planted', 'cpu_s': 1.0}}, open(os.path.join(base, 'meta.json'), 'w'))
    json.dump({'calibration': {'what': 'planted', 'cpu_s': 1.8}}, open(os.path.join(new, 'meta.json'), 'w'))
    try:
        compare(cfg, args)
    except SystemExit as e:
        print('selftest: 80% slower on a machine 80% slower: caught without --scale')
    else:
        sys.exit('selftest: 80% slower on a slower machine was NOT caught without --scale')
    try:
        compare(cfg, argparse.Namespace(**dict(vars(args), scale=True)))
    except SystemExit as e:
        sys.exit(f'selftest: 80% slower on a machine 80% slower was refused with --scale ({e})')
    print('selftest: 80% slower on a machine 80% slower: passed with --scale')
    shutil.rmtree(tmp)
    print('selftest: all planted failures were caught, the unchanged run passed')


# ----------------------------------------------------------------------------------------------------

def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument('mode', choices=['time', 'space', 'report', 'compare', 'sets', 'selftest', 'security', '_opens'])
    ap.add_argument('args', nargs='*')
    ap.add_argument('--config', help='JSON file whose keys override DEFAULTS')
    ap.add_argument('--engine', help='the engine binary (default: $GPRESS, $PDFE or target/release/gpress)')
    ap.add_argument('--corpus', help='root of the test files (default: $CORPUS)')
    ap.add_argument('--set', help='file list: a name in bench/sets/ or a path (default sample120; report: restrict to these files)')
    ap.add_argument('--who', help='comma list of ours,qpdf,xz,brotli,zstd,expandxz')
    ap.add_argument('--modes', help='comma list of pdf,archive')
    ap.add_argument('--efforts', help='comma list, e.g. 0,50,100')
    ap.add_argument('--threads', help='comma list, e.g. 1,2')
    ap.add_argument('--timeout', type=float)
    ap.add_argument('--verify', type=int, help='outputs per normal-PDF setting through verify_pdf.py (0 = none)')
    ap.add_argument('--repeat', type=int, help='runs of the fixed-cost probe')
    ap.add_argument('--limit', type=int, help='only the first N files of the set')
    ap.add_argument('--build', help='label of the engine build written to meta.json (default: git HEAD of this tree)')
    ap.add_argument('--tmp', help='where outputs are written while they are checked (default: the system temp dir)')
    ap.add_argument('--dry-run', action='store_true')
    ap.add_argument('--verbose', '-v', action='store_true')
    ap.add_argument('--markdown', action='store_true', help='report: Markdown tables')
    ap.add_argument('--brief', action='store_true', help='report: fewer columns (the tables of RESULTS.md)')
    ap.add_argument('--tsv', action='store_true', help='report: also write time.tsv, memory.tsv, size.tsv, files.tsv into the run')
    ap.add_argument('--metric', choices=['wall', 'cpu'], help='compare: which time counts (default cpu; wall is what a user waits, but it moves with the machine load)')
    ap.add_argument('--tolerance', type=float, help='compare: allowed growth of the total time, as a fraction (default 0.10)')
    ap.add_argument('--scale', action='store_true', help='compare: divide the new run\'s times by the ratio of the two machines\' calibration jobs')
    ap.add_argument('--with', dest='with_runs', metavar='RUNS',
                    help='report, compare: comma list of runs whose settings are taken when RUN lacks them, on the files of RUN '
                         '(e.g. the stored competitor rows of all907-speed-ca8fa92, so a run of ours alone reports against them)')
    # sets
    ap.add_argument('--files', help='sets: the corpus list (one path per line)')
    ap.add_argument('--times', help='sets: an earlier ours.tsv (path, status, in, out, seconds) for the slowest files')
    ap.add_argument('--n', type=int, default=120)
    ap.add_argument('--seed', type=int, default=11)
    ap.add_argument('--slowest', type=int, default=20)
    ap.add_argument('--largest', type=int, default=20)
    a = ap.parse_args()

    cfg = json.loads(json.dumps(DEFAULTS))
    if a.config:
        deep_update(cfg, json.load(open(a.config)))
    for k in ('engine', 'corpus', 'timeout', 'verify', 'repeat'):
        if getattr(a, k) is not None:
            cfg[k] = getattr(a, k)
    if a.who:
        cfg['who'] = a.who.split(',')
    if a.modes:
        cfg['modes'] = a.modes.split(',')
    if a.efforts:
        cfg['efforts'] = [int(x) for x in a.efforts.split(',')]
    if a.threads:
        cfg['threads'] = [int(x) for x in a.threads.split(',')]
    if a.mode in ('time', 'space', 'report'):
        if not a.args:
            ap.error('RUN is needed')
        a.run = a.args[0]
        if a.mode != 'report' and not a.set:
            a.set = cfg['set']
    if a.mode == '_opens':
        opens_child(int(a.args[0]) if a.args else cfg['opens_pages'])
    elif a.mode == 'time':
        measure(cfg, a, with_fixed=False)
    elif a.mode == 'space':
        measure(cfg, a, with_fixed=True)
    elif a.mode == 'report':
        report(cfg, a)
    elif a.mode == 'compare':
        if len(a.args) != 2:
            ap.error('compare BASE NEW')
        a.base, a.new = a.args
        compare(cfg, a)
    elif a.mode == 'sets':
        if not a.files:
            ap.error('sets needs --files')
        make_sets(cfg, a)
    elif a.mode == 'selftest':
        selftest(cfg, a)
    elif a.mode == 'security':
        sec = os.path.join(HERE, 'security.py')
        if not os.path.exists(sec):
            sys.exit('the security benchmark (part 3) lives in bench/security.py, which is not in this tree')
        os.execv(sys.executable, [sys.executable, '-I', sec] + a.args)


if __name__ == '__main__':
    main()
