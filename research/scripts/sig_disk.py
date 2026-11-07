"""Read-only: find the mod's code patterns in the on-disk exes and print their RVAs.

If the RVAs match what the mod found at runtime (exe+RVA in the logs/notes), the
on-disk code is the same as the code in memory, so static analysis of the file is valid.
"""
import re, struct, sys

PATTERNS = dict(
    FALL="48 89 5C 24 18 55 48 8D AC 24 60 FF FF FF 48 81 EC A0 01 00 00 48 8B 05 ?? ?? ?? ?? 48 33 C4 48 89 85 90 00 00 00 48 8B D9 E8",
    IN_AIR="48 8B F9 C6 44 24 20 01 48 8B 49 08 4C 8D 44 24 20 BA F1 00 00 00 48 8B 49 38 E8",
    CREDITS="48 8B 01 33 D2 4C 8B C1",
    GOD="48 8B ?? 48 8B ?? FF 90 ?? ?? ?? ?? 84 C0 0F 85 ?? ?? ?? ?? 44 38 A7",
    NVG_SET="83 F8 01 75 0C E8 ?? ?? ?? ?? C6 87 99 02 00 00 01 E8",
    NVG_LOAD="41 88 85 0C 12 00 00 45 88 BD 99 02 00 00 48 83 C4 38",
    SIGHT="F3 44 0F 10 79 3C 48 85 D2 74 17 80 BA D0 00 00 00 00",
    FILL="F3 0F 10 81 2C 02 00 00 0F 2F 81 50 02 00 00 0F 83 ?? ?? ?? ?? 48 8B 81 08 02 00 00",
    CLAMP="F3 0F 10 83 2C 02 00 00 F3 41 0F 5F C0 F3 0F 5D 05 ?? ?? ?? ?? F3 0F 11 83 2C 02 00 00",
    POSE_FN="48 89 5C 24 08 57 48 83 EC 30 49 8B 80 40 02 00 00 49 8B B8 48 02 00 00 4D 8B 81 48 02 00 00",
    RECOIL="48 8B C4 55 41 55 48 81 EC ?? ?? ?? ?? 49 83 78 ?? 00 4D",
    AMMO="48 89 5C 24 ?? 57 48 83 EC ?? 8B DA 48 8B F9 E8 ?? ?? ?? ?? 83 78 ?? 02 0F 84",
    STAMINA="80 BF F1 01 00 00 00 75 ?? F3 0F 10 8F ?? ?? ?? ?? 0F 57 C0 0F 2F C8 76",
    DAMAGE="83 BF A8 00 00 00 06 48 8B CE 48 8B 06 F3 48 0F 2C E8",
    FIRE="40 53 48 83 EC 20 48 8B D9 48 8B 49 10 48 8B 49 48 E8 ?? ?? ?? ?? 48 8B 43 10 F3 0F 58 40 38 F3 0F 11 43 18",
)

def to_regex(p):
    return re.compile(b"".join(b"." if t == "??" else re.escape(bytes([int(t, 16)])) for t in p.split()), re.S)

def sections(data):
    pe = struct.unpack_from("<I", data, 0x3C)[0]
    n = struct.unpack_from("<H", data, pe + 6)[0]
    opt = pe + 24
    sec = opt + struct.unpack_from("<H", data, pe + 20)[0]
    out = []
    for i in range(n):
        name, vsize, va, rsize, rptr = struct.unpack_from("<8sIIII", data, sec + i * 40)
        out.append((name.rstrip(b"\0").decode(), va, vsize, rptr, rsize))
    return out

def rva_of(secs, off):
    for name, va, vs, rp, rs in secs:
        if rp <= off < rp + rs:
            return name, va + (off - rp)
    return "?", None

for path in sys.argv[1:]:
    data = open(path, "rb").read()
    secs = sections(data)
    print(f"== {path.split(chr(92))[-1]}")
    for k, p in PATTERNS.items():
        hits = [m.start() for m in to_regex(p).finditer(data)]
        shown = ", ".join(f"{rva_of(secs, h)[0]}:{rva_of(secs, h)[1]:#x}" for h in hits[:4])
        print(f"  {k:9} hits {len(hits):3}  {shown}")
