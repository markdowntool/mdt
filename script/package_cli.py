#!/usr/bin/env python3
"""Verify and archive four CLI builds; write an independent website manifest.

No download-host uploads, website edits, installs or GUI builds. Explicit release
signing submits binaries to Apple's notary service. Release packaging requires a
clean committed checkout; --allow-dirty writes no publishable manifests.
"""
import argparse
import gzip
import hashlib
import io
import json
import os
from pathlib import Path
import platform as host_platform
import re
import shutil
import struct
import subprocess
import tarfile
import tempfile
from urllib.parse import urlparse

ROOT = Path(__file__).resolve().parent.parent
TARGETS = {
    'macos-arm64': 'aarch64-apple-darwin',
    'macos-x86_64': 'x86_64-apple-darwin',
    'linux-arm64': 'aarch64-unknown-linux-musl',
    'linux-x86_64': 'x86_64-unknown-linux-musl',
}


def sha256(path):
    with Path(path).open('rb') as stream:
        return hashlib.file_digest(stream, 'sha256').hexdigest()


def inspect_binary(path, platform):
    """Reject wrong architectures and Linux executables needing shared libraries."""
    data = Path(path).read_bytes()
    if platform.startswith('macos-'):
        expected = 0x0100000C if platform == 'macos-arm64' else 0x01000007
        if len(data) < 32 or data[:4] != b'\xcf\xfa\xed\xfe' or struct.unpack_from('<I', data, 4)[0] != expected:
            raise ValueError(f'{path}: not a thin {platform} Mach-O executable')
        if struct.unpack_from('<I', data, 12)[0] != 2:
            raise ValueError(f'{path}: Mach-O is not an executable')
        return {'format': 'Mach-O', 'architecture': platform.split('-', 1)[1]}
    expected = 183 if platform == 'linux-arm64' else 62
    if len(data) < 64 or data[:6] != b'\x7fELF\x02\x01' or struct.unpack_from('<H', data, 18)[0] != expected:
        raise ValueError(f'{path}: not a 64-bit little-endian {platform} ELF executable')
    if struct.unpack_from('<H', data, 16)[0] not in (2, 3):
        raise ValueError(f'{path}: ELF is not executable')
    phoff = struct.unpack_from('<Q', data, 32)[0]
    phsize, phnum = struct.unpack_from('<HH', data, 54)
    if phsize < 56 or phnum == 0 or phoff + phsize * phnum > len(data):
        raise ValueError(f'{path}: invalid ELF program headers')
    for index in range(phnum):
        start = phoff + phsize * index
        kind = struct.unpack_from('<I', data, start)[0]
        if kind == 3:  # PT_INTERP: dynamic loader
            raise ValueError(f'{path}: Linux CLI must not need a dynamic interpreter')
        if kind == 2:  # PT_DYNAMIC, which static PIE may also have
            offset, size = struct.unpack_from('<Q', data, start + 8)[0], struct.unpack_from('<Q', data, start + 32)[0]
            if offset + size > len(data) or size % 16:
                raise ValueError(f'{path}: invalid ELF dynamic segment')
            for position in range(offset, offset + size, 16):
                tag = struct.unpack_from('<Q', data, position)[0]
                if tag == 1:  # DT_NEEDED
                    raise ValueError(f'{path}: Linux CLI must not need a shared library')
                if tag == 0:
                    break
    return {'format': 'ELF', 'architecture': platform.split('-', 1)[1], 'linkage': 'static', 'libc': 'musl'}


def write_archive(path, entries, timestamp):
    """Stable bytes and safe flat member names; preserve only executable modes."""
    for name, _, _ in entries:
        if Path(name).name != name or '\\' in name or name in ('.', '..'):
            raise ValueError('Archive member names must be plain filenames')
    with path.open('wb') as output, gzip.GzipFile(filename='', mode='wb', fileobj=output, mtime=timestamp) as compressed:
        with tarfile.open(fileobj=compressed, mode='w', format=tarfile.USTAR_FORMAT) as archive:
            for name, data, mode in entries:
                member = tarfile.TarInfo(name)
                member.size, member.mode, member.mtime = len(data), mode, timestamp
                archive.addfile(member, io.BytesIO(data))


def distribution_entries(root):
    """Include the exact project license, dependency notices and manual."""
    return [(name, (root / path).read_bytes(), 0o644) for name, path in (
        ('LICENSE', 'LICENSE'), ('THIRD_PARTY_NOTICES.txt', 'THIRD_PARTY_NOTICES.txt'),
        ('mdt.1', 'docs/mdt.1'),
    )]


def run(*args, **kwargs):
    return subprocess.check_output(args, cwd=ROOT, text=True, **kwargs).strip()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--version', required=True)
    parser.add_argument('--artifact-base-url', default='https://emm-artifacts.t3.tigrisfiles.io')
    parser.add_argument('--output', type=Path, default=ROOT / 'dist' / 'cli')
    parser.add_argument('--platform', action='append', choices=TARGETS, help='Subset for local verification; publication requires all four')
    parser.add_argument('--allow-dirty', action='store_true', help='Local only: do not create release manifests')
    parser.add_argument('--sign-identity', help='Explicit Developer ID identity for release signing')
    parser.add_argument('--notary-profile', help='Explicit existing keychain profile for Apple notarization')
    parser.add_argument('--linux-runner', action='append', default=[], metavar='PLATFORM=EXECUTABLE',
                        help='Optional emulator for Linux execution smoke tests; recorded separately from native execution')
    args = parser.parse_args()
    product = 'mdt'
    documentation = distribution_entries(ROOT)
    if Path(run('git', 'rev-parse', '--show-toplevel')).resolve() != ROOT.resolve():
        parser.error('Package from the mdt repository itself, not a parent repository')
    if not re.fullmatch(r'[A-Za-z0-9][A-Za-z0-9._-]*', args.version):
        parser.error('--version must be safe in a filename')
    base = urlparse(args.artifact_base_url)
    if base.scheme != 'https' or not base.netloc or base.username or base.password or base.query or base.fragment:
        parser.error('--artifact-base-url must be a public HTTPS URL without credentials/query/fragment')
    platforms = args.platform or list(TARGETS)
    runners = {}
    for runner in args.linux_runner:
        platform, separator, executable = runner.partition('=')
        if platform not in ('linux-arm64', 'linux-x86_64') or not separator or not Path(executable).is_file():
            parser.error('--linux-runner needs linux-arm64|linux-x86_64 and an existing executable path')
        runners[platform] = str(Path(executable).resolve())
    dirty = bool(run('git', 'status', '--porcelain', '--untracked-files=normal'))
    if dirty and not args.allow_dirty:
        parser.error('Release packaging requires a clean committed checkout')
    if not args.allow_dirty and set(platforms) != set(TARGETS):
        parser.error('Publishable manifests require all four platforms')
    if not args.allow_dirty and (not args.sign_identity or not args.notary_profile):
        parser.error('Release macOS archives require --sign-identity and --notary-profile')
    commit = run('git', 'rev-parse', 'HEAD')
    timestamp = int(run('git', 'log', '-1', '--format=%ct'))
    args.output.mkdir(parents=True, exist_ok=True)
    manifest = {'schema_version': 1, 'product': product, 'version': args.version,
                'source_commit': commit, 'source_commit_timestamp': timestamp, 'artifacts': {}}
    reports = []
    for platform in platforms:
        executable_name = product
        source = ROOT / 'target' / TARGETS[platform] / 'release' / executable_name
        details = inspect_binary(source, platform)
        if commit[:10].encode() not in source.read_bytes():
            raise ValueError(f'{source}: binary does not contain the source commit; rebuild it first')
        with tempfile.TemporaryDirectory(prefix='cli-package-', dir=args.output) as temp:
            binary = Path(temp) / executable_name
            shutil.copy2(source, binary)
            details.update({'product': product, 'platform': platform, 'target': TARGETS[platform],
                            'source_commit': commit, 'source_dirty': dirty, 'executed': False})
            if platform.startswith('macos-'):
                libraries = run('otool', '-L', str(binary)).splitlines()[1:]
                details['system_libraries'] = [line.strip().split(' (', 1)[0] for line in libraries]
                if any(not name.startswith(('/usr/lib/', '/System/Library/')) for name in details['system_libraries']):
                    raise ValueError(f'{source}: depends on a non-system macOS library')
                load_commands = run('otool', '-l', str(binary))
                minimum = re.search(r'\bminos\s+(\S+)', load_commands)
                if not minimum:
                    minimum = re.search(r'LC_VERSION_MIN_MACOSX\s+cmdsize\s+\d+\s+version\s+(\S+)', load_commands)
                details['minimum_macos'] = minimum.group(1) if minimum else 'unknown'
                if args.sign_identity and args.notary_profile:
                    subprocess.run(['codesign', '--force', '--options', 'runtime', '--timestamp', '--sign', args.sign_identity, str(binary)], check=True)
                    subprocess.run(['codesign', '--verify', '--strict', str(binary)], check=True)
                    submission = Path(temp) / f'{product}.zip'
                    subprocess.run(['ditto', '-c', '-k', '--keepParent', str(binary), str(submission)], check=True)
                    accepted = json.loads(run('xcrun', 'notarytool', 'submit', str(submission), '--keychain-profile', args.notary_profile, '--wait', '--output-format', 'json'))
                    if accepted.get('status') != 'Accepted':
                        raise ValueError(f'Apple did not accept {product} {platform}: {accepted}')
                    details['notarization_id'] = accepted['id']
                    details['signed_and_notarized'] = True
                else:
                    details['signed_and_notarized'] = False
            prefix = None
            if platform.startswith('macos-'):
                prefix = ['arch', '-x86_64'] if platform == 'macos-x86_64' else []
                details['execution_method'] = 'Rosetta or Intel host' if prefix else 'native macOS'
            elif platform in runners:
                prefix = [runners[platform]]
                details['execution_method'] = 'emulator: ' + runners[platform]
            elif host_platform.system() == 'Linux' and host_platform.machine() in (
                ('aarch64', 'arm64') if platform == 'linux-arm64' else ('x86_64', 'amd64')
            ):
                prefix = []
                details['execution_method'] = 'native Linux'
            if prefix is not None:
                with tempfile.TemporaryDirectory(prefix='cli-smoke-') as home:
                    env = os.environ | {'HOME': home, 'QM_STATE_DIR': home}
                    details['version_output'] = run(*prefix, str(binary), '--version', env=env)
                    if commit[:10] not in details['version_output'] or ('dirty' in details['version_output'] and not args.allow_dirty):
                        raise ValueError(f'{source}: binary version does not match clean source {commit}')
                    run(*prefix, str(binary), '--help', env=env)
                details['executed'] = True
            details['binary_sha256'] = sha256(binary)
            installation = (
                f'Run: ./{product} --help\n'
                f'Install: mkdir -p "$HOME/.local/bin" && install -m 755 {product} "$HOME/.local/bin/{product}"\n'
                'Add $HOME/.local/bin to your shell PATH if needed.\n'
            )
            trust = ''
            if platform.startswith('macos-'):
                trust = (
                    'This executable is Developer ID signed and notarized. A bare executable\n'
                    'cannot carry a stapled ticket; first Gatekeeper verification needs internet access.\n'
                    if details['signed_and_notarized'] else
                    'This is an unsigned local verification build, not a published release.\n'
                )
            readme = (f'{product} CLI {args.version} ({commit}) — {platform}\n\n'
                      + installation +
                      'No desktop app is required.\n'
                      'Linux binaries are statically linked with musl and SQLite.\n'
                      'The MIT license and third-party notices are included.\n'
                      'The feedback command also requires curl on PATH.\n'
                      + trust +
                      'See build-info.json for exact verification and signing results for this archive.\n')
            entries = [(executable_name, binary.read_bytes(), 0o755), ('README.txt', readme.encode(), 0o644),
                       ('build-info.json', (json.dumps(details, indent=2) + '\n').encode(), 0o644)]
            entries.extend(documentation)
            extension = 'tar.gz'
            filename = f'{product}-cli-{args.version}-{commit[:10]}-{platform}.{extension}'
            archive = args.output / filename
            write_archive(archive, entries, timestamp)
            checksum = sha256(archive)
            key = f'{product}/{product}-cli-{args.version}-{commit[:10]}-{checksum}-{platform}.{extension}'
            manifest['artifacts'][platform] = {'artifact_url': args.artifact_base_url.rstrip('/') + '/' + key, 'sha256': checksum}
            reports.append(details | {'archive': str(archive.resolve()), 'sha256': checksum, 'upload_key': key})
            print(f'{platform} {product}: {archive} ({checksum})')
    report = args.output / 'mdt-verification.json'
    report.write_text(json.dumps(reports, indent=2) + '\n')
    if not args.allow_dirty:
        (args.output / 'mdt-cli.json').write_text(json.dumps(manifest, indent=2) + '\n')
    else:
        print('Local verification only; no publishable release manifests written.')


if __name__ == '__main__':
    main()
