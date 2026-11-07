"""Read-only: PE data directories, and a sanity look at the exception table (.pdata) and RTTI."""
import struct, sys

NAMES = ["export", "import", "resource", "exception", "security", "reloc", "debug", "arch",
         "globalptr", "tls", "loadconfig", "boundimport", "iat", "delayimport", "clr", "reserved"]

def load(path):
    data = open(path, "rb").read()
    pe = struct.unpack_from("<I", data, 0x3C)[0]
    n = struct.unpack_from("<H", data, pe + 6)[0]
    opt = pe + 24
    optsize = struct.unpack_from("<H", data, pe + 20)[0]
    base = struct.unpack_from("<Q", data, opt + 24)[0]
    ndirs = struct.unpack_from("<I", data, opt + 108)[0]
    dirs = [struct.unpack_from("<II", data, opt + 112 + i * 8) for i in range(ndirs)]
    secs = []
    for i in range(n):
        name, vsize, va, rsize, rptr = struct.unpack_from("<8sIIII", data, opt + optsize + i * 40)
        secs.append((name.rstrip(b"\0").decode(), va, vsize, rptr, rsize))
    return data, base, dirs, secs

def off(secs, rva):
    for name, va, vs, rp, rs in secs:
        if va <= rva < va + max(vs, rs):
            return rp + (rva - va) if rva - va < rs else None
    return None

for path in sys.argv[1:]:
    data, base, dirs, secs = load(path)
    print("==", path.split("\\")[-1])
    for i, (rva, size) in enumerate(dirs):
        if size:
            sec = next((s[0] for s in secs if s[1] <= rva < s[1] + max(s[2], s[4])), "?")
            print(f"  {NAMES[i]:12} rva {rva:#010x} size {size:#x} in {sec}")
    rva, size = dirs[3]
    o = off(secs, rva)
    if o is not None:
        good = 0
        ents = []
        for k in range(min(size // 12, 2000)):
            b, e, u = struct.unpack_from("<III", data, o + k * 12)
            ents.append((b, e, u))
            if 0 < b < e < 0x30000000 and e - b < 0x100000:
                good += 1
        print(f"  exception table: first entries {[(hex(b), hex(e), hex(u)) for b, e, u in ents[:3]]}  plausible {good}/{len(ents)}  total {size // 12:,}")
        # Sorted check
        print(f"  sorted: {all(ents[i][0] < ents[i + 1][0] for i in range(len(ents) - 1))}")
    # RTTI: count complete object locators (x64: signature 1, pSelf == own rva)
    hits = 0
    import re
    for name, va, vs, rp, rs in secs:
        blob = data[rp: rp + rs]
        for m in re.finditer(rb"\x01\x00\x00\x00", blob):
            p = m.start()
            if p % 4 or p + 24 > len(blob):
                continue
            self_rva = struct.unpack_from("<I", blob, p + 20)[0]
            if self_rva == va + p:
                hits += 1
    print(f"  RTTI complete object locators (x64, pSelf check): {hits:,}")
