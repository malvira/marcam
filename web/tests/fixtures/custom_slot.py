"""A custom toolpath as a person's script makes one: G-code for the plate example's vise
(its stock 60 x 40, Z0 its top), a slot 1 mm deep, an arc, and a small square pocket
cleared in a spiral. Usage: python custom_slot.py out.nc
"""
import sys

feed, plunge = 400, 60
out = ["%", "(custom toolpath from a Python script)", "G21 G90 G17", "G54", "T1 M6", "S12000 M3",
       "G0 X8 Y8 Z5"]
# the slot
out += ["G1 Z-1 F%d" % plunge, "G1 X30 F%d" % feed]
# an arc round to (30, 18) by G3 (counter-clockwise), centre (30, 13)
out += ["G3 X30 Y18 I0 J5"]
out += ["G0 Z5"]
# a square pocket 8 x 8 at (40, 20), cleared outwards in rings 1 mm apart, 0.5 mm deep
cx, cy = 44.0, 24.0
out += ["G0 X%.3f Y%.3f" % (cx, cy), "G1 Z-0.5 F%d" % plunge]
for k in range(1, 5):
    h = float(k)
    for x, y in ((cx - h, cy - h), (cx + h, cy - h), (cx + h, cy + h), (cx - h, cy + h), (cx - h, cy - h)):
        out.append("G1 X%.3f Y%.3f F%d" % (x, y, feed))
out += ["G0 Z5", "M5", "M30", "%"]
open(sys.argv[1], "w").write("\n".join(out) + "\n")
