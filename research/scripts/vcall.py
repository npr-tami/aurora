# Find functions that make a virtual call through slot offset OFF (e.g. 0xe8 = slot 29), restricted
# to an rva range. Read-only.
import sys, struct
sys.path.insert(0, __import__("os").path.dirname(__import__("os").path.abspath(__file__)))  # pe.py sits beside this script
from pe import PE
import capstone

off = int(sys.argv[1], 16)
lo = int(sys.argv[2], 16)
hi = int(sys.argv[3], 16)
p = PE()
md = capstone.Cs(capstone.CS_ARCH_X86, capstone.CS_MODE_64)
pats = []
d32 = struct.pack("<I", off)
for modrm in range(0x90, 0x98):  # call [reg+disp32], reg = rax..rdi (rsp needs SIB)
    if modrm == 0x94:
        continue
    pats.append(bytes([0xFF, modrm]) + d32)
hits = []
for pat in pats:
    for h in p.find_all(pat):
        if lo <= h < hi:
            hits.append(h)
            # Also REX.B form: 41 FF 9x
fns = {}
for h in sorted(hits):
    # Verify it decodes as an instruction starting at h or h-1 (REX)
    good = False
    for st in (h - 1, h):
        for ins in md.disasm(p.read(st, 15), st, 1):
            if ins.address == st and ins.mnemonic == "call" and f"0x{off:x}" in ins.op_str:
                good = True
    if not good:
        continue
    fn = p.primary(h)
    fns.setdefault(fn, []).append(h)
for fn in sorted(fns, key=lambda x: x or 0):
    print(f"{fn:#x}" if fn else "?", " ".join(f"{h:#x}" for h in fns[fn]))
print(len(fns), "functions")
