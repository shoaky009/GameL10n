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
        configure_input_handles()


def configure_input_handles():
    """Route authorized input reads to the host; all other I/O stays inside AppContainer."""
    import builtins
    import io
    import json
    import msvcrt
    import stat
    import subprocess
    import threading

    configuration = os.environ.get('GAMEL10N_INPUT_HANDLES')
    if not configuration or getattr(io, '_gamel10n_inputs', False):
        return
    with io.open(configuration, encoding='utf8') as stream:
        settings = json.load(stream)
    roots = [os.path.normcase(os.path.abspath(p)) for p in settings['roots']]
    original_io_open = io.open
    original_os_open, original_stat = os.open, os.stat
    original_lstat, original_scandir, original_listdir = os.lstat, os.scandir, os.listdir
    lock = original_io_open(settings['lock'], 'r+b', buffering=0)
    mutex = threading.Lock()
    request_fd = msvcrt.open_osfhandle(settings['request'], os.O_WRONLY | os.O_BINARY)
    response_fd = msvcrt.open_osfhandle(settings['response'], os.O_RDONLY | os.O_BINARY)

    def authorized(path):
        if not isinstance(path, (str, bytes, os.PathLike)):
            return False
        path = os.path.normcase(os.path.abspath(os.fsdecode(path)))
        if path.startswith('\\\\?\\UNC\\'):
            path = '\\\\' + path[8:]
        elif path.startswith('\\\\?\\'):
            path = path[4:]
        return any(path == root or path.startswith(root.rstrip('\\') + '\\') for root in roots)

    def query(op, path):
        # One request/response at a time across threads and inherited child processes.
        with mutex:
            lock.seek(0)
            msvcrt.locking(lock.fileno(), msvcrt.LK_LOCK, 1)
            try:
                payload = (json.dumps(dict(pid=os.getpid(), op=op, path=os.path.abspath(os.fsdecode(path))), ensure_ascii=True) + '\n').encode()
                while payload:
                    payload = payload[os.write(request_fd, payload):]
                def read_exact(size):
                    data = bytearray()
                    while len(data) < size:
                        part = os.read(response_fd, size - len(data))
                        if not part:
                            raise OSError('Read-only input channel closed')
                        data.extend(part)
                    return data
                size = int.from_bytes(read_exact(4), 'little')
                result = json.loads(read_exact(size))
            finally:
                lock.seek(0)
                msvcrt.locking(lock.fileno(), msvcrt.LK_UNLCK, 1)
        if 'error' in result:
            code = result.get('code', 5)
            kind = FileNotFoundError if code in (2, 3) else PermissionError if code == 5 else OSError
            raise kind(errno.ENOENT if code in (2, 3) else errno.EACCES, result['error'], os.fsdecode(path))
        return result['result']

    def open_fd(path, flags):
        if flags & (os.O_WRONLY | os.O_RDWR | os.O_CREAT | os.O_TRUNC | os.O_APPEND):
            raise PermissionError(errno.EACCES, 'Input resources are read-only', os.fsdecode(path))
        handle = query('open', path)['handle']
        return msvcrt.open_osfhandle(handle, flags | os.O_BINARY)

    def routed_open(file, mode='r', buffering=-1, encoding=None, errors=None, newline=None, closefd=True, opener=None):
        if not authorized(file):
            return original_io_open(file, mode, buffering, encoding, errors, newline, closefd, opener)
        if any(c in mode for c in 'wax+'):
            raise PermissionError(errno.EACCES, 'Input resources are read-only', os.fsdecode(file))
        return original_io_open(file, mode, buffering, encoding, errors, newline, closefd,
                                opener=lambda path, flags: open_fd(path, flags))

    def routed_os_open(path, flags, mode=0o777, *, dir_fd=None):
        if authorized(path):
            return open_fd(path, flags)
        return original_os_open(path, flags, mode, dir_fd=dir_fd)

    def stat_result(data):
        times = [(data[key] - 116444736000000000) * 100 for key in ('accessed', 'modified', 'created')]
        return os.stat_result((stat.S_IFDIR | 0o555 if data['directory'] else stat.S_IFREG | 0o444,
            0, 0, 1, 0, 0, data['size'], *(t / 1e9 for t in times)),
            dict(zip(('st_atime_ns', 'st_mtime_ns', 'st_ctime_ns'), times), st_file_attributes=data['attributes']))

    def routed_stat(path, *args, **kwargs):
        return stat_result(query('stat', path)) if authorized(path) else original_stat(path, *args, **kwargs)

    def routed_lstat(path, *args, **kwargs):
        return stat_result(query('stat', path)) if authorized(path) else original_lstat(path, *args, **kwargs)

    class Entry:
        def __init__(self, root, data):
            self.name = os.fsencode(data['name']) if isinstance(root, bytes) else data['name']
            self.path = os.path.join(root, self.name)
            self._stat = stat_result(data['stat'])
        def __fspath__(self): return self.path
        def stat(self, *, follow_symlinks=True): return self._stat
        def is_dir(self, *, follow_symlinks=True): return stat.S_ISDIR(self._stat.st_mode)
        def is_file(self, *, follow_symlinks=True): return stat.S_ISREG(self._stat.st_mode)
        def is_symlink(self): return False
        def inode(self): return self._stat.st_ino

    class Scan:
        def __init__(self, path): self.entries = iter(Entry(path, item) for item in query('list', path))
        def __iter__(self): return self
        def __next__(self): return next(self.entries)
        def close(self): pass
        def __enter__(self): return self
        def __exit__(self, *args): self.close()

    def routed_scandir(path='.'):
        return Scan(os.fspath(path)) if authorized(path) else original_scandir(path)

    def routed_listdir(path='.'):
        return [entry.name for entry in routed_scandir(path)] if authorized(path) else original_listdir(path)

    def predicate(original, check):
        def routed(path):
            if not authorized(path):
                return original(path)
            try:
                return check(routed_stat(path).st_mode)
            except OSError:
                return False
        return routed

    class Popen(subprocess.Popen):
        def __init__(self, *args, **kwargs):
            startup = kwargs.get('startupinfo') or subprocess.STARTUPINFO()
            startup.lpAttributeList = dict(startup.lpAttributeList or {})
            handles = list(startup.lpAttributeList.get('handle_list', []))
            handles.extend((settings['request'], settings['response']))
            startup.lpAttributeList['handle_list'] = list(dict.fromkeys(handles))
            kwargs['startupinfo'] = startup
            kwargs['close_fds'] = True
            super().__init__(*args, **kwargs)

    builtins.open = io.open = routed_open
    os.open, os.stat, os.lstat = routed_os_open, routed_stat, routed_lstat
    os.scandir, os.listdir = routed_scandir, routed_listdir
    # Python 3.14 uses native Windows predicates that bypass os.stat.
    os.path.isfile = predicate(os.path.isfile, stat.S_ISREG)
    os.path.isdir = predicate(os.path.isdir, stat.S_ISDIR)
    os.path.exists = predicate(os.path.exists, lambda mode: True)
    os.path.lexists = predicate(os.path.lexists, lambda mode: True)
    subprocess.Popen = Popen
    io._gamel10n_inputs = True
