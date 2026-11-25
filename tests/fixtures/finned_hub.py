"""A hub round the axis (X) with four fins twisting round it along X, 2 degrees a mm (an
impeller's blades, simplified): the channels between them for rotary flowline."""
import math, struct, sys
tris = []
def quad(a, b, c, d):
    tris.append((a, b, c)); tris.append((a, c, d))
# hub: a 72-gon prism, r 6, X 0..30, closed at its ends
n, r, L = 72, 6.0, 30.0
for k in range(n):
    a0, a1 = 2 * math.pi * k / n, 2 * math.pi * (k + 1) / n
    p = lambda x, a: (x, r * math.cos(a), r * math.sin(a))
    quad(p(0, a0), p(0, a1), p(L, a1), p(L, a0))
    tris.append(((0, 0, 0), p(0, a1), p(0, a0)))
    tris.append(((L, 0, 0), p(L, a0), p(L, a1)))
# four fins, out to r 12, 2 mm thick, twisting 2 degrees a mm, in 0.5 mm slices
for k in range(4):
    for j in range(60):
        x0, x1 = j * 0.5, (j + 1) * 0.5
        def c(x, rr, v):
            a = math.radians(90 * k + 2.0 * x)
            return (x, rr * math.cos(a) - v * math.sin(a), rr * math.sin(a) + v * math.cos(a))
        # (each face wound to face out: the +v side the other way round)
        for r0, r1, v0, v1 in [(6, 12, -1, -1), (12, 12, -1, 1)]:
            quad(c(x0, r0, v0), c(x0, r1, v1), c(x1, r1, v1), c(x1, r0, v0))
        quad(c(x1, 6, 1), c(x1, 12, 1), c(x0, 12, 1), c(x0, 6, 1))
        if j == 0 or j == 59:
            x = x0 if j == 0 else x1
            quad(c(x, 6, -1), c(x, 12, -1), c(x, 12, 1), c(x, 6, 1))
with open(sys.argv[1], "wb") as f:
    f.write(b"marcam test: finned hub".ljust(80, b" "))
    f.write(struct.pack("<I", len(tris)))
    for t in tris:
        f.write(struct.pack("<3f", 0, 0, 0))
        for v in t:
            f.write(struct.pack("<3f", *v))
        f.write(bytes(2))
