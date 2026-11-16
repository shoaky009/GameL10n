"""Temporary-directory API and cleanup contracts for isolated Python."""
from pathlib import Path
import runpy
import tempfile
import unittest
from unittest.mock import patch

MKDTEMP = runpy.run_path(str(Path(__file__).with_name('isolation.py')))['inherited_mkdtemp']


class IsolatedTemporaryDirectoryTest(unittest.TestCase):
    def test_temporary_directory_preserves_names_and_cleans_after_failure(self):
        with tempfile.TemporaryDirectory() as root:
            with patch.object(tempfile, 'mkdtemp', MKDTEMP), self.assertRaisesRegex(ValueError, 'failed'):
                with tempfile.TemporaryDirectory(prefix='probe-', suffix='-output', dir=Path(root)) as name:
                    directory = Path(name)
                    self.assertEqual(directory.parent, Path(root).resolve())
                    self.assertTrue(directory.name.startswith('probe-') and directory.name.endswith('-output'))
                    (directory / 'sample.bin').write_bytes(b'sample')
                    raise ValueError('failed')
            self.assertFalse(directory.exists())

    def test_bytes_paths_preserve_the_return_type(self):
        with tempfile.TemporaryDirectory() as root:
            name = MKDTEMP(prefix=b'probe-', suffix=b'-output', dir=root.encode())
            self.assertIsInstance(name, bytes)
            self.assertTrue(Path(name.decode()).is_dir())


if __name__ == '__main__':
    unittest.main()
