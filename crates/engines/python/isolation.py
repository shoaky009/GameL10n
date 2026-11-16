"""Temporary directories inside AppContainer inherit the authorized parent's ACL."""
import errno
import os
import sys
import tempfile
import uuid


def inherited_mkdtemp(suffix=None, prefix=None, dir=None):
    suffix, prefix, directory, output_type = tempfile._sanitize_params(suffix, prefix, dir)
    for _ in range(tempfile.TMP_MAX):
        name = uuid.uuid4().hex
        if output_type is bytes:
            name = name.encode('ascii')
        path = os.path.join(directory, prefix + name + suffix)
        sys.audit('tempfile.mkdtemp', path)
        try:
            os.mkdir(path, 0o777 if os.name == 'nt' else 0o700)
        except FileExistsError:
            continue
        return os.path.abspath(path)
    raise FileExistsError(errno.EEXIST, 'No usable temporary directory name found')


def configure_tempfiles(enabled):
    # Python 3.13+ mode 0o700 installs a DACL without the AppContainer SID.
    if enabled and os.name == 'nt':
        tempfile.mkdtemp = inherited_mkdtemp
