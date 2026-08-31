# Faithful Python mirror of src/always/translit.rs's rule engine.
V={"अ":"a","आ":"aa","इ":"i","ई":"i","उ":"u","ऊ":"u","ए":"e","ऍ":"e","ऎ":"e",
   "ऐ":"ai","ओ":"o","ऑ":"o","ऒ":"o","औ":"au","ऋ":"ri","ॠ":"ri","ऌ":"li","ॡ":"li"}
AA="\x02"; SCHWA="\x01"
M={"ा":AA,"ि":"i","ी":"i","ु":"u","ू":"u","े":"e","ॆ":"e","ॅ":"e","ै":"ai",
   "ो":"o","ॊ":"o","ॉ":"o","ौ":"au","ृ":"ri","ॄ":"ri"}
C={"क":"k","क़":"k","ख":"kh","ख़":"kh","ग":"g","ग़":"g","घ":"gh","ङ":"n",
   "च":"c","छ":"x","ज":"j","ज़":"z","झ":"jh","ञ":"n","ट":"t","ठ":"th","ड":"d",
   "ड़":"r","ढ":"dh","ढ़":"rh","ण":"n","त":"t","थ":"th","द":"d","ध":"dh",
   "न":"n","ऩ":"n","प":"p","फ":"ph","फ़":"f","ब":"b","भ":"v","म":"m",
   "य":"y","य़":"y","र":"r","ऱ":"r","ल":"l","ळ":"l","ऴ":"l","व":"w",
   "श":"s","ष":"s","स":"s","ह":"h"}
DIG={d:str(i) for i,d in enumerate("०१२३४५६७८९")}
VIRAMA="्"; PUNCT=set("।॥॰ॱ")
def isdev(c): return "ऀ"<=c<="ॿ"

def syllabify(w):
    out=[]; i=0; n=len(w)
    while i<n:
        ch=w[i]
        if ch in DIG: out.append([DIG[ch],""]); i+=1; continue
        if ch in C:
            onset=C[ch]; i+=1
            while i+1<n and w[i]==VIRAMA and w[i+1] in C:
                nxt=C[w[i+1]]
                onset = "x" if (onset=="c" and nxt=="x") else onset+nxt
                i+=2
            if i<n and w[i]==VIRAMA: out.append([onset,""]); i+=1; continue
            if i<n and w[i] in M:
                v=M[w[i]]; i+=1
                if v==AA and i<n and w[i] in ("ई","इ"): v="ai"; i+=1
                out.append([onset,v]); continue
            out.append([onset,SCHWA]); continue
        if ch in V: out.append(["",V[ch]]); i+=1; continue
        if ch in ("ं","ँ","ः"):
            if out and out[-1][1]==SCHWA: out[-1][1]="a"
            out.append(["h" if ch=="ः" else "n",""]); i+=1; continue
        if ch=="ॐ": out.append(["om",""]); i+=1; continue
        if ch in ("।","॥"): out.append([".",""]); i+=1; continue
        if ch in (VIRAMA,"‌","‍"): i+=1; continue
        if isdev(ch): i+=1; continue
        out.append([ch,""]); i+=1
    return out

def assemble(syl):
    idx=[k for k,(c,v) in enumerate(syl) if v and v!=SCHWA]
    last=idx[-1] if idx else None
    nv=sum(1 for c,v in syl if v)
    n=len(syl)
    for k in range(n):
        if syl[k][1]==AA:
            closed = k+1 < n
            syl[k][1]="aa" if (last==k and (closed or nv==1)) else "a"
    if n>2:
        if syl[n-1][1]==SCHWA: syl[n-1][1]=""
        for k in range(n-2,0,-1):
            if syl[k][1]==SCHWA and syl[k+1][1]!="": syl[k][1]=""
    return "".join(c+("a" if v==SCHWA else v) for c,v in syl)

def rule_word(w, freq):
    base=assemble(syllabify(w))
    if not base or freq.get(base,0)>0: return base
    cands=[]
    if base.endswith("aa"): cands.append(base[:-1])
    elif base.endswith("a"):
        if len(base)>2: cands.append(base[:-1])
        cands.append(base+"a")
    best=base; bf=0
    for c in cands:
        f=freq.get(c,0)
        if f>bf: bf=f; best=c
    return best
