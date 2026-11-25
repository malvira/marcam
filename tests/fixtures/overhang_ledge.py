"""A hub round the axis (X) and two ledges out past a gap, joined by a web at the back end:
a ray from the axis toward the ledge leaves the hub and meets the ledge further out."""
import math, struct, sys
tris = []
def quad(a, b, c, d):
    tris.append((a, b, c)); tris.append((a, c, d))
def box(lo, hi):
    c = lambda i: (hi[0] if i & 1 else lo[0], hi[1] if i & 2 else lo[1], hi[2] if i & 4 else lo[2])
    for q in [(0, 2, 6, 4), (1, 5, 7, 3), (0, 4, 5, 1), (2, 3, 7, 6), (0, 1, 3, 2), (4, 6, 7, 5)]:
        quad(*[c(i) for i in q])
# hub: a 48-gon prism, r 5, X 0..20
n, r, L = 48, 5.0, 20.0
for k in range(n):
    a0, a1 = 2 * math.pi * k / n, 2 * math.pi * (k + 1) / n
    p = lambda x, a: (x, r * math.cos(a), r * math.sin(a))
    quad(p(0, a0), p(0, a1), p(L, a1), p(L, a0))
    tris.append(((0, 0, 0), p(0, a1), p(0, a0)))
    tris.append(((L, 0, 0), p(L, a0), p(L, a1)))
# the ledge (Y 8..10, Z -1..1) and the web joining it to the hub at the back (X 0..3)
# (two, opposite: the part centred on the hub)
for sgn in (1, -1):
    box((0, min(10 * sgn, 12 * sgn), -1), (L, max(10 * sgn, 12 * sgn), 1))
    box((0, min(4.5 * sgn, 10 * sgn), -1), (3, max(4.5 * sgn, 10 * sgn), 1))
with open(sys.argv[1], "wb") as f:
    f.write(b"marcam test: hub, gap, ledge".ljust(80, b" "))
    f.write(struct.pack("<I", len(tris)))
    for t in tris:
        f.write(struct.pack("<3f", 0, 0, 0))
        for v in t:
            f.write(struct.pack("<3f", *v))
        f.write(b"\0\0")
