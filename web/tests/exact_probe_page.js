// The page's half of exact_probe.mjs: a scene dumped by the engine's reference (the
// exact_view example's --dump: the pieces as the GPU takes them, a camera, and what the CPU's
// ray cast saw at each pixel) drawn by the GPU's ray cast (web/gpu/exact.js) into a float
// target, read back, and compared pixel by pixel; and how long a frame takes.
import * as THREE from "three";
import { CAST, ExactStock } from "../gpu/exact.js";

export async function run(url, { frames = 5, cut = null, debug = 0, rot = 0, std = false } = {}) {
  const dump = await (await fetch(url)).json();
  const { w, h } = dump;
  const renderer = new THREE.WebGLRenderer({ antialias: false });
  renderer.setPixelRatio(1);
  renderer.setSize(w, h);
  document.body.appendChild(renderer.domElement);
  const arr = (x) => (typeof x === "string" ? x : new Float32Array(x));
  const data = { ...dump, pieces: arr(dump.pieces), nodes: arr(dump.nodes), prims: arr(dump.prims) };
  const ex = new ExactStock(THREE, data, (tag) => [tag / 255, 0, 0]);
  ex.setCut(cut ?? dump.cut);
  if (debug === 2) ex.uniforms.exMaxSteps.value = 0;
  const mat = new THREE.ShaderMaterial({
    glslVersion: THREE.GLSL3,
    uniforms: ex.uniforms,
    side: THREE.BackSide,
    vertexShader: "void main() { gl_Position = projectionMatrix * modelViewMatrix * vec4(position, 1.0); }",
    fragmentShader: CAST + "\nlayout(location = 0) out highp vec4 exOut;\nvoid main() { " + (debug === 1 ? "exOut = vec4(5.0, exCam.x, exViewport.z, 1.0); return;" : "") + " exactCast(); exOut = vec4(exOp, exT, exSteps, 1.0); gl_FragDepth = exDepth; }",
  });
  if (!std) ex.mesh.material = mat;
  const scene = new THREE.Scene();
  if (std) {
    scene.add(new THREE.AmbientLight(0xffffff, 0.6));
    const sun = new THREE.DirectionalLight(0xffffff, 1.2);
    sun.position.set(20, -30, 40);
    scene.add(sun);
  }
  scene.add(ex.mesh);
  const cam = new THREE.PerspectiveCamera(dump.camera.fov, w / h, 0.1, 2000);
  cam.up.set(0, 0, 1);
  cam.position.fromArray(dump.camera.eye);
  cam.lookAt(new THREE.Vector3().fromArray(dump.camera.target));
  if (rot) {
    // (the work turned by rot about X and the camera with it: the same picture)
    ex.mesh.rotation.x = rot;
    const R = new THREE.Matrix4().makeRotationX(rot);
    cam.position.applyMatrix4(R);
    cam.up.set(0, 0, 1).applyMatrix4(R);
    cam.lookAt(new THREE.Vector3().fromArray(dump.camera.target).applyMatrix4(R));
  }
  cam.updateMatrixWorld();
  const rt = new THREE.WebGLRenderTarget(w, h, { type: THREE.FloatType, format: THREE.RGBAFormat, depthBuffer: true });
  renderer.setRenderTarget(rt);
  renderer.setClearColor(new THREE.Color(0, 0, 0), 0);
  const gl = renderer.getContext();
  const px = new Float32Array(w * h * 4);
  const t0 = performance.now();
  renderer.clear();
  renderer.render(scene, cam);
  renderer.readRenderTargetPixels(rt, 0, 0, w, h, px);
  const first = performance.now() - t0;
  const times = [];
  for (let i = 0; i < frames; i++) {
    const a = performance.now();
    renderer.clear();
    renderer.render(scene, cam);
    renderer.readRenderTargetPixels(rt, 0, 0, w, h, px);
    times.push(performance.now() - a);
  }
  times.sort((a, b) => a - b);
  // compare: the reference is top row first, the read back bottom row first
  const ref = dump.ref;
  const opAt = (x, y) => ref[2 * (y * w + x)];
  const pixel = (2 * Math.tan((dump.camera.fov * Math.PI) / 360)) / h;
  let redder = 0, cutSeen = 0, seen = 0, opDiff = 0, opFar = 0, tWorst = 0, steps = 0, stepsMax = 0;
  const far = [];
  for (let y = 0; y < h; y++) {
    for (let x = 0; x < w; x++) {
      const g = 4 * ((h - 1 - y) * w + x);
      const gOp = px[g + 3] > 0 ? Math.round(px[g]) : -1;
      const gT = px[g + 1];
      const rOp = opAt(x, y), rT = ref[2 * (y * w + x) + 1];
      if (gOp > 0) cutSeen++;
      if (px[g] > px[g + 1] + 0.001) redder++;
      if (gOp >= 0) {
        steps += px[g + 2];
        stepsMax = Math.max(stepsMax, px[g + 2]);
      }
      if (rOp >= 0 || gOp >= 0) seen++;
      if (gOp !== rOp) {
        opDiff++;
        // beyond a pixel: no pixel round it in the reference is what the GPU saw
        let near = false;
        for (let dy = -1; dy <= 1 && !near; dy++)
          for (let dx = -1; dx <= 1 && !near; dx++) {
            const xx = x + dx, yy = y + dy;
            if (xx >= 0 && yy >= 0 && xx < w && yy < h && opAt(xx, yy) === gOp) near = true;
          }
        if (!near) {
          opFar++;
          if (far.length < 10) far.push({ x, y, gpu: gOp, ref: rOp, gT, rT });
        }
      } else if (gOp >= 0) {
        // the depth's error in pixels at that distance
        tWorst = Math.max(tWorst, Math.abs(gT - rT) / (pixel * rT));
      }
    }
  }
  renderer.setRenderTarget(null);
  return JSON.stringify({ redder, cutSeen, sample: [...px.slice(4 * (32 * w + 48), 4 * (32 * w + 48) + 4)], w, h, pieces: dump.count, firstMs: Math.round(first), frameMs: times[times.length >> 1], seen, opDiff, opFar, far, tWorstPx: tWorst, meanSteps: steps / Math.max(1, seen), stepsMax });
}
