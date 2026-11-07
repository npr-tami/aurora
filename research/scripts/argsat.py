# For each call site rva given, print the instructions just before it that set rcx/rdx/r8/r9 (and
# their 32-bit forms), to see the arguments. Read-only.
import sys
sys.path.insert(0, __import__("os").path.dirname(__import__("os").path.abspath(__file__)))  # pe.py sits beside this script
from pe import PE
import capstone

p = PE()
md = capstone.Cs(capstone.CS_ARCH_X86, capstone.CS_MODE_64)
REGS = ("rcx", "ecx", "rdx", "edx", "dl", "r8", "r8d", "r9", "r9d")
for a in sys.argv[1:]:
    site = int(a, 16)
    fn = p.primary(site)
    # Decode from the function start (or 0x200 before) to the call, keep the last 14
    start = fn if fn and site - fn < 0x4000 else site - 0x200
    ins = list(md.disasm(p.read(start, site - start + 16), start))
    before = [i for i in ins if i.address < site][-14:]
    call = next((i for i in ins if i.address == site), None)
    print(f"== call at {site:#x} in fn {fn:#x}: {call.mnemonic + ' ' + call.op_str if call else '?'}")
    for i in before:
        dst = i.op_str.split(",")[0].strip()
        mark = "*" if dst in REGS else " "
        print(f"  {mark} {i.address:#x}  {i.mnemonic} {i.op_str}")
