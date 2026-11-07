"""Read-only: the NVG display object ([[[exe+0x63EBEF8]+0x2b8]+0x90]) and its overlay byte +0x299."""
import ctypes, ctypes.wintypes as wt, struct
PID, B = 17368, 0x7FF73A1B0000
k = ctypes.WinDLL("kernel32"); k.OpenProcess.restype = wt.HANDLE
k.ReadProcessMemory.argtypes = [wt.HANDLE, ctypes.c_uint64, ctypes.c_void_p, ctypes.c_size_t, ctypes.POINTER(ctypes.c_size_t)]
h = k.OpenProcess(0x410, False, PID)
def rd(a, n):
    b = ctypes.create_string_buffer(n); g = ctypes.c_size_t()
    return b.raw if k.ReadProcessMemory(h, a, b, n, ctypes.byref(g)) and g.value == n else None
def q(a):
    b = rd(a, 8); return struct.unpack("<Q", b)[0] if b else 0
g = q(B + 0x63EBEF8); p = q(g + 0x2b8) if g else 0; o = q(p + 0x90) if p else 0
print(f"global {g:#x} player-ish {p:#x} display {o:#x}")
if o:
    vt = q(o); print(f"vt exe+{vt - B:#x}  +0x299 = {rd(o + 0x299, 1)[0]}")
