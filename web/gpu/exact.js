// The playback's stock drawn exact (Mar: "there cant be visual artifacts as that defeats the
// point of the sim"): each pixel's ray meets the bar minus the tool's swept volumes, worked out
// in the fragment shader (WebGL2, in three.js's own material: lit and depth-tested as the rest
// of the scene). No mesh, so no grid to show at any zoom: op boundaries and normals are the
// tools' own.
//
// The engine (simExact, see crates/marcam-core/src/exact.rs: the same algorithm, its
// reference) sends the pieces (a tool moved straight with its axis held, or turning with A),
// and a bounding volume hierarchy over their tools' stretches. A ray: from where it enters the
// bar, while its point is in a piece played before the playback's place (the hierarchy's
// nodes hold the first segment under them: the place is a cutoff), on to where that piece's
// volume ends along it (the furthest of those). Where it stops is the surface, the bar's own
// or the exit of the piece that cut it there: that tool's normal, its operation's colour.

const W = 2048; // data textures' width (texels)

/** A typed array's floats into an RGBA float texture `W` wide (`per` texels an item). */
function dataTexture(THREE, floats) {
  const texels = Math.max(1, Math.ceil(floats.length / 4));
  const h = Math.max(1, Math.ceil(texels / W));
  const data = new Float32Array(W * h * 4);
  data.set(floats);
  const t = new THREE.DataTexture(data, W, h, THREE.RGBAFormat, THREE.FloatType);
  t.minFilter = t.magFilter = THREE.NearestFilter;
  t.generateMipmaps = false;
  t.needsUpdate = true;
  return t;
}

const floats = (x) => {
  if (x instanceof Float32Array) return x;
  if (x instanceof Uint8Array) return new Float32Array(x.buffer, x.byteOffset, x.byteLength / 4);
  const b = Uint8Array.from(atob(x), (c) => c.charCodeAt(0));
  return new Float32Array(b.buffer);
};

// The ray cast (GLSL ES 3.0). Mirrors marcam::exact (span, span_at, Scene::cast).
export const CAST = /* glsl */ `
uniform highp sampler2D exPieces;
uniform highp sampler2D exNodes;
uniform highp sampler2D exPrims;
uniform highp sampler2D exOps;
uniform vec4 exTools[16];
uniform float exRadius;
uniform float exLength;
uniform float exCut;
uniform float exOpCount;
uniform mat4 exInvMVP;
uniform mat4 exMVP;
uniform mat4 exMV;
uniform vec3 exCam;
uniform vec4 exViewport;
uniform int exMaxSteps;
// (1: loop bounds the compiler cannot know, so it does not unroll them)
uniform int exLoop;

vec3 exNormalView;
vec3 exColor;
vec3 exViewPos;
float exDepth;
float exSteps;
// (for tests: the operation seen, 0 the bar's own; the ray's t from the camera)
float exOp;
float exT;

const float EX_INF = 1e30;
// (how much more a later operation must take off a surface to colour it, mm)
const float EX_COLOUR = 0.003;

vec4 exFetch(highp sampler2D s, int i) {
  return texelFetch(s, ivec2(i - (i / ${W}) * ${W}, i / ${W}), 0);
}
float exPrim(int k) {
  vec4 v = exFetch(exPrims, k / 4);
  int c = k - (k / 4) * 4;
  return c == 0 ? v.x : c == 1 ? v.y : c == 2 ? v.z : v.w;
}

float exQuadAt(float q[6], float t, float s) {
  return q[0] * t * t + 2.0 * q[1] * t * s + q[2] * s * s + 2.0 * q[3] * t + 2.0 * q[4] * s + q[5];
}

int exRoots(float a, float b, float c, out float r0, out float r1) {
  r0 = 0.0;
  r1 = 0.0;
  float sc = max(abs(a), max(abs(b), abs(c)));
  if (sc == 0.0) return 0;
  if (abs(a) <= 1e-7 * sc) {
    if (abs(b) > 1e-7 * sc) { r0 = -c / b; return 1; }
    return 0;
  }
  float disc = b * b - 4.0 * a * c;
  if (disc < 0.0) {
    if (disc > -1e-5 * b * b) { r0 = -b / (2.0 * a); return 1; }
    return 0;
  }
  float q = -0.5 * (b + (b >= 0.0 ? 1.0 : -1.0) * sqrt(disc));
  if (q != 0.0) { r0 = q / a; r1 = c / q; return 2; }
  return 1;
}

bool exOk(float q[6], vec3 L[4], int nl, float t, float s, int skipA, int skipB, bool quadOn, float scale) {
  for (int i = 0; i < 4; i++) {
    if (i >= nl) break;
    if (i + 1 == skipA || i + 1 == skipB) continue;
    vec3 l = L[i];
    float v = l.x * t + l.y * s + l.z;
    float m = max(abs(l.x), max(abs(l.y), abs(l.z))) * 2e-6 + 1e-7;
    if (v < -m) return false;
  }
  return quadOn || exQuadAt(q, t, s) <= 1e-5 * scale;
}

struct ExExt { bool ok; float t0; float t1; float s1; int tag1; };

void exTake(inout ExExt e, float t, float s, int tag) {
  if (!e.ok) { e.ok = true; e.t0 = t; e.t1 = t; e.s1 = s; e.tag1 = tag; return; }
  if (t < e.t0) e.t0 = t;
  if (t > e.t1) { e.t1 = t; e.s1 = s; e.tag1 = tag; }
}

// The extremes in t of { F <= 0, every line l.x t + l.y s + l.z >= 0 } (extent in exact.rs).
// One loop over the candidates (the quadric's own extremes, each line's crossings with it, the
// corners): each call site is inlined by the GPU's compiler, so there is one of each.
ExExt exExtent(float q[6], vec3 L[4], int nl) {
  ExExt e;
  e.ok = false; e.t0 = 0.0; e.t1 = 0.0; e.s1 = 0.0; e.tag1 = 0;
  float scale = max(1e-30, max(max(abs(q[0]), abs(q[1])), max(max(abs(q[2]), abs(q[3])), max(abs(q[4]), abs(q[5])))));
  float a = q[0], b = q[1], c = q[2], d = q[3], ee = q[4], f = q[5];
  // c 0: the quadric's extremes (dF/ds = 0); 1..4: line c-1; 5..10: the corners
  for (int ci = 0; ci < 11 * exLoop; ci++) {
    float ct[2], cs[2];
    int cn = 0, tag = 0, skipA = 0, skipB = 0;
    bool quadOn = true;
    if (ci <= 4) {
      vec3 l;
      if (ci == 0) {
        if (q[1] == 0.0 && q[2] == 0.0) continue;
        l = vec3(q[1], q[2], q[4]);
      } else {
        if (ci > nl) continue;
        l = L[ci - 1];
        tag = ci - 1 < 2 ? 0 : ci;
        skipA = ci;
      }
      bool byT = abs(l.y) >= abs(l.x);
      if (byT && l.y == 0.0) continue;
      float m = byT ? -l.x / l.y : -l.y / l.x;
      float nn = byT ? -l.z / l.y : -l.z / l.x;
      float A2 = byT ? a + 2.0 * b * m + c * m * m : a * m * m + 2.0 * b * m + c;
      float B1 = byT ? 2.0 * (b * nn + c * m * nn + d + ee * m) : 2.0 * (a * m * nn + b * nn + d * m + ee);
      float C0 = byT ? c * nn * nn + 2.0 * ee * nn + f : a * nn * nn + 2.0 * d * nn + f;
      float r0, r1;
      cn = exRoots(A2, B1, C0, r0, r1);
      for (int k = 0; k < 2; k++) {
        float r = k == 0 ? r0 : r1;
        ct[k] = byT ? r : m * r + nn;
        cs[k] = byT ? m * r + nn : r;
      }
    } else {
      int pk = ci - 5;
      int i = pk < 3 ? 0 : pk < 5 ? 1 : 2;
      int j = pk < 3 ? pk + 1 : pk < 5 ? pk - 1 : 3;
      if (j >= nl) continue;
      vec3 l1 = L[i], l2 = L[j];
      float det = l1.x * l2.y - l1.y * l2.x;
      if (abs(det) < 1e-6 * (abs(l1.x) + abs(l1.y)) * (abs(l2.x) + abs(l2.y))) continue;
      ct[0] = (-l1.z * l2.y + l1.y * l2.z) / det;
      cs[0] = (-l1.x * l2.z + l1.z * l2.x) / det;
      cn = 1;
      tag = i >= 2 ? i + 1 : j + 1;
      skipA = i + 1;
      skipB = j + 1;
      quadOn = false;
    }
    for (int k = 0; k < 2; k++) {
      if (k >= cn) break;
      float t = ct[k], s = cs[k];
      if (exOk(q, L, nl, t, s, skipA, skipB, quadOn, scale)) exTake(e, t, s, tag);
    }
  }
  return e;
}

struct ExSpan { bool ok; float t0; float t1; vec3 n1; };

// Where the ray o + t d meets the volume of the tool (radius, ball, cone) from tip P along A,
// len long, moved straight by V.
ExSpan exSpanStraight(vec3 P, vec3 V, vec3 A, float len, vec4 tool, vec3 o, vec3 d) {
  ExSpan sp;
  sp.ok = false; sp.t0 = 0.0; sp.t1 = 0.0; sp.n1 = vec3(0.0);
  vec3 mid = P + 0.5 * V;
  float tc = dot(mid - o, d);
  vec3 o2 = o + d * tc;
  vec3 w = o2 - P;
  float wz = dot(w, A), dz = dot(d, A), vz = dot(V, A);
  vec3 wp = w - A * wz, dp = d - A * dz, vp = V - A * vz;
  float r = tool.x;
  bool ball = tool.y > 0.5;
  float cone = tool.z;
  float lo = EX_INF, hi = -EX_INF;
  bool hiReal = false;
  vec3 n1 = vec3(0.0);
  vec3 S0 = vec3(0.0, 1.0, 0.0), S1 = vec3(0.0, -1.0, 1.0);
  for (int part = 0; part < 2 * exLoop; part++) {
    // 0 cylinder, 1 ball, 2 cone
    int kind = 0;
    float zlo = 0.0, zhi = len;
    bool floorReal = true;
    if (ball) {
      if (part == 0) kind = 1;
      else { if (len <= r) break; zlo = r; floorReal = false; }
    } else if (cone > 0.0) {
      float zc = r / cone;
      if (part == 0) { kind = 2; zhi = min(zc, max(len, 0.0)); }
      else { if (len <= zc) break; zlo = zc; floorReal = false; }
    } else if (part == 1) break;
    float q[6];
    vec3 L[4];
    int nl = 4;
    L[0] = S0; L[1] = S1;
    if (kind == 0) {
      q = float[6](dot(dp, dp), -dot(dp, vp), dot(vp, vp), dot(dp, wp), -dot(vp, wp), dot(wp, wp) - r * r);
      L[2] = vec3(dz, -vz, wz - zlo);
      L[3] = vec3(-dz, vz, zhi - wz);
    } else if (kind == 1) {
      vec3 wc = w - A * r;
      q = float[6](1.0, -dot(d, V), dot(V, V), dot(d, wc), -dot(V, wc), dot(wc, wc) - r * r);
      nl = 2;
      L[2] = vec3(0.0); L[3] = vec3(0.0);
    } else {
      float k2 = cone * cone;
      q = float[6](dot(dp, dp) - k2 * dz * dz, -dot(dp, vp) + k2 * dz * vz, dot(vp, vp) - k2 * vz * vz, dot(dp, wp) - k2 * dz * wz, -dot(vp, wp) + k2 * vz * wz, dot(wp, wp) - k2 * wz * wz);
      L[2] = vec3(dz, -vz, wz);
      L[3] = vec3(-dz, vz, zhi - wz);
    }
    ExExt e = exExtent(q, L, nl);
    if (!e.ok) continue;
    lo = min(lo, e.t0);
    vec3 qp = w + d * e.t1 - V * e.s1;
    float z = dot(qp, A);
    vec3 n;
    bool real = true;
    if (kind == 1) n = normalize(qp - A * r);
    else if (e.tag1 == 0 && kind == 0) n = normalize(qp - A * z);
    else if (e.tag1 == 0 && kind == 2) n = normalize(normalize(qp - A * z) - A * cone);
    else if (e.tag1 == 3) { n = -A; real = kind == 0 && floorReal; }
    else if (e.tag1 == 4) n = A;
    else n = normalize(qp - A * z);
    float eps = 1e-6 * (1.0 + abs(e.t1));
    if (e.t1 > hi + eps || (e.t1 >= hi - eps && real && !hiReal)) {
      if (e.t1 > hi) hi = e.t1;
      n1 = n;
      hiReal = real;
    }
  }
  if (lo <= hi) { sp.ok = true; sp.t0 = lo + tc; sp.t1 = hi + tc; sp.n1 = n1; }
  return sp;
}

float exSdBox(vec2 p, vec2 c, vec2 h) {
  vec2 q = abs(p - c) - h;
  return length(max(q, 0.0)) + min(max(q.x, q.y), 0.0);
}

float exSdPoly(vec2 p, vec2 v[5], int n) {
  float d = EX_INF;
  bool inside = true;
  for (int i = 0; i < 5; i++) {
    if (i >= n) break;
    vec2 a = v[i], b = v[i + 1 < n ? i + 1 : 0];
    vec2 e = b - a, w = p - a;
    float h = clamp(dot(w, e) / dot(e, e), 0.0, 1.0);
    d = min(d, length(w - e * h));
    if (e.x * w.y - e.y * w.x < 0.0) inside = false;
  }
  return inside ? -d : d;
}

// A point's signed distance to the tool at tip along a, len long (its profile's, in the plane
// through its axis).
float exSdTool(vec4 tool, vec3 tip, vec3 a, float len, vec3 x) {
  vec3 q = x - tip;
  float z = dot(q, a);
  float r = length(q - a * z);
  float R = tool.x;
  if (tool.y > 0.5) {
    float disc = length(vec2(r, z - R)) - R;
    if (len <= R) return disc;
    return min(disc, exSdBox(vec2(r, z), vec2(0.0, 0.5 * (R + len)), vec2(R, 0.5 * (len - R))));
  }
  if (tool.z > 0.0) {
    float zc = R / tool.z;
    vec2 v[5];
    if (len <= zc) {
      float w = tool.z * len;
      v[0] = vec2(0.0); v[1] = vec2(w, len); v[2] = vec2(-w, len); v[3] = vec2(0.0); v[4] = vec2(0.0);
      return exSdPoly(vec2(r, z), v, 3);
    }
    v[0] = vec2(0.0); v[1] = vec2(R, zc); v[2] = vec2(R, len); v[3] = vec2(-R, len); v[4] = vec2(-R, zc);
    return exSdPoly(vec2(r, z), v, 5);
  }
  return exSdBox(vec2(r, z), vec2(0.0, 0.5 * len), vec2(R, 0.5 * len));
}

// Whether a s² + b s + c <= 0 somewhere in [lo, hi].
bool exQuadLe(float a, float b, float c, float lo, float hi) {
  if (lo > hi) return false;
  float ql = (a * lo + b) * lo + c, qh = (a * hi + b) * hi + c;
  if (ql <= 0.0 || qh <= 0.0) return true;
  if (a > 0.0) {
    float v = -b / (2.0 * a);
    return v > lo && v < hi && (a * v + b) * v + c <= 0.0;
  }
  return false;
}

// The s in [lo, hi] where zlo <= z0 - s dz <= zhi.
vec2 exZRange(float z0, float dz, float zlo, float zhi, float lo, float hi) {
  if (abs(dz) < 1e-12) return z0 >= zlo && z0 <= zhi ? vec2(lo, hi) : vec2(1.0, 0.0);
  float a = (z0 - zlo) / dz, b = (z0 - zhi) / dz;
  return vec2(max(lo, min(a, b)), min(hi, max(a, b)));
}

// Whether x is in the straight piece's swept volume, eps inside (holds_straight in exact.rs).
bool exHoldsStraight(vec3 P, vec3 V, vec3 A, float len, vec4 tool, vec3 x, float eps) {
  vec3 q0 = x - P;
  float z0 = dot(q0, A), vz = dot(V, A);
  vec3 q0p = q0 - A * z0, vp = V - A * vz;
  float R = tool.x, r = R - eps;
  float ca = dot(vp, vp), cb = -2.0 * dot(q0p, vp), cc = dot(q0p, q0p) - r * r;
  if (tool.y > 0.5) {
    vec3 w = q0 - A * R;
    if (exQuadLe(dot(V, V), -2.0 * dot(w, V), dot(w, w) - r * r, 0.0, 1.0)) return true;
    if (len > R) { vec2 zr = exZRange(z0, vz, R, len - eps, 0.0, 1.0); return exQuadLe(ca, cb, cc, zr.x, zr.y); }
    return false;
  }
  if (tool.z > 0.0) {
    float k = tool.z, k2 = k * k, zc = R / k;
    float shift = eps * sqrt(1.0 + k2) / k;
    float zs = z0 - shift;
    vec2 zr = exZRange(z0, vz, shift, min(zc, len), 0.0, 1.0);
    if (exQuadLe(ca - k2 * vz * vz, cb + 2.0 * k2 * zs * vz, dot(q0p, q0p) - k2 * zs * zs, zr.x, zr.y)) return true;
    if (len > zc) { zr = exZRange(z0, vz, zc, len - eps, 0.0, 1.0); return exQuadLe(ca, cb, cc, zr.x, zr.y); }
    return false;
  }
  vec2 zr = exZRange(z0, vz, eps, len - eps, 0.0, 1.0);
  return exQuadLe(ca, cb, cc, zr.x, zr.y);
}

// A turning piece's tool at f: its tip and axis (the machine's tip m0 + f dm turned back by A).
void exPose(vec4 T0, vec4 T1, vec4 T2, float f, out vec3 tip, out vec3 axis) {
  vec3 m = T0.xyz + T1.xyz * f;
  float ang = -(T2.x + T2.y * f);
  float sn = sin(ang), cs = cos(ang);
  tip = vec3(m.x, cs * m.y - sn * m.z, sn * m.y + cs * m.z);
  axis = vec3(0.0, -sn, cs);
}

// Whether x is in the piece's volume (holds in exact.rs): the pose to leave it from (0 for a
// straight piece), or -1. A turning piece: its poses' distances to x (none further than half
// a pose's step: out), then the deepest place round the nearest, a golden search. One loop,
// the tool's distance worked out in one place (the GPU's compiler inlines every call).
float exHolds(int pi, vec3 x, float eps) {
  vec4 T0 = exFetch(exPieces, 4 * pi);
  vec4 T1 = exFetch(exPieces, 4 * pi + 1);
  vec4 T2 = exFetch(exPieces, 4 * pi + 2);
  vec4 T3 = exFetch(exPieces, 4 * pi + 3);
  float len = T0.w;
  vec4 tool = exTools[int(T2.w / 65536.0)];
  if (T3.y < 0.5) return exHoldsStraight(T0.xyz, T1.xyz, T2.xyz, len, tool, x, eps) ? 0.0 : -1.0;
  int n = int(T3.x);
  float h = 1.0 / float(n - 1);
  float speed = T2.z;
  float reach = 0.5 * speed * h + eps;
  float ds[16];
  for (int k = 0; k < 16; k++) ds[k] = EX_INF;
  // phase 0: the middle pose; 1: the poses; 3: the nearest not tried; 2: a golden search
  int phase = 0, k = 0, tries = 0, gi = 0;
  float done = -EX_INF, ga = 0.0, gb = 0.0, gc = 0.0, gv = 0.0, gx = 0.5;
  bool gright = false;
  for (int it = 0; it < 160 * exLoop; it++) {
    float f = phase == 0 ? 0.5 : phase == 1 ? float(k) * h : gx;
    vec3 tip, axis;
    exPose(T0, T1, T2, f, tip, axis);
    float dd = exSdTool(tool, tip, axis, len, x);
    if (phase == 0) {
      if (dd > 0.5 * speed + eps) return -1.0;
      phase = 1;
      continue;
    }
    if (phase == 1) {
      ds[k] = dd;
      k++;
      if (k < n) continue;
      phase = 3;
    } else if (phase == 2) {
      // (the least distance: the greatest of its negative)
      if (dd < gv) {
        if (gright) ga = gc; else gb = gc;
        gc = gx;
        gv = dd;
      } else {
        if (gright) gb = gx; else ga = gx;
      }
      gi++;
      if (gi < 28 && gb - ga >= 1e-7) {
        gright = gb - gc > gc - ga;
        gx = gright ? gc + 0.381966 * (gb - gc) : gc - 0.381966 * (gc - ga);
        continue;
      }
      if (gv <= -eps) return gc;
      phase = 3;
    }
    // phase 3: the nearest pose not yet tried, near enough
    int kb = -1;
    float vb = EX_INF;
    for (int kk = 0; kk < 16; kk++) {
      if (kk >= n) break;
      if (ds[kk] <= reach && ds[kk] > done && ds[kk] < vb) { vb = ds[kk]; kb = kk; }
    }
    if (kb < 0 || tries >= 3) return -1.0;
    tries++;
    done = vb;
    ga = float(max(kb - 1, 0)) * h;
    gb = float(min(kb + 1, n - 1)) * h;
    gc = float(kb) * h;
    gv = vb;
    gi = 0;
    gright = gb - gc > gc - ga;
    gx = gright ? gc + 0.381966 * (gb - gc) : gc - 0.381966 * (gc - ga);
    phase = 2;
  }
  return -1.0;
}

// Where the ray leaves the piece's volume from a point in it held at f (exit_from in
// exact.rs): a straight piece's whole stretch; a turning one's tool at f, and on through the
// run of poses next to it that the ray meets. One loop, one call.
bool exExit(int pi, float f, vec3 o, vec3 d, out float t1, out vec3 n1) {
  vec4 T0 = exFetch(exPieces, 4 * pi);
  vec4 T1 = exFetch(exPieces, 4 * pi + 1);
  vec4 T2 = exFetch(exPieces, 4 * pi + 2);
  vec4 T3 = exFetch(exPieces, 4 * pi + 3);
  float len = T0.w;
  vec4 tool = exTools[int(T2.w / 65536.0)];
  bool turning = T3.y > 0.5;
  int n = turning ? int(T3.x) : 2;
  float h = 1.0 / float(n - 1);
  int k0 = min(int(floor(f / h)), n - 1);
  t1 = -EX_INF;
  n1 = vec3(0.0);
  bool any = false;
  // phase 0: at f (a straight piece: its sweep); 1: on down from k0; 2: on up from k0 + 1
  int phase = 0, k = k0;
  for (int it = 0; it < 40 * exLoop; it++) {
    vec3 P = T0.xyz, V = T1.xyz, A = T2.xyz;
    if (turning) {
      exPose(T0, T1, T2, phase == 0 ? f : float(k) * h, P, A);
      V = vec3(0.0);
    }
    ExSpan s = exSpanStraight(P, V, A, len, tool, o, d);
    if (s.ok && s.t1 > t1) { t1 = s.t1; n1 = s.n1; }
    if (phase == 0) {
      if (!s.ok) return false;
      any = true;
      if (!turning) return true;
      phase = 1;
      k = k0;
      continue;
    }
    if (phase == 1) {
      if (s.ok && k > 0) { k--; continue; }
      phase = 2;
      k = k0 + 1;
      if (k >= n) return any;
      continue;
    }
    if (!s.ok || k + 1 >= n) return any;
    k++;
  }
  return any;
}

// The visible point on the stock: sets exNormalView, exColor, exViewPos, exDepth (or discards).
void exactCast() {
  vec2 ndc = ((gl_FragCoord.xy - exViewport.xy) / exViewport.zw) * 2.0 - 1.0;
  vec4 fp = exInvMVP * vec4(ndc, 1.0, 1.0);
  vec3 o = exCam;
  vec3 d = normalize(fp.xyz / fp.w - o);
  // (counted from the ray's point nearest the bar's middle: small numbers)
  float tb = dot(vec3(-0.5 * exLength, 0.0, 0.0) - o, d);
  o = o + d * tb;
  float t0 = -EX_INF, t1 = EX_INF;
  vec3 n0 = vec3(0.0);
  float a = d.y * d.y + d.z * d.z, b = o.y * d.y + o.z * d.z, c = o.y * o.y + o.z * o.z - exRadius * exRadius;
  if (a < 1e-12) {
    if (c > 0.0) discard;
  } else {
    float disc = b * b - a * c;
    if (disc < 0.0) discard;
    float q = sqrt(disc);
    t0 = (-b - q) / a;
    t1 = (-b + q) / a;
    vec3 p = o + d * t0;
    n0 = normalize(vec3(0.0, p.y, p.z));
  }
  if (abs(d.x) < 1e-12) {
    if (o.x < -exLength || o.x > 0.0) discard;
  } else {
    float e0 = (-exLength - o.x) / d.x, e1 = -o.x / d.x;
    vec3 ne = vec3(-1.0, 0.0, 0.0);
    if (e0 > e1) { float sw = e0; e0 = e1; e1 = sw; ne = vec3(1.0, 0.0, 0.0); }
    if (e0 > t0) { t0 = e0; n0 = ne; }
    t1 = min(t1, e1);
  }
  // (from the near plane on: what is nearer the camera is cut away, as drawing clips it; the
  // camera inside the stock sees a flat cap there, not the solid it sits in)
  vec4 np = exInvMVP * vec4(ndc, -1.0, 1.0);
  float tNear = dot(np.xyz / np.w - exCam, d) - tb;
  if (tNear > t0) { t0 = tNear; n0 = -d; }
  if (t0 >= t1) discard;
  float t = t0;
  int hitPiece = -1;
  vec3 n = n0;
  float eps = 3e-5 + 2e-7 * abs(t);
  int stack[64];
  exSteps = 0.0;
  // (then a last look for its colour: the first operation to have cut within EX_COLOUR of the
  // surface there; see COLOUR_DEPTH in exact.rs)
  bool colourPass = false;
  int colourPiece = -1;
  float firstSeg = 0.0;
  for (int it = 0; it < exMaxSteps + 1; it++) {
    exSteps += 1.0;
    vec3 p = o + d * t + (colourPass ? n * EX_COLOUR : vec3(0.0));
    float best = t + eps;
    int found = -1;
    vec3 fn = vec3(0.0);
    // the pieces holding the point (up to 8: any one is a step on)
    int cand[8];
    float candF[8];
    int nc = 0;
    int sp = 0;
    stack[sp++] = 0;
    for (int guard = 0; guard < 100000 * exLoop; guard++) {
      if (sp == 0 || nc >= 8) break;
      int ni = stack[--sp];
      vec4 N0 = exFetch(exNodes, 3 * ni);
      vec4 N1 = exFetch(exNodes, 3 * ni + 1);
      vec4 N2 = exFetch(exNodes, 3 * ni + 2);
      if (N2.x >= exCut) continue;
      if (any(lessThan(p, N0.xyz)) || any(greaterThan(p, N1.xyz))) continue;
      int first = int(N0.w), count = int(N1.w);
      if (count == 0) {
        if (sp < 62) { stack[sp++] = first; stack[sp++] = first + 1; }
        continue;
      }
      for (int k = 0; k < 8; k++) {
        if (k >= count || nc >= 8) break;
        int pi = int(exPrim(first + k));
        float sg = exFetch(exPieces, 4 * pi + 1).w;
        if (sg >= exCut) continue;
        if (colourPass) {
          if (sg < firstSeg && exHolds(pi, p, eps) >= 0.0) { firstSeg = sg; colourPiece = pi; }
          continue;
        }
        // (already held: a piece's tool in stretches, its boxes apart)
        bool dup = false;
        for (int c = 0; c < 8; c++) { if (c >= nc) break; if (cand[c] == pi) dup = true; }
        if (dup) continue;
        float f = exHolds(pi, p, eps);
        if (f >= 0.0) { cand[nc] = pi; candF[nc] = f; nc++; }
      }
    }
    // where each leaves it: the furthest
    for (int c = 0; c < 8; c++) {
      if (c >= nc) break;
      float ex;
      vec3 nn;
      if (exExit(cand[c], candF[c], o, d, ex, nn) && ex > best) { best = ex; found = cand[c]; fn = nn; }
    }
    if (colourPass) break;
    if (found < 0) {
      if (hitPiece < 0) break;
      colourPass = true;
      colourPiece = hitPiece;
      firstSeg = exFetch(exPieces, 4 * hitPiece + 1).w;
      continue;
    }
    t = best;
    hitPiece = found;
    n = -fn;
    if (t >= t1) discard;
  }
  vec3 hitObj = o + d * t;
  exT = tb + t;
  exOp = 0.0;
  if (colourPiece >= 0) {
    float op = mod(exFetch(exPieces, 4 * colourPiece + 2).w, 65536.0);
    exOp = op;
    exColor = texelFetch(exOps, ivec2(int(op), 0), 0).rgb;
  } else {
    exColor = texelFetch(exOps, ivec2(0, 0), 0).rgb;
  }
  exNormalView = normalize(mat3(exMV) * n);
  exViewPos = -(exMV * vec4(hitObj, 1.0)).xyz;
  vec4 clip = exMVP * vec4(hitObj, 1.0);
  exDepth = 0.5 * clip.z / clip.w + 0.5;
}
`;

/** The exact stock: a three.js mesh (a box round the bar, its pixels ray-cast) in the part's
 *  frame. `data`: simExact's result. `opColor(tag)`: an operation's [r, g, b] (0: uncut). */
export class ExactStock {
  constructor(THREE, data, opColor) {
    this.THREE = THREE;
    this.count = data.count;
    const pieces = floats(data.pieces);
    const nodes = floats(data.nodes);
    const prims = floats(data.prims);
    this.textures = [dataTexture(THREE, pieces), dataTexture(THREE, nodes), dataTexture(THREE, prims)];
    // (what it was made from, for whoever looks: the tests)
    this.data = { pieces, nodes, prims, tools: data.tools, radius: data.radius, length: data.length, count: data.count };
    this.maxTag = 0;
    for (let i = 0; i < data.count; i++) this.maxTag = Math.max(this.maxTag, pieces[16 * i + 11] % 65536);
    const tools = new Array(16).fill(null).map(() => new THREE.Vector4());
    (data.tools || []).slice(0, 16).forEach((t, i) => tools[i].set(t[0], t[1], t[2], 0));
    this.opsTex = null;
    const u = (this.uniforms = {
      exPieces: { value: this.textures[0] },
      exNodes: { value: this.textures[1] },
      exPrims: { value: this.textures[2] },
      exOps: { value: null },
      exTools: { value: tools },
      exRadius: { value: data.radius },
      exLength: { value: data.length },
      exCut: { value: 0 },
      exOpCount: { value: 0 },
      exInvMVP: { value: new THREE.Matrix4() },
      exMVP: { value: new THREE.Matrix4() },
      exMV: { value: new THREE.Matrix4() },
      exCam: { value: new THREE.Vector3() },
      exViewport: { value: new THREE.Vector4() },
      exMaxSteps: { value: 256 },
      exLoop: { value: 1 },
    });
    this.recolor(opColor);
    const mat = new THREE.MeshStandardMaterial({ roughness: 0.55, metalness: 0.15, side: THREE.BackSide });
    mat.onBeforeCompile = (sh) => {
      Object.assign(sh.uniforms, u);
      sh.fragmentShader = sh.fragmentShader
        .replace("#include <common>", "#include <common>\n" + CAST)
        .replace("void main() {", "void main() {\n\texactCast();")
        .replace("#include <color_fragment>", "#include <color_fragment>\n\tdiffuseColor.rgb = exColor;")
        .replace("#include <normal_fragment_begin>", "#include <normal_fragment_begin>\n\tnormal = exNormalView;\n\tnonPerturbedNormal = exNormalView;")
        .replace("#include <lights_fragment_begin>", THREE.ShaderChunk.lights_fragment_begin.replaceAll("vViewPosition", "exViewPos"))
        .replace("#include <dithering_fragment>", "#include <dithering_fragment>\n\tgl_FragDepth = exDepth;");
    };
    // (its own program: three keys programs by the material's source)
    mat.customProgramCacheKey = () => "marcam-exact-stock";
    const r = data.radius + 1, len = data.length + 2;
    const geo = new THREE.BoxGeometry(len, 2 * r, 2 * r).translate(-data.length / 2, 0, 0);
    const mesh = (this.mesh = new THREE.Mesh(geo, mat));
    mesh.userData.exactStock = true;
    mesh.frustumCulled = false;
    this.budget = 65536;
    // the exact view, once drawn whole, in the main scene: its colour and depth as drawn
    this.quadMat = new THREE.ShaderMaterial({
      glslVersion: THREE.GLSL3,
      uniforms: { tColor: { value: null }, tDepth: { value: null } },
      vertexShader: "void main() { gl_Position = vec4(position.xy, 0.0, 1.0); }",
      fragmentShader: `uniform sampler2D tColor;
uniform sampler2D tDepth;
layout(location = 0) out highp vec4 pcOut;
void main() {
  ivec2 p = ivec2(gl_FragCoord.xy);
  float z = texelFetch(tDepth, p, 0).x;
  if (z >= 1.0) discard;
  vec4 c = texelFetch(tColor, p, 0);
  pcOut = linearToOutputTexel(vec4(c.rgb, 1.0));
  gl_FragDepth = z;
}`,
    });
    this.quad = new THREE.Mesh(new THREE.PlaneGeometry(2, 2), this.quadMat);
    this.quad.frustumCulled = false;
    this.quad.userData.exactStock = true;
    this.quad.visible = false;
    const mvp = new THREE.Matrix4(), mv = new THREE.Matrix4(), inv = new THREE.Matrix4(), vp = new THREE.Vector4();
    mesh.onBeforeRender = (renderer, scene, camera) => {
      mv.multiplyMatrices(camera.matrixWorldInverse, mesh.matrixWorld);
      mvp.multiplyMatrices(camera.projectionMatrix, mv);
      inv.copy(mvp).invert();
      u.exMV.value.copy(mv);
      u.exMVP.value.copy(mvp);
      u.exInvMVP.value.copy(inv);
      u.exCam.value.setFromMatrixPosition(camera.matrixWorld).applyMatrix4(inv.copy(mesh.matrixWorld).invert());
      inv.copy(mvp).invert();
      renderer.getCurrentViewport(vp);
      u.exViewport.value.copy(vp);
    };
  }

  /** Its program built without holding the page (the GPU's compiler takes seconds the first
   *  time on Windows; then the browser keeps it): resolves when it can be drawn. The mesh is
   *  not to be drawn before (a draw would build it there and then, the page stopped meanwhile). */
  compile(renderer, camera, scene) {
    const THREE = this.THREE;
    this.lights(scene);
    this.mesh.visible = true;
    // (built for where it is drawn, its own target: the program for the screen is another)
    const rt = new THREE.WebGLRenderTarget(1, 1, { type: THREE.HalfFloatType, depthTexture: new THREE.DepthTexture(1, 1) });
    const prev = renderer.getRenderTarget();
    renderer.setRenderTarget(rt);
    const p = renderer.compileAsync(this.scene, camera);
    renderer.setRenderTarget(prev);
    return p.then((x) => {
      rt.dispose();
      return x;
    });
  }

  /** Its own scene (drawn into its own target, a share at a time: see step), lit as `main`. */
  lights(main) {
    if (this.scene) return;
    const THREE = this.THREE;
    this.scene = new THREE.Scene();
    main.updateMatrixWorld();
    main.traverse((o) => {
      if (!o.isLight) return;
      const l = o.clone();
      o.matrixWorld.decompose(l.position, l.quaternion, l.scale);
      this.scene.add(l);
    });
    this.mesh.matrixAutoUpdate = false;
    this.scene.add(this.mesh);
  }

  /** The view drawn exact, a share of its pixels a frame (in tiles, as many as `budget` says:
   *  the GPU is never asked for a whole frame of it at once), started again whenever the view
   *  changes (the camera, the playback's place, the work's turn, the colours) and only once it
   *  has stayed so a moment. `work`: the part's frame. Returns whether it is still at it; its
   *  `complete`: the view drawn whole, to be shown (`quad`) instead of the tiles. */
  step(renderer, camera, main, work) {
    const THREE = this.THREE;
    this.lights(main);
    const size = renderer.getDrawingBufferSize(this._size || (this._size = new THREE.Vector2()));
    if (size.x < 1 || size.y < 1) return false;
    work.updateWorldMatrix(true, false);
    camera.updateMatrixWorld();
    // (the view as it was when this drawing started, within 1e-7 of each value's size: the
    // camera's controls compute it afresh each frame, its last bits jittering, and a view that
    // never stayed the same was never drawn exact)
    const vals = [...camera.matrixWorld.elements, ...camera.projectionMatrix.elements, ...work.matrixWorld.elements];
    const fixed = [size.x, size.y, this.uniforms.exCut.value, this.colorGen || 0].join();
    const same = this.vals && fixed === this.fixed && vals.every((x, i) => Math.abs(x - this.vals[i]) <= 1e-7 * Math.max(1, Math.abs(x)));
    const now = performance.now();
    if (!same) {
      this.vals = vals;
      this.fixed = fixed;
      // (a new view: a new key, for whoever waits on it to be drawn)
      this.key = (this.key || 0) + 1;
      this.changedAt = now;
      this.cursor = 0;
      this.complete = false;
      return true;
    }
    if (this.complete) return false;
    // (a moment's rest first: orbiting, scrubbing or playing, the tiles show)
    if (now - this.changedAt < 150) return true;
    if (!this.rt || this.rt.width !== size.x || this.rt.height !== size.y) {
      this.rt?.dispose();
      this.rt = new THREE.WebGLRenderTarget(size.x, size.y, { type: THREE.HalfFloatType, depthTexture: new THREE.DepthTexture(size.x, size.y) });
      this.quadMat.uniforms.tColor.value = this.rt.texture;
      this.quadMat.uniforms.tDepth.value = this.rt.depthTexture;
    }
    // (the budget follows the frames: fewer pixels when they come slow)
    if (this.lastStep) {
      const dt = now - this.lastStep;
      this.budget = dt > 34 ? Math.max(8192, this.budget * 0.6) : dt < 22 ? Math.min(1 << 22, this.budget * 1.4) : this.budget;
    }
    this.lastStep = now;
    this.mesh.matrix.copy(work.matrixWorld);
    this.mesh.matrixWorld.copy(work.matrixWorld);
    const T = 128;
    const cols = Math.ceil(size.x / T), rows = Math.ceil(size.y / T);
    const prev = renderer.getRenderTarget(), auto = renderer.autoClear;
    const cc = renderer.getClearColor(this._cc || (this._cc = new THREE.Color())), ca = renderer.getClearAlpha();
    renderer.setRenderTarget(this.rt);
    renderer.autoClear = false;
    if (this.cursor === 0) {
      this.rt.scissorTest = false;
      renderer.setRenderTarget(this.rt);
      renderer.setClearColor(0x000000, 0);
      renderer.clear(true, true, false);
      this.t0 = now;
    }
    this.rt.scissorTest = true;
    let used = 0;
    while (used < this.budget && this.cursor < cols * rows) {
      const cx = this.cursor % cols, cy = Math.floor(this.cursor / cols);
      const w = Math.min(T, size.x - cx * T), h = Math.min(T, size.y - cy * T);
      this.rt.scissor.set(cx * T, cy * T, w, h);
      // (three takes a target's scissor as it is set as the target)
      renderer.setRenderTarget(this.rt);
      renderer.render(this.scene, camera);
      used += w * h;
      this.cursor++;
    }
    this.rt.scissorTest = false;
    renderer.setRenderTarget(prev);
    renderer.autoClear = auto;
    renderer.setClearColor(cc, ca);
    this.progress = this.cursor / (cols * rows);
    if (this.log) this.log.push([Math.round(now - this.changedAt), this.cursor, Math.round(this.budget), Math.round(performance.now() - now)]);
    if (this.cursor >= cols * rows) {
      this.complete = true;
      this.lastMs = performance.now() - this.t0;
      this.settleMs = performance.now() - this.changedAt;
      this.lastStep = 0;
      return false;
    }
    return true;
  }

  /** The program built ahead, before any playback needs it (a small stand-in, kept: three
   *  keeps a program while a material uses it). */
  static prewarm(THREE, renderer, camera, scene) {
    if (ExactStock.warm) return ExactStock.warm;
    const one = new Float32Array(16);
    const stub = new ExactStock(THREE, { count: 0, pieces: one, nodes: new Float32Array(12), prims: new Float32Array(4), tools: [[1, 0, 0]], radius: 1, length: 1 }, () => [0, 0, 0]);
    ExactStock.stub = stub;
    ExactStock.warm = stub.compile(renderer, camera, scene).then(() => true);
    return ExactStock.warm;
  }

  /** Show the stock cut by the segments before `n`. */
  setCut(n) {
    this.uniforms.exCut.value = n;
  }

  /** The operations' colours again (tag -> [r, g, b]; 0 the uncut stock's). */
  recolor(opColor) {
    const THREE = this.THREE;
    this.colorGen = (this.colorGen || 0) + 1;
    const w = Math.max(2, this.maxTag + 1);
    const data = new Float32Array(w * 4);
    for (let i = 0; i < w; i++) {
      const c = opColor(i) || [1, 0, 1];
      data.set([c[0], c[1], c[2], 1], 4 * i);
    }
    this.opsTex?.dispose();
    const t = (this.opsTex = new THREE.DataTexture(data, w, 1, THREE.RGBAFormat, THREE.FloatType));
    t.minFilter = t.magFilter = THREE.NearestFilter;
    t.needsUpdate = true;
    this.uniforms.exOps.value = t;
    this.uniforms.exOpCount.value = w;
  }

  dispose() {
    this.quad.parent?.remove(this.quad);
    this.quad.geometry.dispose();
    this.quadMat.dispose();
    this.rt?.dispose();
    this.mesh.geometry.dispose();
    this.mesh.material.dispose();
    for (const t of this.textures) t.dispose();
    this.opsTex?.dispose();
  }
}
