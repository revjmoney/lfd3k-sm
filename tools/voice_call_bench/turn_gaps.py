"""Voice bench (task 9b): which quarter seconds of each turn reached the far ear.

py -3.14 turn_gaps.py SF12 <lag DAVE to MAYA ms> <lag MAYA to DAVE ms>

One line per turn: # heard, X said but not heard (silent in the far ear
at the measured mouth to ear lag), blank: no speech. Uses analyze.py.
"""
import sys, numpy as np, wave, csv, os
import analyze as A
sf=sys.argv[1]; d=os.path.join(A.HERE,sf)
side={"a":A.rd(os.path.join(A.CONVO,"side_a.wav")),"b":A.rd(os.path.join(A.CONVO,"side_b.wav"))}
rec={w:A.rd(os.path.join(d,w,"heard.wav")) for w in "ab"}
m={w:A.marks(os.path.join(d,w,"trace","marks.csv")) for w in "ab"}
rec0={w:int(next(r for r in m[w] if r["what"]=="rec")["unix_ms"]) for w in "ab"}
lag={"a":int(sys.argv[2]),"b":int(sys.argv[3])}
for w,ear in (("a","b"),("b","a")):
    for r in m[w]:
        if r["what"]!="play" or r["a"]==r["b"]: continue
        u=int(r["unix_ms"]); a=int(r["a"]); b=int(r["b"])
        out=[]
        for t in range(a,b,250):
            s=side[w][t*16:(t+250)*16]
            if np.sqrt((s**2).mean())<300: out.append(' '); continue
            eu=u+(t-a)+lag[w]; i=(eu-rec0[ear])*16
            e=rec[ear][i:i+250*16]
            out.append('#' if len(e) and np.sqrt((e**2).mean())>150 else 'X')
        print(w, f"{(u-rec0[ear])/1000:6.1f}s", f"{a/1000:6.1f}-{b/1000:6.1f}", ''.join(out))
