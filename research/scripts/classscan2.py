"""Read-only: find the player's 7 class levels stored as a group (any order), as u8/u16/u32 with a
fixed stride. Levels from the class screen: 1,1,12,1,2,14,10.
usage: classscan2.py <pid>   -> %TEMP%\\aurora-class\\scan.txt"""
import ctypes, sys, os, time
import numpy as np
sys.path.insert(0, __import__("os").path.dirname(__import__("os").path.abspath(__file__)))  # pe.py sits beside this script
import memsnap
k32 = memsnap.k32
pid = int(sys.argv[1])
h = k32.OpenProcess(0x0400 | 0x0010, False, pid)
WANT = np.array(sorted([1, 1, 12, 1, 2, 14, 10]))
LEVELS = [1, 2, 10, 12, 14]
OUT = os.path.join(os.environ.get("TEMP", "."), "aurora-class")
os.makedirs(OUT, exist_ok=True)
out = open(os.path.join(OUT, "scan.txt"), "w")
t0 = time.time(); total = 0; found = 0
CH = 0x100000
for base, size in memsnap.regions(h):
    parts = []
    for off in range(0, size, CH):
        n = min(CH, size - off)
        buf = ctypes.create_string_buffer(n); got = ctypes.c_size_t()
        ok = k32.ReadProcessMemory(h, base + off, buf, n, ctypes.byref(got)) and got.value == n
        parts.append(buf.raw if ok else bytes(n))
    raw = np.frombuffer(b"".join(parts), dtype=np.uint8)
    total += len(raw)
    for dt, w, maxs in ((np.uint8, 1, 0x40), (np.uint16, 2, 0x100), (np.uint32, 4, 0x100)):
        a = raw[: len(raw) // w * w].view(dt)
        n = len(a)
        hits = np.nonzero(a == 14)[0]
        if len(hits) == 0:
            continue
        valid = np.isin(a, LEVELS)
        for k in range(1, maxs // w + 1):
            # A group of 7 needs a level right next to the 14 (one stride away)
            near = np.zeros(len(hits), bool)
            m1 = hits >= k
            near[m1] |= valid[hits[m1] - k]
            m2 = hits + k < n
            near[m2] |= valid[hits[m2] + k]
            cand = hits[near]
            for p in range(7):
                s = cand - p * k
                s = s[(s >= 0) & (s + 6 * k < n)]
                if len(s) == 0:
                    continue
                v = np.stack([valid[s + j * k] for j in range(7)], axis=1).all(axis=1)
                s = s[v]
                if len(s) == 0:
                    continue
                vals = np.stack([a[s + j * k] for j in range(7)], axis=1).astype(np.int64)
                m = np.all(np.sort(vals, axis=1) == WANT, axis=1)
                for i in np.nonzero(m)[0]:
                    addr = base + int(s[i]) * w
                    out.write(f"{addr:#x} u{w*8} stride {k*w:#x} values {list(vals[i])}\n"); found += 1
out.close()
print(f"read {total >> 20} MB in {time.time() - t0:.0f} s, {found} groups")
