import ctypes, ctypes.wintypes as w, struct, sys
k=ctypes.windll.kernel32
PROCESS_VM_READ=0x10; PROCESS_QUERY_INFORMATION=0x400
pid=int(sys.argv[1])
h=k.OpenProcess(PROCESS_VM_READ|PROCESS_QUERY_INFORMATION,False,pid)
assert h, "OpenProcess failed"
def read(addr,n):
    buf=ctypes.create_string_buffer(n); got=ctypes.c_size_t()
    ok=k.ReadProcessMemory(h,ctypes.c_void_p(addr),buf,n,ctypes.byref(got))
    return buf.raw[:got.value] if ok else b""
d=open(__import__("os").path.expandvars(r"%APPDATA%\Aurora\candidates.bin"),"rb").read()
recs=[struct.unpack("<Qf",d[i:i+12]) for i in range(0,len(d)-len(d)%12,12)]
xs=[a for a,v in recs if -3339<v<-3336]
print(len(xs),"x copies")
def fl(b,o): return struct.unpack_from("<f",b,o)[0]
def plaus(v): return abs(v)<40000 and v==v and abs(v)>0.001
found=[]
for a in xs:
    b=read(a-32,32+48)
    if len(b)<80: continue
    vals=[fl(b,o) for o in range(0,80,4)]
    # Print window: 8 floats before, x, 11 after
    now=vals[8]
    pre=" ".join(f"{v:9.2f}" if plaus(v) else "    .    " for v in vals[4:8])
    post=" ".join(f"{v:9.2f}" if plaus(v) else "    .    " for v in vals[9:16])
    print(f"{a:#x}  [{pre}]  X={now:9.2f}  [{post}]")
k.CloseHandle(h)
