import struct, capstone

IDX_AT = 0x400
RING_AT = 0x800
READS_AT = 0x80
TSC_AT = 0xF8
HW = [0, 3, 1, 2, 6, 7, 5, 4, 8, 9, 10, 11, 12, 13, 14, 15]
REG_NAMES = ["rax","rbx","rcx","rdx","rsi","rdi","rbp","rsp","r8","r9","r10","r11","r12","r13","r14","r15"]

def rel(frm, to):
    return struct.pack("<i", to - frm)

def cave_code(cave, at, moved, ring, reads):
    c = bytearray([0x50, 0x41, 0x53, 0x9C])
    c += bytes([0xB8, 1, 0, 0, 0, 0xF0, 0x0F, 0xC1, 0x05])
    c += rel(cave + len(c) + 4, cave + IDX_AT)
    c.append(0x25)
    c += struct.pack("<I", ring - 1)
    c += bytes([0xC1, 0xE0, 0x08])
    c += bytes([0x4C, 0x8D, 0x1D])
    c += rel(cave + len(c) + 4, cave + RING_AT)
    c += bytes([0x4C, 0x01, 0xD8])
    c += bytes([0x4C, 0x8B, 0x5C, 0x24, 0x10, 0x4C, 0x89, 0x58, 0x00])
    c += bytes([0x4C, 0x8B, 0x5C, 0x24, 0x08, 0x4C, 0x89, 0x58, 0x58])
    c += bytes([0x4C, 0x8D, 0x5C, 0x24, 0x18, 0x4C, 0x89, 0x58, 0x38])
    for slot, hw in enumerate(HW):
        if slot in (0, 7, 11):
            continue
        c += bytes([0x4C if hw >= 8 else 0x48, 0x89, 0x40 | ((hw & 7) << 3), slot * 8])
    for i, (reg, off) in enumerate(reads):
        c += bytes([0x4C, 0x8B, 0x58, reg * 8])
        c += bytes([0x4D, 0x8B, 0x9B])
        c += struct.pack("<i", off)
        c += bytes([0x4C, 0x89, 0x98])
        c += struct.pack("<I", READS_AT + i * 8)
    c += bytes([0x49, 0x89, 0xC3])
    c.append(0x52)
    c += bytes([0x0F, 0x31])
    c += bytes([0x48, 0xC1, 0xE2, 0x20])
    c += bytes([0x48, 0x09, 0xD0])
    c += bytes([0x49, 0x89, 0x83])
    c += struct.pack("<I", TSC_AT)
    c.append(0x5A)
    c += bytes([0x9D, 0x41, 0x5B, 0x58])
    c += moved
    c.append(0xE9)
    c += rel(cave + len(c) + 4, at + len(moved))
    return bytes(c)

def reg(n):
    return REG_NAMES.index(n)

cave = 0x7ff6_0000_0000
at = 0x7ff6_1234_5678
inbox = bytes([0x48, 0x83, 0xEC, 0x28, 0xF6, 0x81, 0xC0, 0x02, 0x00, 0x00, 0x10])
noise = bytes([0x48, 0x89, 0x5C, 0x24, 0x10])
md = capstone.Cs(capstone.CS_ARCH_X86, capstone.CS_MODE_64)
for name, moved, reads in [
    ("inbox", inbox, [(reg("rsp"), 0), (reg("rdx"), 0), (reg("rdx"), 8)]),
    ("noise", noise, [(reg("rsp"), 0), (reg("rcx"), 0), (reg("rdx"), 0), (reg("rsp"), 0x28), (reg("rsp"), 0x30), (reg("rsp"), 0x38), (reg("rsp"), 0x40)]),
]:
    code = cave_code(cave, at, moved, 16384, reads)
    print(f"== {name}: {len(code)} bytes")
    n = 0
    for ins in md.disasm(code, cave):
        print(f"  +{ins.address - cave:03x}: {ins.bytes.hex():24} {ins.mnemonic} {ins.op_str}")
        n += ins.size
    print("decoded", n, "of", len(code))
# Max length
code = cave_code(cave, at, bytes([0x90]*32), 16384, [(15, i) for i in range(15)])
print("max len", hex(len(code)))
