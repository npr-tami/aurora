"""Read the noise recording (regs-rows.csv from grbresearch-dll, request
research/grbresearch-dll/requests/noise.request) and say, per second and in total, which
sender posted which stimulus type, and whether its source was the player.

    python research/scripts/noise_report.py <regs-rows.csv> [--bin 1000]

Callers are named from the return address (exe+rva of the instruction after the call),
using the offline map in research/docs/perception.md (Vulkan build 25349072).
Read-only: it only reads the CSV.
"""
import argparse
import collections
import re
import sys

# Call-site rva (the call instruction) -> name, for the enemy inbox (sub_29cb140).
INBOX_CALLERS = {
    0x2302B15: "sub_23026a0",
    0x240ECE0: "broadcast (sub_240ea50)",
    0x2486EDE: "30 m broadcast (sub_2486d50)",
    0x24EB9B8: "sub_24eb720",
    0x29C86CD: "sub_29c8560",
    0x2A0AD12: "sub_2a0aca0",
    0x2A21CB6: "sensor A (sub_2a21c30)",
    0x2A236B9: "sensor B (sub_2a23610)",
    0x2A2374D: "sub_2a236d0",
    0x2A26CDB: "sub_2a26b20",
    0x2A27A00: "sub_2a27910",
    0x2A2DBEF: "forward (sub_2a2dbb0)",
    0x2A2F4D6: "sub_2a2f2e0",
    0x2A326C3: "sub_2a324d0",
    0x3087EE8: "sub_3087dd0",
    0x309DA8D: "sub_309d8a0",
    0x309DBBE: "sub_309daf0",
    0x30F110F: "sub_30f0fe0",
    0x318772E: "sub_31875e0 a",
    0x31877A5: "sub_31875e0 b",
    0x31878FC: "sub_31875e0 c",
    0x3187934: "sub_31875e0 d",
    0x31DBC2D: "sub_31dbb90",
    0x31DC156: "sub_31dc120",
    0x32C55EC: "sub_32c55b0",
    0x32D1F2F: "sub_32d1e10",
    0x32E3EA5: "sub_32e3e60",
    0x3325E06: "sub_3325a40",
}
# For the noise event (sub_22411d0).
NOISE_CALLERS = {
    0x28905C7: "explosion (sub_2890490)",
    0x368E798: "weapon (sub_368e680)",
}
# Callers of the generic helper sub_2a0aca0 (read rsp+c0 at the inbox), with the type each passes
# (research/docs/perception.md, round 3).
HELPER_CALLERS = {
    0x22DF7E9: "sub_22dea20 (0x28)",
    0x2729DBC: "sub_27299f0 (0x62)",
    0x29C7D8C: "sight wrapper (sub_29c7d60)",
    0x2A2D646: "sensor dispatch (sub_2a2d380) a",
    0x2A2D660: "sensor dispatch (sub_2a2d380) b",
    0x2A2D721: "sensor dispatch (sub_2a2d380) c",
    0x2A2D800: "sensor dispatch (sub_2a2d380) d",
    0x2A44F0E: "sub_2a44e60 (0x30)",
    0x2A58FC0: "timed (sub_2a58e90) a",
    0x2A59039: "timed (sub_2a58e90) b",
    0x2AD83E9: "sub_2ad8210",
    0x2AD869A: "sub_2ad84b0",
    0x308BA64: "sub_308b9a0 (0x43)",
    0x308BAB1: "sub_308b9a0 (0x0b)",
    0x30ED404: "sub_30ed3c0 (0x54)",
    0x317D2AE: "sub_317d240",
    0x31ADA7B: "sub_31ad900",
    0x31AF5A9: "close sense (sub_31af140)",
    0x31F593B: "sub_31f5820 (0x46)",
    0x31F667B: "sub_31f6520 (0x46)",
    0x321F89A: "sub_321f660 (0x5a)",
    0x321F9E1: "sub_321f960 (0x1a)",
    0x32821A3: "sub_32820e0 (0x30)",
    0x333F34B: "sub_333f290",
    0x362F230: "sub_362f0b0 (0x2a)",
}
# Callers of the sight wrapper sub_29c7d60 (read rsp+f0). Two reach it by a tail jump, so the return
# lands after the call to them in sub_2a59050.
SIGHT_CALLERS = {
    0x2A590B8: "sight (sub_2a59050) at once",
    0x2A59205: "sight (sub_2a59050) unchecked",
    0x2A59164: "sight (sub_2a568f0, tail)",
    0x2A5919B: "sight build-up (sub_2a5e3c0, tail)",
    0x2A5E522: "sight build-up (sub_2a5e3c0)",
    0x2A569A8: "sub_2a56950 (0x12)",
}
# Callers of the broadcast sub_240ea50 (read rsp+100).
BROADCAST_CALLERS = {
    0x2458DDA: "noise handler (sub_2458cd0)",
    0x2459134: "sub_24590a0",
    0x245ED13: "sub_245eb70",
    0x24620AE: "sub_2461f70",
    0x240EDF2: "sub_240ed80",
    0x2FBD0CE: "sub_2fbcf60",
}
INBOX_COLS = ("rsp+0", "rdx+0", "rdx+8:4")
NOISE_COLS = ("rsp+0", "rcx+0", "rdx+0", "rsp+28:f", "rsp+30:4", "rsp+38:f", "rsp+40:4")

EXE_RE = re.compile(r"exe\+0x([0-9a-fA-F]+)")


def caller(cell, table):
    m = EXE_RE.search(cell)
    if not m:
        return "? " + cell.split()[0]
    ret = int(m.group(1), 16)
    # A near call is 5 bytes (E8 rel32); sub_30c8ed0 tail-jumps, so its rows name its caller.
    return table.get(ret - 5, f"exe+{ret:#x}")


def player(cell):
    return "player" if ("player's body" in cell) else "other"


def load(path):
    rows = []
    headers = {}
    with open(path, encoding="utf-8") as f:
        for line in f:
            line = line.rstrip("\n")
            if not line:
                continue
            if line.startswith("# "):
                spot, cols = line[2:].split(": ", 1)
                headers[spot] = cols.split(",")
                continue
            cells = line.split(",")
            spot = cells[0]
            cols = headers.get(spot)
            if cols is None or len(cells) != len(cols):
                print(f"skipped (no header or wrong width): {line}", file=sys.stderr)
                continue
            rows.append(dict(zip(cols, cells)))
    check_columns(headers)
    return rows


def inbox_sender(r):
    """The direct caller, or the one behind the helper / sight wrapper / broadcast."""
    name = caller(r["rsp+0"], INBOX_CALLERS)
    if name == "sub_2a0aca0" and "rsp+c0" in r:
        name = caller(r["rsp+c0"], HELPER_CALLERS)
        if name.startswith("sight wrapper") and "rsp+f0" in r:
            name = caller(r["rsp+f0"], SIGHT_CALLERS)
    elif name.startswith("broadcast") and "rsp+100" in r:
        name = "broadcast from " + caller(r["rsp+100"], BROADCAST_CALLERS)
    return name


def check_columns(headers):
    """Stop with a clear message when the CSV's reads are not the ones this script expects."""
    for spot, need in (("inbox", INBOX_COLS), ("noise", NOISE_COLS)):
        cols = headers.get(spot)
        if cols is None:
            continue
        missing = [c for c in need if c not in cols]
        if missing:
            sys.exit(f"spot {spot}: columns {missing} not in the CSV (has {cols}); "
                     "the request's reads must be spelled as in requests/noise.request")


def key(r):
    if r["spot"] == "inbox":
        return ("inbox", inbox_sender(r), "type " + r["rdx+8:4"], player(r["rdx+0"]))
    if r["spot"] == "noise":
        return (
            "noise",
            caller(r["rsp+0"], NOISE_CALLERS),
            "type " + r["rsp+40:4"],
            player(r["rcx+0"]),
            f"radius {r['rsp+38:f']} loud {r['rsp+28:f']} extra {r['rsp+30:4']}",
        )
    return (r["spot"],)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("csv")
    ap.add_argument("--bin", type=float, default=1000.0, help="timeline step in ms")
    a = ap.parse_args()
    rows = sorted(load(a.csv), key=lambda r: float(r["ms"]))
    if not rows:
        print("no rows")
        return
    total = collections.Counter(key(r) for r in rows)
    enemies = collections.defaultdict(set)
    for r in rows:
        enemies[key(r)].add(r["rcx"])
    print(f"{len(rows)} calls, {rows[0]['ms']} to {rows[-1]['ms']} ms")
    print("\n== totals (calls, different rcx)")
    for k, n in total.most_common():
        print(f"{n:6} {len(enemies[k]):4}  " + " | ".join(k))
    # "player" is worked out when the recording ends (the body then). If the player died or respawned during
    # the run, or the source is not the body pointer, the label can be wrong: the raw source values
    # and when they were seen let it be checked against what the player did.
    print("\n== inbox sources (top 12: calls, first and last ms, value as recorded)")
    seen = collections.defaultdict(list)
    for r in rows:
        if r["spot"] == "inbox":
            seen[r["rdx+0"]].append(float(r["ms"]))
    for v, ms in sorted(seen.items(), key=lambda kv: -len(kv[1]))[:12]:
        print(f"{len(ms):6}  {ms[0]:10.0f} {ms[-1]:10.0f}  {v}")
    print("\n== timeline (only the player's stimuli and every noise event)")
    bins = collections.defaultdict(collections.Counter)
    for r in rows:
        k = key(r)
        if k[0] == "noise" or (k[0] == "inbox" and k[3] == "player"):
            bins[int(float(r["ms"]) // a.bin)][k] += 1
    for b in sorted(bins):
        parts = [f"{n}x " + " ".join(k[1:]) for k, n in bins[b].most_common()]
        print(f"{b * a.bin / 1000:7.1f} s  " + "; ".join(parts))


if __name__ == "__main__":
    main()
