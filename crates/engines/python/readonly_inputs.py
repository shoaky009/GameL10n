"""Reject ordinary Python writes beneath an operation's input directory."""


def protect_inputs(directory):
    if not directory:
        return
    import os
    import sys
    from pathlib import Path

    roots = [Path(path).resolve() for path in (directory if isinstance(directory, list) else [directory])]
    write_flags = os.O_WRONLY | os.O_RDWR | os.O_CREAT | os.O_TRUNC | os.O_APPEND

    def check(path):
        if isinstance(path, (str, bytes, os.PathLike)):
            target = Path(os.fsdecode(path)).resolve()
            if any(target == root or target.is_relative_to(root) for root in roots):
                raise PermissionError(f'Input resources are read-only: {target}')

    def audit(event, args):
        if event == 'open' and isinstance(args[2], int) and args[2] & write_flags:
            check(args[0])
        elif event in ('os.remove', 'os.rmdir', 'os.mkdir', 'os.chmod', 'os.truncate', 'os.utime'):
            check(args[0])
        elif event in ('os.rename', 'os.link'):
            check(args[0])
            check(args[1])
        elif event == 'os.symlink':
            check(args[1])

    sys.addaudithook(audit)
