// WebGPU stock simulation for the web app, with the same shader as the native crate
// (web/gpu/stamp.wgsl). The engine (WASM) says where the tool is (gpuOpen: tool positions
// sampled exactly as its CPU simulation samples them); this runs them on the GPU and reads
// back the stock. Heights agree with the CPU within 1 µm (see docs/gpu.md). Callers fall
// back to the WASM simulation when there is no WebGPU, no adapter, or gpuOpen refuses a job.

const POS_WORDS = 16;
const DISP_STRIDE = 256; // dynamic uniform offset alignment
const BATCH = 65535; // positions per dispatch (the dispatch's Y size)

let devicePromise = null;

/** The GPU, once: {device, pipeline, layout, name}, or null without WebGPU. */
export function gpuDevice() {
  if (!devicePromise) {
    devicePromise = (async () => {
      if (!globalThis.navigator?.gpu) return null;
      try {
        const adapter = await navigator.gpu.requestAdapter({ powerPreference: "high-performance" });
        if (!adapter) return null;
        const lim = adapter.limits;
        const device = await adapter.requestDevice({
          requiredLimits: {
            maxStorageBufferBindingSize: lim.maxStorageBufferBindingSize,
            maxBufferSize: lim.maxBufferSize,
          },
        });
        const code = await (await fetch(new URL("./stamp.wgsl", import.meta.url))).text();
        const module = device.createShaderModule({ code });
        const buf = (type, hasDynamicOffset = false) => ({ visibility: GPUShaderStage.COMPUTE, buffer: { type, hasDynamicOffset } });
        const layout = device.createBindGroupLayout({
          entries: [
            { binding: 0, ...buf("uniform") },
            { binding: 1, ...buf("read-only-storage") },
            { binding: 2, ...buf("storage") },
            { binding: 3, ...buf("uniform", true) },
            { binding: 4, ...buf("storage") },
          ],
        });
        const pipeline = await device.createComputePipelineAsync({
          layout: device.createPipelineLayout({ bindGroupLayouts: [layout] }),
          compute: { module, entryPoint: "main" },
        });
        const info = adapter.info || {};
        const name = info.description || [info.vendor, info.architecture].filter(Boolean).join(" ") || "WebGPU";
        device.lost.then(() => (devicePromise = null));
        return { device, pipeline, layout, name };
      } catch (e) {
        console.warn("WebGPU unavailable:", e);
        return null;
      }
    })();
  }
  return devicePromise;
}

// (the worker decodes the engine's base64 into bytes)
const b64 = (s) => (typeof s === "string" ? Uint8Array.from(atob(s), (c) => c.charCodeAt(0)) : s);
const u32 = (s) => {
  const b = b64(s);
  return new Uint32Array(b.buffer, b.byteOffset, b.byteLength / 4);
};

/** A stock on the GPU for one gpuOpen result; advances segment by segment. */
export class GpuStock {
  constructor(gpu, open) {
    const { device, layout } = gpu;
    this.gpu = gpu;
    this.open = open;
    this.kind = open.kind;
    const g = open.grid;
    this.rows = open.kind === "xyz" ? g.ny : g.nx;
    this.cols = open.kind === "xyz" ? g.nx : g.na;
    const inp = open.input;
    this.words = u32(inp.words);
    this.segStart = u32(inp.seg_start);
    this.segRapid = b64(inp.seg_rapid);
    this.progSeg = inp.prog_seg;
    this.segments = this.segRapid.length;
    this.initial = u32(open.keys);
    this.pos = 0; // segments applied
    const cells = this.initial.length;
    const S = GPUBufferUsage;
    const make = (size, usage) => device.createBuffer({ size: Math.max(16, size), usage });
    this.keys = make(cells * 4, S.STORAGE | S.COPY_SRC | S.COPY_DST);
    device.queue.writeBuffer(this.keys, 0, this.initial);
    this.hits = make(Math.max(1, this.segments) * 4, S.STORAGE | S.COPY_SRC | S.COPY_DST);
    this.posBuf = make(this.words.byteLength, S.STORAGE | S.COPY_DST);
    if (this.words.length) device.queue.writeBuffer(this.posBuf, 0, this.words);
    const grid = new ArrayBuffer(32);
    const f = new Float32Array(grid), u = new Uint32Array(grid);
    if (open.kind === "xyz") {
      f[0] = g.x0;
      f[1] = g.cell;
      f[2] = g.y0;
      f[3] = open.zbase;
      u[4] = g.nx;
      u[5] = g.ny;
    } else {
      f[0] = g.x0;
      f[1] = g.dx;
      u[4] = g.nx;
      u[5] = g.na;
    }
    this.gridBuf = make(32, S.UNIFORM | S.COPY_DST);
    device.queue.writeBuffer(this.gridBuf, 0, grid);
    // snapshots every `every` segments (about 256 MB of them at most)
    this.every = Math.max(500, Math.ceil((this.segments * cells * 4) / (256 << 20)));
    this.snaps = new Map();
  }

  /** Dispatches for segments a..b: feed runs, each rapid segment alone; [first, count]. */
  runs(a, b) {
    const out = [];
    let s = a;
    while (s < b) {
      let e = s + 1;
      if (!this.segRapid[s]) while (e < b && !this.segRapid[e]) e++;
      let p = this.segStart[s];
      const q = this.segStart[e];
      while (p < q) {
        const n = Math.min(q - p, BATCH);
        out.push([p, n]);
        p += n;
      }
      s = e;
    }
    return out;
  }

  /** Rows the positions of segments a..b touch: [lo, hi] or null. */
  touched(a, b) {
    const w = this.words;
    let lo = Infinity, hi = -1;
    const xyz = this.kind === "xyz";
    for (let p = this.segStart[a]; p < this.segStart[b]; p++) {
      const o = p * POS_WORDS;
      const r0 = xyz ? w[o + 10] | 0 : w[o + 8];
      const r1 = xyz ? w[o + 11] | 0 : w[o + 9];
      if (r0 < lo) lo = r0;
      if (r1 > hi) hi = r1;
    }
    return hi < 0 ? null : [Math.max(0, lo), Math.min(this.rows - 1, hi)];
  }

  dispatch(a, b) {
    const { device, pipeline, layout } = this.gpu;
    const runs = this.runs(a, b);
    if (!runs.length) return;
    const disp = new Uint32Array((runs.length * DISP_STRIDE) / 4);
    const sizes = [];
    runs.forEach(([first, count], k) => {
      let wi = 1, wj = 1;
      for (let p = first; p < first + count; p++) {
        const o = p * POS_WORDS;
        wi = Math.max(wi, this.words[o + 9] - this.words[o + 8] + 1);
        wj = Math.max(wj, (this.words[o + 11] | 0) - (this.words[o + 10] | 0) + 1);
      }
      disp.set([first, count, wi, wj], (k * DISP_STRIDE) / 4);
      sizes.push([Math.ceil((wi * wj) / 64), count]);
    });
    const dispBuf = device.createBuffer({ size: disp.byteLength, usage: GPUBufferUsage.UNIFORM | GPUBufferUsage.COPY_DST });
    device.queue.writeBuffer(dispBuf, 0, disp);
    const bind = device.createBindGroup({
      layout,
      entries: [
        { binding: 0, resource: { buffer: this.gridBuf } },
        { binding: 1, resource: { buffer: this.posBuf } },
        { binding: 2, resource: { buffer: this.keys } },
        { binding: 3, resource: { buffer: dispBuf, size: 16 } },
        { binding: 4, resource: { buffer: this.hits } },
      ],
    });
    const enc = device.createCommandEncoder();
    const pass = enc.beginComputePass();
    pass.setPipeline(pipeline);
    // in order: each dispatch sees the stock the ones before it left
    sizes.forEach(([gx, gy], k) => {
      pass.setBindGroup(0, bind, [k * DISP_STRIDE]);
      pass.dispatchWorkgroups(gx, gy);
    });
    pass.end();
    device.queue.submit([enc.finish()]);
    dispBuf.destroy?.();
  }

  /** Apply segments up to `n` (backwards: from the nearest snapshot). Returns the rows that
   * may have changed: [lo, hi], or null. */
  seek(n) {
    const { device } = this.gpu;
    n = Math.max(0, Math.min(n, this.segments));
    let rows = null;
    const join = (r) => {
      if (r) rows = rows ? [Math.min(rows[0], r[0]), Math.max(rows[1], r[1])] : r;
    };
    if (n < this.pos) {
      let at = 0;
      for (const k of this.snaps.keys()) if (k <= n && k > at) at = k;
      const enc = device.createCommandEncoder();
      if (at > 0) enc.copyBufferToBuffer(this.snaps.get(at), 0, this.keys, 0, this.initial.byteLength);
      device.queue.submit([enc.finish()]);
      if (at === 0) device.queue.writeBuffer(this.keys, 0, this.initial);
      // the whole stock may differ from what is on screen
      rows = [0, this.rows - 1];
      this.pos = at;
    }
    while (this.pos < n) {
      const stop = Math.min(n, (Math.floor(this.pos / this.every) + 1) * this.every);
      join(this.touched(this.pos, stop));
      this.dispatch(this.pos, stop);
      this.pos = stop;
      if (this.pos % this.every === 0 && !this.snaps.has(this.pos)) {
        const snap = device.createBuffer({ size: this.initial.byteLength, usage: GPUBufferUsage.COPY_SRC | GPUBufferUsage.COPY_DST });
        const enc = device.createCommandEncoder();
        enc.copyBufferToBuffer(this.keys, 0, snap, 0, this.initial.byteLength);
        device.queue.submit([enc.finish()]);
        this.snaps.set(this.pos, snap);
      }
    }
    return rows;
  }

  async read(buf, offset, bytes) {
    const { device } = this.gpu;
    const r = device.createBuffer({ size: Math.max(4, bytes), usage: GPUBufferUsage.MAP_READ | GPUBufferUsage.COPY_DST });
    const enc = device.createCommandEncoder();
    enc.copyBufferToBuffer(buf, offset, r, 0, bytes);
    device.queue.submit([enc.finish()]);
    await r.mapAsync(GPUMapMode.READ);
    const out = new Uint32Array(r.getMappedRange().slice(0));
    r.unmap();
    r.destroy();
    return out;
  }

  /** Keys of rows lo..hi. */
  readRows(lo, hi) {
    return this.read(this.keys, lo * this.cols * 4, (hi - lo + 1) * this.cols * 4);
  }

  /** Per segment: 2 if it was a rapid through the stock, 1 one that grazed it. */
  readHits() {
    return this.read(this.hits, 0, Math.max(1, this.segments) * 4);
  }

  /** Heights (radius or Z) and tags from keys. */
  decode(keys) {
    const h = new Float32Array(keys.length), op = new Uint16Array(keys.length);
    const base = this.kind === "xyz" ? this.open.zbase : 0;
    for (let k = 0; k < keys.length; k++) {
      h[k] = base + (keys[k] >>> 8) / 1000;
      op[k] = keys[k] & 255;
    }
    return { h, op };
  }

  destroy() {
    for (const b of [this.keys, this.hits, this.posBuf, this.gridBuf, ...this.snaps.values()]) b.destroy?.();
  }
}

/**
 * The whole job, program by program, as the engine's `simulate` returns it: {kind, grid,
 * part, stock_radius | stock_box, stocks: [{name, r | z, max_left, ..., rapid_hits}]}.
 * `stats(opts, keyBytes)` is the engine's gpuStats.
 */
export async function simulateOnGpu(gpu, open, stats, progress) {
  const gs = new GpuStock(gpu, open);
  try {
    const stocks = [];
    // (the work done told as it goes: the moves in steps of a hundredth, each waited for)
    const all = gs.segments || 1;
    const step = Math.max(1, Math.ceil(all / 100));
    progress?.(0);
    for (let p = 0; p < open.programs.length; p++) {
      const [a, b] = [gs.progSeg[p], gs.progSeg[p + 1]];
      if (progress) {
        for (let s = Math.min(b, a + step); s < b; s = Math.min(b, s + step)) {
          gs.seek(s);
          await gpu.device.queue.onSubmittedWorkDone();
          progress(s / all);
        }
      }
      gs.seek(b);
      const keys = await gs.readRows(0, gs.rows - 1);
      const hits = await gs.readHits();
      // (2: through the stock; 1: a graze only, said as a warning)
      let rapid = 0, graze = 0;
      // (which: their segments, for the engine to check on the 3D stock and name their moves)
      const flagged = [];
      for (let s = a; s < b; s++) {
        if (hits[s] >= 2) rapid++;
        else if (hits[s] === 1) graze++;
        if (hits[s] >= 1) flagged.push([s, hits[s] >= 2 ? 2 : 1]);
      }
      const st = await stats({ rapid_hits: rapid, rapid_grazes: graze }, new Uint8Array(keys.buffer));
      const { h } = gs.decode(keys);
      stocks.push({ name: open.programs[p], samples: gs.segStart[b] - gs.segStart[a], ...st, [gs.kind === "xyz" ? "z" : "r"]: h, flagged });
      progress?.(b / all);
    }
    return { kind: open.kind, grid: open.grid, part: open.part, stock_radius: open.stock_radius, stock_box: open.stock_box, stocks };
  } finally {
    gs.destroy();
  }
}
