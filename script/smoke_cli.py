#!/usr/bin/env python3
"""Exercise a packaged mdt executable in disposable state, optionally via an emulator.

Usage: smoke_cli.py /path/to/command [--runner /path/to/runner]
The runner must accept the command path and arguments (a shell wrapper can add
Rosetta's architecture flag or Blink's -jm). No credentials or real project data
are used, and no network operation is requested.
"""
import argparse
import json
import os
from pathlib import Path
import subprocess
import tempfile


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('binary', type=Path)
    parser.add_argument('--runner', type=Path)
    args = parser.parse_args()
    command = ([str(args.runner.resolve())] if args.runner else []) + [str(args.binary.resolve())]
    with tempfile.TemporaryDirectory(prefix='mdt-release-smoke-') as temporary:
        root = Path(temporary)
        env = os.environ | {'HOME': temporary, 'QM_STATE_DIR': str(root / 'markdown-state')}

        def run(*arguments, code=0):
            result = subprocess.run(command + list(arguments), cwd=root, env=env,
                                    capture_output=True, text=True, timeout=60)
            if result.returncode != code:
                raise AssertionError(f'{arguments}: expected exit {code}, got {result.returncode}\n'
                                     f'{result.stdout}\n{result.stderr}')
            return result.stdout

        def data(*arguments):
            return json.loads(run(*arguments, '--json'))

        version = run('--version').strip()
        assert 'mdt' in run('--help')
        before = '# Smoke\r\n\r\n## Plans\r\nOriginal café.\r\n\r\n## Removed\r\nRemember this deletion.\r\n'
        after = '# Smoke\r\n\r\n## Plans\r\nHuman revision.\r\n'
        file = root / 'example.md'
        file.write_bytes(before.encode())
        reference = str(file)
        initial = data('read', reference)
        file.write_bytes(after.encode())
        baseline = initial['revision']
        assert initial['content'] == before
        changes = data('diff', reference, '--since', baseline)
        encoded_changes = json.dumps(changes, ensure_ascii=False)
        assert 'Remember this deletion' in encoded_changes, encoded_changes
        assert 'Human revision' in encoded_changes, encoded_changes
        assert data('read', reference, '--rev', baseline)['content'] == before
        current = data('read', reference)
        assert current['content'] == after
        # A stale edit of changed text must fail and preserve the human's save.
        replacement = root / 'replacement.md'
        replacement.write_text('## Plans\nUnsafe overwrite.\n')
        run('replace', reference, '--rev', baseline, '--node', 'Plans', '--body', str(replacement), code=5)
        assert data('read', reference)['content'] == after
        data('replace', reference, '--rev', current['revision'], '--old', 'Human revision.', '--new', 'Verified change.')
        assert data('read', reference)['content'] == after.replace('Human revision.', 'Verified change.')
        print(json.dumps({'product': 'mdt', 'version': version, 'runner': str(args.runner) if args.runner else 'native',
                          'checks': ['help', 'version', 'exact CRLF/unicode read', 'deleted and changed text diff',
                                     'retained revision read', 'stale edit refusal', 'guarded replacement']}, indent=2))


if __name__ == '__main__':
    main()
