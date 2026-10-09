"""CLI release checks reject wrong targets, shared-library Linux and unsafe archives."""
import gzip
import io
from pathlib import Path
import struct
import tarfile
import tempfile
import unittest

from package_cli import distribution_entries, inspect_binary, write_archive


def elf(machine, program_kind=1, dynamic_tag=0):
    data = bytearray(136)
    data[:6] = b'\x7fELF\x02\x01'
    struct.pack_into('<HH', data, 16, 2, machine)
    struct.pack_into('<Q', data, 32, 64)
    struct.pack_into('<HH', data, 54, 56, 1)
    struct.pack_into('<I', data, 64, program_kind)
    struct.pack_into('<Q', data, 72, 120)
    struct.pack_into('<Q', data, 96, 16)
    struct.pack_into('<Q', data, 120, dynamic_tag)
    return bytes(data)


class CliPackageTests(unittest.TestCase):
    def test_license_notices_and_manual_are_required_and_preserved(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            (root / 'docs').mkdir()
            with self.assertRaises(FileNotFoundError):
                distribution_entries(root)
            for relative, contents in [('LICENSE', b'MIT license'),
                                       ('THIRD_PARTY_NOTICES.txt', b'Third-party license text'),
                                       ('docs/mdt.1', b'manual')]:
                (root / relative).write_bytes(contents)
            entries = distribution_entries(root)
            self.assertEqual(entries, [('LICENSE', b'MIT license', 0o644),
                                       ('THIRD_PARTY_NOTICES.txt', b'Third-party license text', 0o644),
                                       ('mdt.1', b'manual', 0o644)])

    def inspect(self, data, platform):
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / 'command'
            path.write_bytes(data)
            return inspect_binary(path, platform)

    def test_linux_static_architecture_and_dynamic_dependency_refusal(self):
        for platform, machine in [('linux-arm64', 183), ('linux-x86_64', 62)]:
            with self.subTest(platform=platform):
                self.assertEqual(self.inspect(elf(machine), platform)['linkage'], 'static')
                self.assertEqual(self.inspect(elf(machine, 2), platform)['linkage'], 'static')
                with self.assertRaisesRegex(ValueError, 'interpreter'):
                    self.inspect(elf(machine, 3), platform)
                with self.assertRaisesRegex(ValueError, 'shared library'):
                    self.inspect(elf(machine, 2, 1), platform)
        with self.assertRaisesRegex(ValueError, '64-bit'):
            self.inspect(elf(62), 'linux-arm64')
        with self.assertRaises(ValueError):
            self.inspect(b'not an executable', 'linux-arm64')

    def test_macos_cpu_and_executable_type(self):
        for platform, cpu in [('macos-arm64', 0x0100000C), ('macos-x86_64', 0x01000007)]:
            data = bytearray(32)
            data[:4] = b'\xcf\xfa\xed\xfe'
            struct.pack_into('<I', data, 4, cpu)
            struct.pack_into('<I', data, 12, 2)
            self.assertEqual(self.inspect(data, platform)['format'], 'Mach-O')
            struct.pack_into('<I', data, 12, 6)
            with self.assertRaisesRegex(ValueError, 'not an executable'):
                self.inspect(data, platform)
        with self.assertRaises(ValueError):
            self.inspect(data, 'macos-arm64')

    def test_archives_retain_executable_mode_and_are_repeatable(self):
        entries = [('mdt', b'binary', 0o755), ('README.txt', b'install', 0o644)]
        with tempfile.TemporaryDirectory() as tmp:
            first, second = Path(tmp) / 'one.tar.gz', Path(tmp) / 'two.tar.gz'
            write_archive(first, entries, 100)
            write_archive(second, entries, 100)
            self.assertEqual(first.read_bytes(), second.read_bytes())
            with tarfile.open(first) as archive:
                self.assertEqual(archive.getnames(), ['mdt', 'README.txt'])
                self.assertEqual(archive.getmember('mdt').mode, 0o755)
                self.assertEqual(archive.extractfile('mdt').read(), b'binary')
            with self.assertRaises(ValueError):
                write_archive(second, [('../mdt', b'bad', 0o755)], 100)



if __name__ == '__main__':
    unittest.main()
