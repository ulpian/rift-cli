"""Bounded native installer/setup/Settings checks in disposable POSIX terminals.

Requires Python 3.11+ and a native Rift binary. Uses local downloads, never GCS.
"""

import argparse
import os
from pathlib import Path
import re
import sys
import tempfile
import tomllib

from preview_install import MANIFEST, ROOT, prepare
from preview_support import environment, project


def advance(term, title):
    term.send(b'\x1b[B' * 30)
    term.capture()
    term.send(b'\r')
    return term.wait(title)


def checked(term, label):
    line = next(line for line in term.capture().splitlines() if label in line and ('●' in line or '○' in line))
    return '●' in line


def check_installer(binary, root, env):
    from terminal_preview import Terminal
    prepare(root, binary)
    env.update(RIFT_BIN_DIR=str(root / 'bin'), RIFT_DOWNLOAD_MANIFEST=MANIFEST,
               RIFT_PREVIEW_ASSETS=str(root / 'assets'),
               PATH=str(root / 'commands') + os.pathsep + env['PATH'])
    # Pipe script source to sh, matching curl | sh. Wizard input must use the terminal device.
    emitter = root / 'emit.py'
    emitter.write_text('import pathlib,sys;sys.stdout.write(pathlib.Path(sys.argv[1]).read_text())')
    command = ['sh', '-c', '"$1" "$2" "$3" | sh', 'rift-test', sys.executable, emitter, ROOT / 'install.sh']
    defaults = root / 'config/rift/defaults.toml'
    with Terminal(command, root, env) as term:
        text = term.wait('Choose your harnesses')
        assert 'Step 2/5' in text and 'rift update --from' in text and 'Rift v' in text
        before = len(term.raw)
        term.capture(0.8)
        assert '█'.encode() in term.raw[before:], 'logo did not shimmer'
        assert not re.search(rb'(?:38|48);2;', term.raw), 'Apple Terminal received RGB escapes'
        term.send(b'\x03')
        term.finish()
    assert not defaults.exists() and not (root / '.rift').exists()
    assert not list((root / 'cache').rglob('*.db'))
    installed = root / 'bin/rift'
    with Terminal([installed, 'install'], root, env) as term:
        term.wait('Choose your harnesses')
        rows = [line for line in term.capture().splitlines() if re.match(r'\s*>?\s*[○●] ', line)]
        for line in rows:
            label = re.sub(r'^\s*>?\s*[○●] ', '', line).strip()
            if '●' in line: term.click(label)
        term.click('Codex')
        advance(term, 'Choose default features')
        for label in ['Total Recall', 'Raw Transcripts']:
            if checked(term, label): term.click(label)
        advance(term, 'Review installation defaults')
        assert not defaults.exists()
        advance(term, 'Confirm defaults')
        assert not defaults.exists()
        term.send(b'\r')
        term.finish()
    data = tomllib.loads(defaults.read_text())
    assert data['harnesses'] == ['codex']
    assert data['features']['total_recall'] is False and data['features']['raw_transcripts'] is False
    assert not (root / '.rift').exists()
    print('PASS piped installer, shimmer, cancellation, reviewed defaults and save verification', flush=True)
    return installed


def check_configured_clone(binary, root, env):
    from terminal_preview import Terminal
    repo = project(root, env, name='configured-clone')
    (repo / '.rift').mkdir()
    policy = repo / '.rift/config.toml'
    source = 'schema_version=1\n[project]\nname="Configured clone"\n[features]\ntotal_recall=true\n'
    policy.write_text(source)
    with Terminal([binary], repo, env) as term:
        term.wait('Choose your harnesses')
        selected = [re.sub(r'^\s*>?\s*● ', '', line).strip()
                    for line in term.capture().splitlines() if re.match(r'\s*>?\s*● ', line)]
        assert selected == ['Codex'], selected
        advance(term, 'Review harness permissions')
        advance(term, 'Choose project settings')
        assert checked(term, 'Total Recall'), 'global defaults overrode declared policy'
        term.send(b'\x03')
        term.finish()
    assert policy.read_text() == source
    print('PASS configured clone: saved harnesses selected, declared features preserved', flush=True)


def check_project(binary, root, env):
    from terminal_preview import Terminal
    repo = project(root, env)
    with Terminal([binary], repo, env) as term:
        text = term.wait('Choose your harnesses')
        assert 'Step 1/5' in text and 'rift doctor' in text and 'rift --help' in text
        assert checked(term, 'Codex'), 'saved harness did not seed project setup'
        term.click('Codex')  # No harness wiring is needed for this isolated Settings check.
        advance(term, 'Review harness permissions')
        advance(term, 'Choose project settings')
        assert not checked(term, 'Total Recall')
        assert not checked(term, 'Raw Transcripts')
        term.send(b'\x03')
        term.finish()
    assert not (repo / '.rift').exists()
    with Terminal([binary], repo, env) as term:
        term.wait('Choose your harnesses')
        term.click('Codex')
        advance(term, 'Review harness permissions')
        advance(term, 'Choose project settings')
        advance(term, 'Review and set up')
        advance(term, 'Confirm setup')
        term.send(b'\r')  # Exactly one final Enter.
        term.wait('[Home]', timeout=25)
        term.send(b'\x1b[Z')
        term.wait('Project Settings')
        before = checked(term, 'Tool-Call Capture')
        term.click('Tool-Call Capture')
        term.capture(1)
        assert checked(term, 'Tool-Call Capture') != before
        policy = tomllib.loads((repo / '.rift/config.toml').read_text())
        assert policy['capture']['tool_calls'] != before
        assert not re.search(rb'(?:38|48);2;', term.raw)
    print('PASS project defaults, cancel without writes, final confirmation, milestones and Settings toggle', flush=True)


def check(binary):
    if os.name != 'posix': raise SystemExit('Native PTY checks require macOS or Linux.')
    with tempfile.TemporaryDirectory(prefix='rift-flow-check-') as temporary:
        root = Path(temporary)
        env = environment(root)
        env.pop('NO_COLOR', None)
        env.update(TERM='xterm-256color', TERM_PROGRAM='Apple_Terminal', COLORTERM='truecolor', LANG='en_US.UTF-8')
        installed = check_installer(binary, root, env)
        check_configured_clone(installed, root, env)
        # Hook installation deliberately refuses binaries under system temp roots.
        # Use the supplied durable release executable (the exact bytes installed above)
        # for project setup; keep that safety rule intact.
        check_project(binary, root, env)
    print('PASS temporary projects, binaries, archives and caches cleaned up; production installation untouched.', flush=True)


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--binary', type=Path, required=True)
    args = parser.parse_args()
    check(args.binary.expanduser().resolve(strict=True))
