"""Current-user Windows DPAPI compatibility with existing desktop settings."""
import base64
import ctypes
import json
import sys
from ctypes import wintypes


class Blob(ctypes.Structure):
    _fields_ = [("size", wintypes.DWORD), ("data", ctypes.POINTER(ctypes.c_ubyte))]


def transform(operation, text):
    if sys.platform != "win32":
        raise RuntimeError("Stored credentials require Windows DPAPI")
    raw = text.encode("utf-8") if operation == "protect" else base64.b64decode(text, validate=True)
    buffer = (ctypes.c_ubyte * len(raw))(*raw)
    source = Blob(len(raw), buffer)
    output = Blob()
    crypt = ctypes.WinDLL("crypt32", use_last_error=True)
    kernel = ctypes.WinDLL("kernel32", use_last_error=True)
    method = crypt.CryptProtectData if operation == "protect" else crypt.CryptUnprotectData
    method.argtypes = [ctypes.POINTER(Blob), ctypes.c_void_p, ctypes.c_void_p, ctypes.c_void_p, ctypes.c_void_p, wintypes.DWORD, ctypes.POINTER(Blob)]
    method.restype = wintypes.BOOL
    kernel.LocalFree.argtypes = [ctypes.c_void_p]
    kernel.LocalFree.restype = ctypes.c_void_p
    try:
        if not method(ctypes.byref(source), None, None, None, None, 1, ctypes.byref(output)):
            raise RuntimeError("Cannot access stored API credential for this Windows account")
        result = ctypes.string_at(output.data, output.size)
        return base64.b64encode(result).decode("ascii") if operation == "protect" else result.decode("utf-8")
    finally:
        ctypes.memset(buffer, 0, len(raw))
        if output.data:
            ctypes.memset(output.data, 0, output.size)
            kernel.LocalFree(output.data)


if __name__ == "__main__":
    try:
        request = json.load(sys.stdin)
        if request["operation"] not in ("protect", "unprotect"):
            raise ValueError("Unknown credential operation")
        print(json.dumps({"value": transform(request["operation"], request["value"])}))
    except Exception:
        sys.stderr.write("API credential could not be read or saved; enter the key again.\n")
        sys.exit(1)
