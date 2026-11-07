# Find code that uses the stimulus-type enum hash 0xADEB13F6 as an immediate.
import sys, struct
sys.path.insert(0, __import__("os").path.dirname(__import__("os").path.abspath(__file__)))  # pe.py sits beside this script
from pe import PE
import capstone

p = PE()
md = capstone.Cs(capstone.CS_ARCH_X86, capstone.CS_MODE_64)
val = int(sys.argv[1], 16) if len(sys.argv) > 1 else 0xADEB13F6
hits = p.find_all(struct.pack("<I", val))
print(f"{len(hits)} raw hits of {val:#x} in code")
by_fn = {}
for h in hits:
    fn = p.primary(h)
    # Decode a bit before the hit to see the instruction
    ok = None
    for back in range(1, 12):
        start = h - back
        code = p.read(start, 16)
        for ins in md.disasm(code, start, 1):
            if ins.address == start and start < h < start + ins.size:
                ok = ins
        if ok:
            break
    txt = f"{ok.mnemonic} {ok.op_str}" if ok else "?"
    by_fn.setdefault(fn, []).append((h, txt))
for fn in sorted(by_fn, key=lambda x: x or 0):
    for h, t in by_fn[fn]:
        print(f"fn {fn:#x}  at {h:#x}  {t}" if fn else f"fn ?  at {h:#x}  {t}")
