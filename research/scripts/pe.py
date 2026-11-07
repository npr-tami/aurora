# Minimal read-only PE helper for GRB_vulkan.exe: sections, exception table, function lookup.
import struct, bisect, sys, mmap

EXE = r"C:\Program Files (x86)\Steam\steamapps\common\Ghost Recon Breakpoint\GRB_vulkan.exe"
BASE = 0x140000000

class PE:
    def __init__(self, path=EXE):
        f = open(path, "rb")
        self.m = mmap.mmap(f.fileno(), 0, access=mmap.ACCESS_READ)
        m = self.m
        pe = struct.unpack_from("<I", m, 0x3c)[0]
        nsec = struct.unpack_from("<H", m, pe + 6)[0]
        optsz = struct.unpack_from("<H", m, pe + 20)[0]
        opt = pe + 24
        self.datadirs = [struct.unpack_from("<II", m, opt + 112 + 8 * i) for i in range(16)]
        self.secs = []
        sh = opt + optsz
        for i in range(nsec):
            name = m[sh:sh + 8].rstrip(b"\0")
            vsize, va, rawsz, rawptr = struct.unpack_from("<IIII", m, sh + 8)
            ch = struct.unpack_from("<I", m, sh + 36)[0]
            self.secs.append((i + 1, name, va, vsize, rawptr, rawsz, ch))
            sh += 40
        # Exception table
        exc_rva, exc_sz = self.datadirs[3]
        off = self.rva2off(exc_rva)
        n = exc_sz // 12
        ents = []
        for i in range(n):
            b, e, u = struct.unpack_from("<III", m, off + 12 * i)
            ents.append((b, e, u))
        ents.sort()
        self.ents = ents
        self.starts = [x[0] for x in ents]

    def code_secs(self):
        # Executable and not writable
        return [s for s in self.secs if (s[6] & 0x20000000) and not (s[6] & 0x80000000)]

    def rva2off(self, rva):
        for (_, _, va, vs, rp, rs, _) in self.secs:
            if va <= rva < va + max(vs, rs):
                return rva - va + rp
        return None

    def read(self, rva, n):
        o = self.rva2off(rva)
        return self.m[o:o + n]

    def entry_of(self, rva):
        i = bisect.bisect_right(self.starts, rva) - 1
        if i < 0:
            return None
        b, e, u = self.ents[i]
        if b <= rva < e:
            return (b, e, u)
        return None

    def primary(self, rva):
        """Follow chained unwind info to the primary function start."""
        ent = self.entry_of(rva)
        seen = 0
        while ent and seen < 8:
            b, e, u = ent
            uo = self.rva2off(u)
            if uo is None:
                return b
            ver_flags = self.m[uo]
            flags = ver_flags >> 3
            ncodes = self.m[uo + 2]
            if flags & 0x4:  # CHAININFO
                co = uo + 4 + ((ncodes + 1) & ~1) * 2
                pb, pe_, pu = struct.unpack_from("<III", self.m, co)
                ent = (pb, pe_, pu)
                seen += 1
                continue
            return b
        return ent[0] if ent else None

    def find_all(self, pat, code_only=True):
        hits = []
        secs = self.code_secs() if code_only else self.secs
        for (_, _, va, vs, rp, rs, _) in secs:
            data = self.m[rp:rp + rs]
            i = data.find(pat)
            while i != -1:
                hits.append(va + i)
                i = data.find(pat, i + 1)
        return hits
