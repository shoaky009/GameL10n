"""Temporary-directory API and cleanup contracts for isolated Python."""
from pathlib import Path
import ntpath
import os
import runpy
import tempfile
import unittest
from unittest.mock import patch

ISOLATION = runpy.run_path(str(Path(__file__).with_name('isolation.py')))
MKDTEMP = ISOLATION['inherited_mkdtemp']


class IsolatedTemporaryDirectoryTest(unittest.TestCase):
    def test_temporary_directory_preserves_names_and_cleans_after_failure(self):
        with tempfile.TemporaryDirectory() as root:
            with patch.object(tempfile, 'mkdtemp', MKDTEMP), self.assertRaisesRegex(ValueError, 'failed'):
                with tempfile.TemporaryDirectory(prefix='probe-', suffix='-output', dir=Path(root)) as name:
                    directory = Path(name)
                    self.assertTrue(directory.is_absolute(), directory)
                    self.assertTrue(directory.parent.samefile(root),
                                    f'Expected parent directory {root}, got {directory.parent}')
                    self.assertTrue(directory.name.startswith('probe-') and directory.name.endswith('-output'))
                    (directory / 'sample.bin').write_bytes(b'sample')
                    raise ValueError('failed')
            self.assertFalse(directory.exists())

    def test_bytes_paths_preserve_the_return_type(self):
        with tempfile.TemporaryDirectory() as root:
            name = MKDTEMP(prefix=b'probe-', suffix=b'-output', dir=root.encode())
            self.assertIsInstance(name, bytes)
            self.assertTrue(Path(name.decode()).is_dir())


class InputPathTest(unittest.TestCase):
    def test_windows_input_paths_normalize_extended_prefixes_and_case(self):
        normalize = ISOLATION['normalize_input_path']
        for original, expected in [
            (r'\\server\share\Game\script.ks', r'\\server\share\game\script.ks'),
            (r'\\?\UNC\server\share\Game\script.ks', r'\\server\share\game\script.ks'),
            (r'\\?\unc\SERVER\SHARE\Game\script.ks', r'\\server\share\game\script.ks'),
            (r'\\?\C:\Game\script.ks', r'c:\game\script.ks'),
            (r'C:\Game\script.ks', r'c:\game\script.ks'),
        ]:
            with self.subTest(path=original), patch.object(os, 'path', ntpath):
                self.assertEqual(normalize(original), expected)
                self.assertEqual(normalize(os.fsencode(original)), expected)


if __name__ == '__main__':
    unittest.main()
