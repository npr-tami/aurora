// Decompile the functions that contain the given RVAs, in Ghidra headless mode, without a full
// auto-analysis (that takes hours on a 500 MB exe). The function start comes from the exe's own
// exception table (.pdata), following chained unwind info to the primary start, so the result is
// the whole function even when the RVA is a hook site in its middle.
//
// Run through research\ghidra\ghidra.ps1 (it sets Java, memory and the project): analyzeHeadless
// <project dir> Aurora -process GRB_vulkan.exe -noanalysis -scriptPath tools\ghidra -postScript
// AuroraDecompile.java <out file> <rva> [<rva> ...]
//
// Addresses inside the decompiled text are Ghidra's preferred VAs (image base 0x140000000 + RVA),
// not runtime addresses: subtract 0x140000000 for the RVA. Read-only towards the game: it only adds
// functions to the Ghidra project's own database. @category Aurora

import ghidra.app.decompiler.DecompInterface;
import ghidra.app.decompiler.DecompileOptions;
import ghidra.app.decompiler.DecompileResults;
import ghidra.app.script.GhidraScript;
import ghidra.program.model.address.Address;
import ghidra.program.model.listing.Function;
import ghidra.program.model.mem.Memory;
import java.io.PrintWriter;

public class AuroraDecompile extends GhidraScript {
    private Address base;
    private Memory mem;

    private long u32(long rva) throws Exception {
        return mem.getInt(base.add(rva)) & 0xffffffffL;
    }

    private int u8(long rva) throws Exception {
        return mem.getByte(base.add(rva)) & 0xff;
    }

    /** The primary start of the function holding rva (exception table, chains followed), or -1. */
    private long functionStart(long rva) throws Exception {
        long opt = u32(0x3c) + 24;
        long dir = u32(opt + 112 + 3 * 8);
        long count = u32(opt + 112 + 3 * 8 + 4) / 12;
        long lo = 0, hi = count - 1, hit = -1;
        while (lo <= hi) {
            long mid = (lo + hi) >>> 1;
            long b = u32(dir + mid * 12), e = u32(dir + mid * 12 + 4);
            if (rva < b) {
                hi = mid - 1;
            } else if (rva >= e) {
                lo = mid + 1;
            } else {
                hit = mid;
                break;
            }
        }
        if (hit < 0) {
            return -1;
        }
        long begin = u32(dir + hit * 12);
        long unwind = u32(dir + hit * 12 + 8);
        for (int guard = 0; guard < 32; guard++) {
            if ((unwind & 1) != 0) {
                // Low bit set: the field points at another RUNTIME_FUNCTION, not unwind info.
                long rf = unwind & ~1L;
                begin = u32(rf);
                unwind = u32(rf + 8);
                continue;
            }
            int flags = u8(unwind) >> 3;
            if ((flags & 4) == 0) {
                break; // no UNW_FLAG_CHAININFO: this is the primary entry
            }
            int codes = u8(unwind + 2);
            long parent = unwind + 4 + ((codes + 1) & ~1) * 2L;
            begin = u32(parent);
            unwind = u32(parent + 8);
        }
        return begin;
    }

    @Override
    public void run() throws Exception {
        String[] a = getScriptArgs();
        if (a.length < 2) {
            printerr("arguments: <out file> <rva hex> [<rva hex> ...]");
            return;
        }
        base = currentProgram.getImageBase();
        mem = currentProgram.getMemory();
        DecompInterface di = new DecompInterface();
        di.setOptions(new DecompileOptions());
        di.openProgram(currentProgram);
        try (PrintWriter out = new PrintWriter(a[0], "UTF-8")) {
            out.printf("// %s, image base %s. Addresses below are preferred VAs: RVA = VA - image base.%n",
                currentProgram.getName(), base);
            for (int i = 1; i < a.length; i++) {
                long rva = Long.decode(a[i]);
                out.printf("%n// ===== rva 0x%x%n", rva);
                long start = functionStart(rva);
                if (start < 0) {
                    out.println("// not inside any function of the exception table");
                    continue;
                }
                Address entry = base.add(start);
                Function f = getFunctionAt(entry);
                if (f == null) {
                    disassemble(entry);
                    f = createFunction(entry, String.format("sub_%x", start));
                }
                if (f == null) {
                    out.printf("// could not make a function at rva 0x%x%n", start);
                    continue;
                }
                out.printf("// function rva 0x%x, the rva is at +0x%x%n", start, rva - start);
                DecompileResults r = di.decompileFunction(f, 180, monitor);
                if (r == null || !r.decompileCompleted()) {
                    out.println("// decompile failed: " + (r == null ? "no result" : r.getErrorMessage()));
                    continue;
                }
                out.println(r.getDecompiledFunction().getC());
            }
        } finally {
            di.dispose();
        }
        println("wrote " + a[0]);
    }
}
