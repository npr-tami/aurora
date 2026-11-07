# Find instructions whose text matches a regex, by scanning raw byte candidates then decoding.
# Usage: insfind.py <hex bytes needle, with ?? wildcards not supported> <regex on "mnemonic op_str">
import sys, re
sys.path.insert(0, __import__("os").path.dirname(__import__("os").path.abspath(__file__)))  # pe.py sits beside this script
from pe import PE
import capstone

needle = bytes.fromhex(sys.argv[1])
rx = re.compile(sys.argv[2])
back = int(sys.argv[3]) if len(sys.argv) > 3 else 4
p = PE()
md = capstone.Cs(capstone.CS_ARCH_X86, capstone.CS_MODE_64)
out = {}
for h in p.find_all(needle):
    for b in range(0, back + 1):
        st = h - b
        for ins in md.disasm(p.read(st, 15), st, 1):
            t = f"{ins.mnemonic} {ins.op_str}"
            if ins.address == st and st + ins.size > h and rx.search(t):
                fn = p.primary(st)
                out.setdefault(fn, []).append((st, t))
for fn in sorted(out, key=lambda x: x or 0):
    for st, t in out[fn]:
        print(f"fn {fn:#x}  at {st:#x}  {t}" if fn else f"fn ?  at {st:#x}  {t}")
print(len(out), "functions")
