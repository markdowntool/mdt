#!/usr/bin/env python3
"""Regenerate the release dependency inventory and notices from local sources.

Run after fetching Cargo.lock dependencies and installing the pinned Rust toolchain:
  python3 tools/update_notices.py --zig-root /path/to/zig-0.13.0
No dependency source or absolute local path is copied into the generated files.
"""
import argparse
import hashlib
from html.parser import HTMLParser
import json
from pathlib import Path
import re
import subprocess

ROOT = Path(__file__).resolve().parent.parent
TARGETS = (
    'aarch64-apple-darwin', 'x86_64-apple-darwin',
    'aarch64-unknown-linux-musl', 'x86_64-unknown-linux-musl',
)


def command(*args):
    return subprocess.check_output(args, cwd=ROOT, text=True).strip()


def metadata(target=None):
    args = ['cargo', 'metadata', '--locked', '--offline', '--format-version=1']
    if target:
        args.extend(['--filter-platform', target])
    return json.loads(command(*args))


def dependencies(data):
    nodes = {node['id']: node for node in data['resolve']['nodes']}
    pending, found = [data['resolve']['root']], set()
    while pending:
        identifier = pending.pop()
        if identifier in found:
            continue
        found.add(identifier)
        for dep in nodes[identifier]['deps']:
            if any(kind['kind'] != 'dev' for kind in dep['dep_kinds']):
                pending.append(dep['pkg'])
    return found


def digest(data):
    return hashlib.sha256(data).hexdigest()


def choice(expression):
    if 'MIT' in expression:
        return 'MIT AND Unicode-3.0' if 'Unicode-3.0' in expression else 'MIT'
    if 'Apache-2.0' in expression:
        return 'Apache-2.0'
    if expression == 'Zlib':
        return 'Zlib'
    raise ValueError(f'Review this release dependency license: {expression}')


class PlainText(HTMLParser):
    def __init__(self):
        super().__init__()
        self.parts = []

    def handle_data(self, data):
        self.parts.append(data)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--zig-root', required=True, type=Path)
    args = parser.parse_args()
    all_packages = metadata()['packages']
    by_target = {target: dependencies(metadata(target)) for target in TARGETS}
    inventory, sections = [], []

    def section(title, source, data):
        sections.append(f'\n{"=" * 78}\n{title}\nSource: {source}\n'
                        f'SHA-256: {digest(data)}\n{"=" * 78}\n\n'
                        + data.decode('utf-8').rstrip() + '\n')

    for package in sorted(all_packages, key=lambda p: (p['name'], p['version'])):
        if package['source'] is None:
            continue
        if not package['source'].startswith('registry+'):
            raise ValueError(f'Non-registry dependency requires review: {package["name"]}')
        targets = [target for target in TARGETS if package['id'] in by_target[target]]
        entry = {
            'name': package['name'], 'version': package['version'],
            'declared_license': package['license'],
            'source': f'https://crates.io/crates/{package["name"]}/{package["version"]}',
            'release_targets': targets,
            'selected_license': choice(package['license']) if targets else None,
            'notice_files': [],
        }
        if targets:
            base = Path(package['manifest_path']).parent
            # Registry license/notice files at the package root cover these crates.
            # Native bundled SQLite is handled explicitly below; SQLCipher is disabled.
            files = sorted(p for p in base.iterdir() if p.is_file() and
                           re.match(r'^(LICENSE|COPYING|NOTICE|COPYRIGHT)([._-]|$)', p.name, re.I))
            if not files:
                raise ValueError(f'No license notice found for {package["name"]}')
            for path in files:
                data = path.read_bytes()
                entry['notice_files'].append({'file': path.name, 'sha256': digest(data)})
                section(f'{package["name"]} {package["version"]} — {path.name}', entry['source'], data)
        inventory.append(entry)

    sqlite = next(p for p in all_packages if p['name'] == 'libsqlite3-sys')
    sqlite_source = (Path(sqlite['manifest_path']).parent / 'sqlite3/sqlite3.c').read_text()
    start = sqlite_source.index('/*\n** 2001 September 15')
    end = sqlite_source.index('*/', start) + 2
    section('SQLite 3.51.3 — bundled on Linux; system library on macOS',
            'https://sqlite.org/copyright.html', sqlite_source[start:end].encode())

    toolchain = Path(command('rustc', '--print', 'sysroot'))
    rust_docs = toolchain / 'share/doc/rust'
    text = PlainText()
    text.feed((rust_docs / 'COPYRIGHT-library.html').read_text())
    section('Rust 1.98.1 standard library — complete upstream attribution inventory',
            'Rust 1.98.1 distribution: share/doc/rust/COPYRIGHT-library.html (HTML text content)',
            ''.join(text.parts).strip().encode())
    for name in ('MIT', 'Apache-2.0', 'BSD-2-Clause', 'Unicode-3.0', 'LLVM-exception'):
        section(f'Rust standard-library license text — {name}',
                f'Rust 1.98.1 distribution: share/doc/rust/licenses/{name}.txt',
                (rust_docs / f'licenses/{name}.txt').read_bytes())

    for path, name in (
        ('lib/libc/musl/COPYRIGHT', 'musl — Linux C runtime'),
        ('LICENSE', 'Zig runtime support'),
        ('lib/libunwind/LICENSE.TXT', 'LLVM libunwind runtime'),
    ):
        section(name, f'Zig 0.13.0 distribution: {path}', (args.zig_root / path).read_bytes())

    header = '''THIRD-PARTY NOTICES FOR MDT 0.1.0

mdt's first-party MIT license is in LICENSE. The following dependencies retain
their own licenses. This file conservatively includes normal/build dependencies
for all four supported targets, plus bundled runtime notices. Not every listed
component is linked into every executable. Alternative license texts are retained;
the selected Cargo license branches are recorded in docs/dependency-licenses.json.

The complete Rust upstream standard-library inventory includes other architectures
and optional components (for example, the MPL-2.0 Fortanix SGX ABI is not used on
the four supported mdt targets). Its presence here does not imply use by mdt.
Runtime inventories and original notices are intentionally preserved in full.

Generated with tools/update_notices.py from Cargo.lock and the pinned toolchain.
See docs/dependency-licenses.md for scope, provenance and update instructions.
'''
    (ROOT / 'THIRD_PARTY_NOTICES.txt').write_text(header + ''.join(sections))
    data = {
        'format': 1, 'cargo_lock_sha256': digest((ROOT / 'Cargo.lock').read_bytes()),
        'rust_toolchain': '1.98.1', 'zig_distribution': '0.13.0',
        'targets': list(TARGETS),
        'scope': 'Cargo normal/build dependency union; dev-only and other-target packages inventoried separately',
        'packages': inventory,
    }
    (ROOT / 'docs/dependency-licenses.json').write_text(json.dumps(data, indent=2) + '\n')
    print(f'Inventoried {len(inventory)} registry packages; '
          f'included notices for {sum(bool(p["release_targets"]) for p in inventory)} release dependencies.')


if __name__ == '__main__':
    main()
