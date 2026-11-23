"""Write wound_face.step: one face of an impeller's hub floor, made up (marcam STEP fixture).

A dish (a curved profile turned about Z, written as NX writes such a surface: a cubic rational
B-spline round the axis whose knots run past their ends) between a circle r5 and the rim r20,
with three twisted "blade root" notches cut into it from the rim. Each notch turns 158 degrees,
so every half-plane from the axis crosses one: no iso-line seam can split the face, and
SolidCraft's kernel leaves it out (marcam meshes it from the file instead).

    python tests/fixtures/step/make_wound_face.py
"""

import math
import os

R0, R1 = 5.0, 20.0  # hub circle, rim
ZS = (1.5, 1.0, 6.0)  # the profile's control heights at r 5, 12.5, 20 (a quadratic)
NOTCH_IN = 8.0  # radius each notch reaches
TURN, WIDTH = 150.0, 8.0  # degrees a notch's sides turn, and its width
STARTS = (0.0, 120.0, 240.0)
N_SPIRAL = 160


def z_at(r):
    u = (r - R0) / (R1 - R0)
    return (1 - u) ** 2 * ZS[0] + 2 * u * (1 - u) * ZS[1] + u * u * ZS[2]


def pt(r, deg):
    a = math.radians(deg)
    return (r * math.cos(a), r * math.sin(a), z_at(r))


lines = []
n = [0]


def add(text):
    n[0] += 1
    lines.append(f"#{n[0]}={text};")
    return n[0]


def fmt(x):
    return repr(float(x))


def cpoint(p):
    return add(f"CARTESIAN_POINT('',({','.join(fmt(c) for c in p)}))")


def direction(d):
    return add(f"DIRECTION('',({','.join(fmt(c) for c in d)}))")


app = add("APPLICATION_CONTEXT('managed model based 3d engineering')")
add(f"APPLICATION_PROTOCOL_DEFINITION('international standard','ap242_managed_model_based_3d_engineering',2014,#{app})")
pc = add(f"PRODUCT_CONTEXT('',#{app},'mechanical')")
pdc = add(f"PRODUCT_DEFINITION_CONTEXT('part definition',#{app},'design')")
mm = add("(LENGTH_UNIT()NAMED_UNIT(*)SI_UNIT(.MILLI.,.METRE.))")
rad = add("(NAMED_UNIT(*)PLANE_ANGLE_UNIT()SI_UNIT($,.RADIAN.))")
sr = add("(NAMED_UNIT(*)SI_UNIT($,.STERADIAN.)SOLID_ANGLE_UNIT())")
unc = add(f"UNCERTAINTY_MEASURE_WITH_UNIT(LENGTH_MEASURE(1.E-3),#{mm},'distance_accuracy_value','confusion accuracy')")
ctx = add(f"(GEOMETRIC_REPRESENTATION_CONTEXT(3)GLOBAL_UNCERTAINTY_ASSIGNED_CONTEXT((#{unc}))GLOBAL_UNIT_ASSIGNED_CONTEXT((#{mm},#{rad},#{sr}))REPRESENTATION_CONTEXT('Context #1','3D Context with UNIT and UNCERTAINTY'))")
o = cpoint((0, 0, 0))
zx = direction((0, 0, 1))
xx = direction((1, 0, 0))
origin = add(f"AXIS2_PLACEMENT_3D('',#{o},#{zx},#{xx})")
prod = add(f"PRODUCT('wound_face','wound_face','',(#{pc}))")
pdf = add(f"PRODUCT_DEFINITION_FORMATION('','',#{prod})")
pd = add(f"PRODUCT_DEFINITION('design','',#{pdf},#{pdc})")
pds = add(f"PRODUCT_DEFINITION_SHAPE('','',#{pd})")
shape = add(f"SHAPE_REPRESENTATION('wound_face',(#{origin}),#{ctx})")
add(f"SHAPE_DEFINITION_REPRESENTATION(#{pds},#{shape})")

# the surface: rows along the profile (u), columns round the axis (v), as NX writes it
rows = []
for r, z in zip((R0, 0.5 * (R0 + R1), R1), ZS):
    ring = [(-r, 0), (-r, -2 * r), (r, -2 * r), (r, 0), (r, 2 * r), (-r, 2 * r), (-r, 0)]
    rows.append("(" + ",".join(f"#{cpoint((x, y, z))}" for x, y in ring) + ")")
w = "(1.,0.333333333333333,0.333333333333333,1.,0.333333333333333,0.333333333333333,1.)"
surf = add(
    "(BOUNDED_SURFACE()B_SPLINE_SURFACE(2,3,(" + ",".join(rows) + "),.UNSPECIFIED.,.F.,.T.,.F.)"
    "B_SPLINE_SURFACE_WITH_KNOTS((3,3),(1,3,3,3,1),(0.,1.),(-0.5,0.,0.5,1.,1.5),.UNSPECIFIED.)"
    "GEOMETRIC_REPRESENTATION_ITEM()RATIONAL_B_SPLINE_SURFACE((" + ",".join([w] * 3) + "))"
    "REPRESENTATION_ITEM('')SURFACE())"
)


def vertex(p):
    return add(f"VERTEX_POINT('',#{cpoint(p)})")


def circle(r):
    c = cpoint((0, 0, z_at(r)))
    return add(f"CIRCLE('',#{add(f'AXIS2_PLACEMENT_3D(\'\',#{c},#{zx},#{xx})')},{fmt(r)})")


def polyline(points):
    k = len(points)
    ids = ",".join(f"#{cpoint(p)}" for p in points)
    mults = ",".join(["2"] + ["1"] * (k - 2) + ["2"])
    knots = ",".join(fmt(i) for i in range(k))
    return add(f"B_SPLINE_CURVE_WITH_KNOTS('',1,({ids}),.UNSPECIFIED.,.F.,.F.,({mults}),({knots}),.UNSPECIFIED.)")


def edge(v1, v2, curve, same=True):
    return add(f"EDGE_CURVE('',#{v1},#{v2},#{curve},{'.T.' if same else '.F.'})")


def oriented(e, fwd=True):
    return add(f"ORIENTED_EDGE('',*,*,#{e},{'.T.' if fwd else '.F.'})")


rim, ring, hub = circle(R1), circle(NOTCH_IN), circle(R0)
notches = []
for phi in STARTS:
    a0, a1 = vertex(pt(R1, phi)), vertex(pt(NOTCH_IN, phi + TURN))
    b0, b1 = vertex(pt(NOTCH_IN, phi + TURN + WIDTH)), vertex(pt(R1, phi + WIDTH))
    notches.append((phi, a0, a1, b0, b1))
outer = []
for k, (phi, a0, a1, b0, b1) in enumerate(notches):
    side = lambda off, i: pt(R1 - (R1 - NOTCH_IN) * i / N_SPIRAL, phi + off + TURN * i / N_SPIRAL)
    # in along one side of the notch, across its end, out along the other
    outer.append(oriented(edge(a0, a1, polyline([side(0.0, i) for i in range(N_SPIRAL + 1)]))))
    outer.append(oriented(edge(a1, b0, ring)))
    outer.append(oriented(edge(b1, b0, polyline([side(WIDTH, i) for i in range(N_SPIRAL + 1)])), False))
    # then round the rim to the next notch
    outer.append(oriented(edge(b1, notches[(k + 1) % len(notches)][1], rim)))
hv = vertex(pt(R0, 0.0))
inner = [oriented(edge(hv, hv, hub), False)]
lo = add(f"EDGE_LOOP('',({','.join(f'#{u}' for u in outer)}))")
li = add(f"EDGE_LOOP('',({','.join(f'#{u}' for u in inner)}))")
face = add(f"ADVANCED_FACE('',(#{add(f'FACE_OUTER_BOUND(\'\',#{lo},.T.)')},#{add(f'FACE_BOUND(\'\',#{li},.T.)')}),#{surf},.T.)")
shell = add(f"OPEN_SHELL('',(#{face}))")
model = add(f"SHELL_BASED_SURFACE_MODEL('wound_face',(#{shell}))")
rep = add(f"MANIFOLD_SURFACE_SHAPE_REPRESENTATION('wound_face',(#{model},#{origin}),#{ctx})")
add(f"SHAPE_REPRESENTATION_RELATIONSHIP('','',#{shape},#{rep})")
add(f"PRODUCT_RELATED_PRODUCT_CATEGORY('part','',(#{prod}))")

head = """ISO-10303-21;
HEADER;
FILE_DESCRIPTION(('marcam STEP import fixture: a face wound round its surface (make_wound_face.py)'),'2;1');
FILE_NAME('wound_face.step','',('marcam fixtures'),(''),'make_wound_face.py','make_wound_face.py','');
FILE_SCHEMA(('AP242_MANAGED_MODEL_BASED_3D_ENGINEERING_MIM_LF { 1 0 10303 442 1 1 4 }'));
ENDSEC;
DATA;
"""
out = os.path.join(os.path.dirname(os.path.abspath(__file__)), "wound_face.step")
with open(out, "w", newline="\n") as f:
    f.write(head + "\n".join(lines) + "\nENDSEC;\nEND-ISO-10303-21;\n")


def area():
    """The face's area (numerically): the dish between r5 and r20 less the three notches."""
    total = 0.0
    nr, na = 600, 2160
    for i in range(nr):
        r = R0 + (R1 - R0) * (i + 0.5) / nr
        dr = (R1 - R0) / nr
        dz = (z_at(r + 1e-6) - z_at(r - 1e-6)) / 2e-6
        ring_area = 2 * math.pi * r * dr * math.sqrt(1 + dz * dz)
        # the notches' share of this ring: WIDTH degrees each where they reach
        cut = sum(WIDTH for _ in STARTS) / 360.0 if r >= NOTCH_IN else 0.0
        total += ring_area * (1 - cut)
    return total


print(out, "area", round(area(), 3))
