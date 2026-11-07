# Offline check of the 9 global patterns: one hit in each exe; the rip target (or hit + n) equals
# the Vulkan constant; in DX11 the target is in the right kind of section and a vtable's first entry
# points into code. Read-only.
import sys, struct, re
sys.path.insert(0, __import__("os").path.dirname(__import__("os").path.abspath(__file__)))  # pe.py sits beside this script
from pe import PE

D = r"C:\Program Files (x86)\Steam\steamapps\common\Ghost Recon Breakpoint"
G = [
    ("body root", "48 8B 15 ?? ?? ?? ?? 33 ED", "rip", 0x063EBEB0, "data"),
    ("camera root", "48 8B 0D ?? ?? ?? ?? 48 8D 54 24 40 E8 ?? ?? ?? ?? F2 0F 10 48 18 F2 0F 10 40 10", "rip", 0x062F24B0, "data"),
    ("gear slots root", "48 8B 0D ?? ?? ?? ?? 8B 7D", "rip", 0x063EB5D0, "data"),
    ("gear level root", "48 8B 0D ?? ?? ?? ?? 48 85 C9 74 25 48 8B 01 41", "rip", 0x063A0D20, "data"),
    ("weapon table", "48 8D 05 ?? ?? ?? ?? 48 89 6C 24 38 48 8B F9 48 89 74 24 40 48 8B B1 B0", "rip", 0x04894BD0, "vt"),
    ("gear table", "48 8D 05 ?? ?? ?? ?? 48 89 07 48 8D B7 88 00 00 00 48", "rip", 0x047B5130, "vt"),
    ("npc tag", "48 8D 05 ?? ?? ?? ?? 48 89 07 48 8B C7 48 89 77 20 48 89 77 28 48 C7", "rip", 0x049BFD70, "vt"),
    ("player tag", "48 8D 05 ?? ?? ?? ?? 48 89 03 48 8D 05 ?? ?? ?? ?? 48 89 43 20 48 8D 05 ?? ?? ?? ?? 48 89 83 A0 02 00 00 48 C7", "rip", 0x04A4C5F0, "vt"),
    ("credits caller", "48 8B 42 60 FF 10", 6, 0x0273990E, "code"),
]


def regex(p):
    out = b""
    for t in p.split():
        out += b"." if t == "??" else re.escape(bytes([int(t, 16)]))
    return re.compile(out, re.S)


def hits(pe, rx):
    found = []
    for s in pe.code_secs():
        _, name, va, vsize, rawptr, rawsz, ch = s
        blob = pe.m[rawptr:rawptr + min(vsize, rawsz)]
        for m in rx.finditer(blob):
            found.append(va + m.start())
    return found


def sec_of(pe, rva):
    for s in pe.secs:
        if s[2] <= rva < s[2] + s[3]:
            return s
    return None


ok = True
for exe in ("GRB_vulkan.exe", "GRB.exe"):
    pe = PE(D + "\\" + exe)
    print("==", exe)
    for name, pat, how, want, kind in G:
        h = hits(pe, regex(pat))
        if len(h) != 1:
            print(f"  {name}: {len(h)} hits  BAD")
            ok = False
            continue
        at = h[0]
        if how == "rip":
            disp = struct.unpack_from("<i", pe.m, pe.rva2off(at + 3))[0]
            t = at + 7 + disp
        else:
            t = at + how
        s = sec_of(pe, t)
        sname = s[1].decode(errors="replace") if s else "?"
        extra = ""
        if kind == "vt":
            first = struct.unpack_from("<Q", pe.m, pe.rva2off(t))[0] - 0x140000000
            fs = sec_of(pe, first)
            extra = f" first entry rva {first:#x} in {fs[1].decode(errors='replace') if fs else '?'} ({'code' if fs and fs[6] & 0x20000000 else 'NOT code'})"
        same = ""
        if exe == "GRB_vulkan.exe":
            same = " = constant" if t == want else f" != constant {want:#x}  BAD"
            ok &= t == want
        print(f"  {name}: hit {at:#x} -> {t:#x} in {sname}{same}{extra}")
print("ALL OK" if ok else "PROBLEMS")
