"""Read-only first look at a PE file: header, sections, entropy, RTTI name count."""
import math, struct, sys, hashlib, collections

def entropy(b):
    if not b:
        return 0.0
    c = collections.Counter(b[: 4 << 20])  # first 4 MB of the section is enough
    n = sum(c.values())
    return -sum(v / n * math.log2(v / n) for v in c.values())

for path in sys.argv[1:]:
    data = open(path, "rb").read()
    pe = struct.unpack_from("<I", data, 0x3C)[0]
    machine, nsec, stamp = struct.unpack_from("<HHI", data, pe + 4)
    opt = pe + 24
    magic = struct.unpack_from("<H", data, opt)[0]
    entry = struct.unpack_from("<I", data, opt + 16)[0]
    imagebase = struct.unpack_from("<Q", data, opt + 24)[0]
    sizeimg = struct.unpack_from("<I", data, opt + 56)[0]
    optsize = struct.unpack_from("<H", data, pe + 20)[0]
    sec = opt + optsize
    print(f"== {path}")
    print(f"size {len(data):,}  sha256 {hashlib.sha256(data).hexdigest()[:16]}  machine {machine:#x} magic {magic:#x}")
    print(f"timestamp {stamp:#x}  imagebase {imagebase:#x}  entry rva {entry:#x}  sizeofimage {sizeimg:#x}")
    for i in range(nsec):
        name, vsize, va, rsize, rptr = struct.unpack_from("<8sIIII", data, sec + i * 40)
        chars = struct.unpack_from("<I", data, sec + i * 40 + 36)[0]
        body = data[rptr : rptr + rsize]
        print(f"  {name.rstrip(b'\0').decode(errors='replace'):10} va {va:#010x} vsize {vsize:#010x} raw {rsize:#010x} chars {chars:#010x} entropy {entropy(body):.2f}")
    print(f"  RTTI type names ('.?AV'): {data.count(b'.?AV'):,}   ('.?AU'): {data.count(b'.?AU'):,}")
    for s in [b"Denuvo", b"VMProtect", b".vmp", b"steam_api64", b"uplay_r1", b"upc_r2", b"vulkan-1.dll", b"d3d11.dll"]:
        print(f"  contains {s.decode()}: {data.count(s)}")
