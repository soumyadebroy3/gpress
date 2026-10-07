#!/usr/bin/env python3
"""Describe the blocks of a zlib stream: type, header bits, data bits, symbols, bytes produced."""
import sys, zlib

class BR:
    def __init__(s, b): s.b, s.p = b, 0
    def get(s, n):
        v = 0
        for k in range(n):
            v |= ((s.b[s.p >> 3] >> (s.p & 7)) & 1) << k
            s.p += 1
        return v

LB=[3,4,5,6,7,8,9,10,11,13,15,17,19,23,27,31,35,43,51,59,67,83,99,115,131,163,195,227,258]
LE=[0,0,0,0,0,0,0,0,1,1,1,1,2,2,2,2,3,3,3,3,4,4,4,4,5,5,5,5,0]
DE=[0,0,0,0,1,1,2,2,3,3,4,4,5,5,6,6,7,7,8,8,9,9,10,10,11,11,12,12,13,13]
ORD=[16,17,18,0,8,7,9,6,10,5,11,4,12,3,13,2,14,1,15]

def table(lens):
    codes = {}
    bl = [0]*16
    for l in lens:
        if l: bl[l] += 1
    nxt = [0]*16; c = 0
    for b in range(1,16):
        c = (c + bl[b-1]) << 1; nxt[b] = c
    for s, l in enumerate(lens):
        if l:
            codes[(l, nxt[l])] = s; nxt[l] += 1
    return codes

def dec(br, t):
    code = 0
    for l in range(1, 16):
        code = (code << 1) | br.get(1)
        if (l, code) in t: return t[(l, code)]
    raise ValueError('bad code')

def describe(z):
    br = BR(z[2:]); out = []
    while True:
        start = br.p
        last = br.get(1); bt = br.get(2)
        if bt == 0:
            br.p = (br.p + 7) // 8 * 8
            n = br.get(16); br.get(16); br.p += 8*n
            out.append(('stored', br.p-start, 0, n, n)); 
        else:
            if bt == 1:
                ll = [8]*144+[9]*112+[7]*24+[8]*8; dl=[5]*32
            else:
                hlit=br.get(5)+257; hdist=br.get(5)+1; hclen=br.get(4)+4
                cl=[0]*19
                for i in range(hclen): cl[ORD[i]]=br.get(3)
                ct=table(cl); lens=[]
                while len(lens)<hlit+hdist:
                    s=dec(br,ct)
                    if s<16: lens.append(s)
                    elif s==16: lens += [lens[-1]]*(3+br.get(2))
                    elif s==17: lens += [0]*(3+br.get(3))
                    else: lens += [0]*(11+br.get(7))
                ll=lens[:hlit]; dl=lens[hlit:]
            hdr = br.p - start
            lt=table(ll); dt=table(dl); nsym=0; nbytes=0
            while True:
                s=dec(br,lt)
                if s==256: break
                nsym+=1
                if s<256: nbytes+=1
                else:
                    c=s-257; L=LB[c]+br.get(LE[c]); d=dec(br,dt); br.get(DE[d]); nbytes+=L
            out.append(('dyn' if bt==2 else 'fixed', hdr, br.p-start-hdr, nsym, nbytes))
        if last: break
    return out

if __name__ == '__main__':
    for f in sys.argv[1:]:
        z = open(f,'rb').read()
        bl = describe(z)
        print(f, len(z), 'bytes', len(bl), 'blocks')
        for b in bl: print('   %-6s header %5d bits, data %7d bits, %6d symbols, %7d bytes' % b)
