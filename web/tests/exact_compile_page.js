// The page's half of exact_compile.mjs: the exact stock's ray cast compiled in a bare WebGL2
// program, whole and with parts stubbed out, each timed (ms).
export async function run() {
  const { CAST } = await import("../gpu/exact.js");
  const out = {};
  const variants = {
    full: (s) => s,
    again: (s) => s,
    straightOnly: (s) => s.replace("bool turning = T3.y > 0.5;", "bool turning = false; if (T3.y > 0.5) return false;"),
    noExtent: (s) => s.replace("e.ok = false; e.t0 = 0.0; e.t1 = 0.0; e.s1 = 0.0; e.tag1 = 0;", "e.ok = false; e.t0 = 0.0; e.t1 = 0.0; e.s1 = 0.0; e.tag1 = 0; if (exLoop > 0) return e;"),
    noSpanAt: (s) => s.replace("bool exSpanAt(int pi, vec3 o, vec3 d, float tq, float eps, out float exit, out vec3 nrm) {", "bool exSpanAt(int pi, vec3 o, vec3 d, float tq, float eps, out float exit, out vec3 nrm) { exit = 0.0; nrm = vec3(0.0); if (exLoop > 0) return false;"),
    noMachine: (s) => s.replace("int mode = turning ? 0 : 10;", "int mode = turning ? 0 : 10; if (turning) return false;").replace("for (int it = 0; it < 512 * exLoop; it++) {", "for (int it = 0; it < 1; it++) {"),
    noSd: (s) => s.replace("float exSdTool(vec4 tool, vec3 tip, vec3 a, float len, vec3 x) {", "float exSdTool(vec4 tool, vec3 tip, vec3 a, float len, vec3 x) { if (exLoop > 0) return 0.0;"),
  };
  for (const [name, f] of Object.entries(variants)) {
    const c = document.createElement("canvas");
    const gl = c.getContext("webgl2");
    const vs = gl.createShader(gl.VERTEX_SHADER);
    gl.shaderSource(vs, "#version 300 es\nin vec3 p; void main(){ gl_Position = vec4(p,1.0); }");
    gl.compileShader(vs);
    const fs = gl.createShader(gl.FRAGMENT_SHADER);
    const src = "#version 300 es\nprecision highp float;\nprecision highp int;\n" + f(CAST) + "\nout vec4 col;\nvoid main(){ exactCast(); col = vec4(exColor * max(exNormalView.z, 0.0), 1.0); gl_FragDepth = exDepth; }";
    gl.shaderSource(fs, src);
    const t0 = performance.now();
    gl.compileShader(fs);
    const pr = gl.createProgram();
    gl.attachShader(pr, vs);
    gl.attachShader(pr, fs);
    gl.linkProgram(pr);
    const ok = gl.getProgramParameter(pr, gl.LINK_STATUS);
    out[name] = { ms: Math.round(performance.now() - t0), ok, log: ok ? "" : (gl.getShaderInfoLog(fs) || gl.getProgramInfoLog(pr)).slice(0, 400) };
    const e = gl.getExtension("WEBGL_debug_renderer_info");
    out.renderer = e ? gl.getParameter(e.UNMASKED_RENDERER_WEBGL) : "";
  }
  return JSON.stringify(out);
}
