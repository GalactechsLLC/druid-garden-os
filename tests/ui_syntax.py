"""Check native UI JavaScript using JavaScriptCore, without Node or execution."""
import ctypes
import ctypes.util
import os
from pathlib import Path
import re

os.environ['GIGACAGE_ENABLED'] = '0'
library = ctypes.util.find_library('javascriptcoregtk-4.1')
if not library:
    raise SystemExit('JavaScriptCore GTK 4.1 is required for this optional syntax check')
js = ctypes.CDLL(library)
pointer = ctypes.c_void_p
js.JSGlobalContextCreate.argtypes = [pointer]
js.JSGlobalContextCreate.restype = pointer
js.JSStringCreateWithUTF8CString.argtypes = [ctypes.c_char_p]
js.JSStringCreateWithUTF8CString.restype = pointer
js.JSCheckScriptSyntax.argtypes = [pointer, pointer, pointer, ctypes.c_int, ctypes.POINTER(pointer)]
js.JSCheckScriptSyntax.restype = ctypes.c_bool
js.JSStringRelease.argtypes = [pointer]
js.JSGlobalContextRelease.argtypes = [pointer]
context = js.JSGlobalContextCreate(None)
public = Path(__file__).resolve().parents[1] / 'druid-garden-os-ui' / 'public'
try:
    for path in sorted(public.glob('*.js')):
        source = path.read_text()
        # Resolve the simple static module imports separately, then check the
        # complete body in an async function to preserve top-level await syntax.
        for imported in re.findall(r"^import .*? from ['\"](.+?)['\"];", source, re.M):
            assert (path.parent / imported).is_file(), imported
        source = re.sub(r'^import .*?;\s*$', '', source, flags=re.M)
        source = re.sub(r'^export (?=function )', '', source, flags=re.M)
        script = js.JSStringCreateWithUTF8CString(('async function moduleBody() {\n' + source + '\n}').encode())
        error = pointer()
        valid = js.JSCheckScriptSyntax(context, script, None, 1, ctypes.byref(error))
        js.JSStringRelease(script)
        assert valid, f'JavaScript syntax error in {path.name}'
        print(f'PASS: {path.name} syntax and static imports')
finally:
    js.JSGlobalContextRelease(context)
