import json, subprocess, sys
import os
exe = os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", "..", "target", "release", "grbstatic.exe")
G = r"C:\Program Files (x86)\Steam\steamapps\common\Ghost Recon Breakpoint"
V, D = G + r"\GRB_vulkan.exe", G + r"\GRB.exe"
out = os.path.join(os.environ.get("TEMP", "."), "grbstatic-out")
rv = set()
for c in sys.argv[1:]:
    subprocess.run([exe, "--out", out, "--json", "vtable", V, c], capture_output=True)
    safe = "".join(ch if ch.isalnum() else "_" for ch in c)[:60]
    d = json.load(open(out + r"\GRB_vulkan-64ff3734\vtable-" + safe + ".json"))
    for vt in d["vtables"]:
        for s in vt["slots"]:
            rv.add(s["function_rva"])
rv = sorted(rv)
print(len(rv), "slot targets")
r = subprocess.run([exe, "compare", V, D] + rv, capture_output=True, text=True)
rows = r.stdout.splitlines()[2:]
from collections import Counter
print(Counter(" ".join(x.split()[1:3]) for x in rows))
for x in rows:
    if "unique signature" not in x:
        print(x[:200])
print(r.stderr[:500])
