"""Voice bench (task 9b): which tenths of a second of each turn reached the far ear.

py -3.14 turn_gaps.py SF12

One line per turn: the lag (mouth to ear, found per turn), then # heard,
X said but not heard, blank: no speech. Uses analyze.py.
"""
import sys, numpy as np, os
import analyze as A
sf=sys.argv[1]; d=os.path.join(A.HERE,sf)
side={"a":A.rd(os.path.join(A.CONVO,"side_a.wav")),"b":A.rd(os.path.join(A.CONVO,"side_b.wav"))}
rec={w:A.rd(os.path.join(d,w,"heard.wav")) for w in "ab"}
m={w:A.marks(os.path.join(d,w,"trace","marks.csv")) for w in "ab"}
rec0={w:int(next(r for r in m[w] if r["what"]=="rec")["unix_ms"]) for w in "ab"}
def env(x): 
    n=len(x)//1600; return np.sqrt((x[:n*1600].reshape(n,1600)**2).mean(1))
for w,ear in (("a","b"),("b","a")):
    for r in m[w]:
        if r["what"]!="play" or r["a"]==r["b"]: continue
        u=int(r["unix_ms"]); a=int(r["a"]); b=int(r["b"])
        s=side[w][a*16:b*16]; es=env(s)>300
        best=(-1,0)
        for lag in range(0,9000,100):
            i=(u+lag-rec0[ear])*16
            e=rec[ear][i:i+len(s)]
            if len(e)<len(s): break
            ee=env(e)>150
            sc=(ee==es).mean()
            if sc>best[0]: best=(sc,lag)
        lag=best[1]; i=(u+lag-rec0[ear])*16
        ee=env(rec[ear][i:i+len(s)])>150
        line=''.join('#' if (x and y) else ('X' if x else ' ') for x,y in zip(es,ee))
        print(w,f"lag {lag:5d}",line)
