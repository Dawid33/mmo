#!/usr/bin/env python3
"""CPU ms per frame per Bevy system from a folded `perf` profile.

    perf script --no-inline -i perf.data | inferno-collapse-perf > folded.txt
    scripts/perf_systems.py folded.txt <frames-in-recording> [top=40]

Demangles every frame with c++filt (Rust v0), attributes each stack to the
innermost `<FunctionSystem<…, NAME> as System>::run_unsafe` frame, and prints
per-thread and per-system ms/frame (samples are cpu-clock ns). The main thread
and Bevy's (unnamed) render thread both show as the process name ("client").
Stacks outside any system land in "(<thread> other)". `perf_summary.py` gives
the per-symbol view; this one answers "which system".
"""
import json, re, subprocess, sys, collections

folded=sys.argv[1]
syms=set()
for l in open(folded):
    syms.update(l.rstrip('\n').rpartition(' ')[0].split(';'))
syms=sorted(syms)
out=subprocess.run(['c++filt'],input='\n'.join(syms),capture_output=True,text=True).stdout.splitlines()
m={s:re.sub(r'::h[0-9a-f]{16}','',d) for s,d in zip(syms,out)}
frames=float(sys.argv[2])
def clean(s):
    s=re.sub(r'\[[0-9a-f]{16}\]','',s)
    return s
sysc=collections.Counter(); thr=collections.Counter(); leafc=collections.Counter()
for l in open(folded):
    st,_,n=l.rstrip('\n').rpartition(' '); n=int(n)
    fr=st.split(';'); t=fr[0]; fr=[clean(m.get(f,f)) for f in fr[1:]]
    thr[t]+=n
    name=None
    for f in reversed(fr):
        key='> as bevy_ecs::system::system::System>::run_unsafe'
        if key in f:
            name=f.split(key)[0].rsplit(', ',1)[-1]
            break
    if name is None:
        # render graph / pass work
        for f in fr:
            if 'render_app' in f or 'pipelined_rendering' in f: name='(render thread, non-system)'; break
        name=name or f'({t} other)'
    sysc[name]+=n
    leafc[fr[-1][:110] if fr else '?']+=n
tot=sum(thr.values())
print("threads ms/frame:", {k: round(v/1e6/frames,2) for k,v in thr.most_common(6)})
for k,v in sysc.most_common(int(sys.argv[3]) if len(sys.argv)>3 else 40):
    print(f"{v/1e6/frames:7.3f} ms/frm  {k[:150]}")
