# GPU acceleration: findings and plan

**Question.** Mar asked whether marcam should use the GPU when one is available, natively with
wgpu (Vulkan/DX12/Metal) and in the browser with WebGPU. Any GPU path needs an automatic
CPU fallback, and the G-code must not depend on whether a GPU is present.

This note records:
- what takes time today;
- what cheaper CPU changes gained;
- where a GPU can help without risking G-code determinism.

Every number here comes from `cargo run --release -p marcam-core --example profile` on the
development PC.

## The machine

- CPU: Intel Core i9-13900K, 24 cores / 32 threads; 64 GB RAM.
- GPU: NVIDIA GeForce RTX 5080, plus the Intel UHD Graphics 770 in the CPU.
- Headless CI has no GPU, so every GPU path needs a CPU fallback, and the tests must pass
  without a GPU.

## Where the time goes (single thread, before any change)

| Example | Stage | ms | Size |
| --- | --- | ---: | --- |
| knob (Z1) | plan (turning, knurl, chamfer) | 3 | 36 914 moves |
| knob (Z1) | post / verify / time estimate | 37 / 18 / 57 | 36 933 lines |
| knob (Z1) | gouge check | 632 | 21 649 cut moves |
| knob (Z1) | simulate, Normal grid (0.1 mm x 1.5°) | 1 925 | 66 480 cells |
| knob (Z1) | simulate, Fine grid (0.05 mm x 0.5°) | **22 849** | 398 880 cells |
| knob (Z1) | playback to the end (Normal) | 4 194 | 36 898 segments |
| knob mesh finish (Z1) | plan (rotary rough + 2 finishes) | **14 450** | 51 032 moves |
| knob mesh finish (Z1) | gouge check | **13 149** | 50 918 cut moves |
| knob mesh finish (Z1) | simulate, Fine | 16 980 | 399 600 cells |
| plate (Carvera) | plan (incl. 3D raster over the dome) | **21 221** | 16 929 moves |
| plate (Carvera) | simulate 3-axis, Fine | 805 | 153 600 cells |
| plaque (Carvera) | plan (image relief rough + finish + engraving) | 4 965 | 95 894 moves |
| plaque (Carvera) | simulate 3-axis, Fine | 1 385 | 230 400 cells |
| ring (Z1) | simulate, Fine | 13 126 | 734 400 cells |
| ring (Z1) | playback to the end (Normal) | 2 270 | 22 650 segments |

Post, verify and the time estimate never take more than 0.2 s. The cost is in two places:
- **Drop-cutter queries**, used for planning and gouge checks.
- **Rotary stock simulation and playback**, which grow with grid resolution: Fine is about
  6 times Normal's cells and about 10 times its time.

## Cheaper CPU wins first (done, exact)

Three changes to the drop-cutters' spatial index return exactly the same answers, because a
maximum doesn't depend on the order it's taken in:
- **Sort each bin by the triangles' top**, and stop at the first triangle that can't beat
  the best contact found. Rotary bins sort by largest radius, 3-axis bins by top Z.
- **3-axis bins of half the tool radius**, plus a bounding-box test before the exact contact
  calculation.
- **Compute each surface point once per 3-D raster**, not once per step-down level.

| Stage | Before | After |
| --- | ---: | ---: |
| plate: plan | 21 221 ms | **1 467 ms** (14x) |
| plaque: plan | 4 965 ms | **1 825 ms** |
| knob mesh finish: plan | 14 450 ms | **7 396 ms** |
| knob mesh finish: gouge check | 13 149 ms | **6 723 ms** |

All 12 G-code files of the five examples are byte-identical before and after, and the Z1
goldens are unchanged.

## Determinism: why the GPU must not write G-code

WebGPU's shading language (WGSL) has no 64-bit floats, and native `f64` on consumer GPUs is
optional and slow. A drop-cutter evaluated on the GPU in 32-bit floats differs from the CPU
in the last digits. After rounding to the post's 3 decimals that changes some coordinates,
so the G-code would depend on whether a GPU was present. Bit-exact `f32` emulation of the
`f64` code is not realistic.

**Rule: toolpaths and G-code are always computed on the CPU.** The GPU is only for:
- material-removal simulation and playback, compared with the CPU within a stated tolerance;
- display;
- maybe acceleration structures whose answers are re-checked exactly on the CPU.

## Options for what remains

1. **CPU threads, bit-identical** (`std::thread::scope` or rayon).
   - *Simulation:* split the grid into bands of rows, one per thread. Each thread replays
     every tool position in order but stamps only its own rows. Each cell sees its stamps
     in the same order as single-threaded, so heights, operation tags and counts are
     identical.
   - *Drop-cutter:* evaluate the sample points of a raster row or pass in parallel, then
     emit the moves in order.
   - *Reach:* CLI, Python and native hosts get up to the core count. In the browser, WASM
     threads need cross-origin isolation (COOP/COEP headers) and a threaded build, so the
     web app stays single-threaded unless it is served that way.
2. **GPU stamping of the stock heightmap** (wgpu natively, WebGPU in the browser).
   - *Method:* one compute pass per batch of tool positions. A thread per (position, cell
     under the tool) computes the cut height and keeps the lowest with `atomicMin` on a
     quantized height (1 µm), with the operation tag packed in the low bits.
   - *Fit:* the simulation is all independent minimum operations, so it suits a GPU,
     especially Fine/Ultra grids and the browser's playback.
   - *Differences from the CPU:* `f32` and the 1 µm quantization; on equal heights the tag
     is the lowest, not the last one written. These matter for preview and reports only.
   - *Costs:* wgpu is a large dependency. The browser needs a separate JS + WGSL path,
     because our WASM build uses raw exports with no wasm-bindgen; the WGSL can be shared.
     WebGPU ships in Chrome and Edge, with partial support elsewhere.
3. **GPU drop-cutter for previews only** (for example interactive range edits). Not
   recommended: planning is now fast enough, and the G-code must come from the CPU anyway.

## Recommended plan

- **A. Exact CPU index changes.** Done; numbers above.
- **B. Threaded simulation and drop-cutter, bit-identical, on by default natively.** Tested
  by comparing against the single-threaded results bit for bit. This removes most of the
  Fine-grid wait in the CLI and Python with no new dependency.
- **C. GPU simulation prototype in a separate crate** (`marcam-gpu`, wgpu), behind a
  feature flag, falling back to the CPU when no adapter is found. Tests compare the GPU
  heightmap with the CPU's within the quantization, and skip cleanly without a GPU. A
  benchmark runs on the knob and ring at Fine and Ultra.
- **D. If C pays off: WebGPU in the web app** for Simulate and machining playback at fine
  grids. The same WGSL is driven from JS, with the WASM simulation as fallback.
- **Never:** GPU for toolpaths, G-code, the verifier or the time estimates.

Results of B and C are added below as they land.

## B: threaded simulation (done)

`sim::apply_banded` splits the rotary grid into bands of rows, one per thread, using
`std::thread::scope` with no new dependency. Every band replays every tool position in order
and stamps only its own rows. The stock, the operation tags and the statistics are identical
to the single-threaded run; a test compares them bit for bit at 1, 3 and 7 threads on the
indexed knob.

`simulate` uses all cores natively (`MARCAM_THREADS` overrides) and one thread on wasm.
Playback runs long seeks the same way, between snapshots.

| Stage | One thread | Threads |
| --- | ---: | ---: |
| knob: simulate, Normal | 1 925 ms | 262 ms |
| knob: simulate, Fine | 22 849 ms | **1 355 ms** (17x) |
| knob: playback to the end | 4 194 ms | 331 ms |
| knob mesh finish: simulate, Fine | 16 980 ms | 2 324 ms |
| ring: simulate, Fine | 13 126 ms | 3 703 ms |

The ring gains less because its cutting is concentrated in a few X bands, so most threads
finish early. Interleaving rows across threads would even that out.

## C: GPU simulation prototype (done)

**The crate.** `crates/marcam-gpu` uses wgpu 25 and one WGSL compute shader. It is not in
the default build. The CLI uses it with `cargo build -p marcam-cli --features gpu`, which
adds `marcam simulate --gpu`. `simulate_or_cpu` falls back to the CPU simulation in two cases:
- when there is no hardware adapter (software adapters count as none);
- when a job has indexed 3+1 moves or more than 255 operations.

**How it works.**
- The CPU samples the moves exactly as `sim::apply_segment` does and gives each tool
  position its cell window, in f64.
- The shader evaluates the CPU's ray-entry iteration per (position, cell) in f32 and keeps
  the lowest with `atomicMin` on height (µm) << 8 | tag.

**Agreement with the CPU** (`cargo test -p marcam-gpu`, on the RTX 5080):

| Example | Tool positions | Largest height difference | Tags that differ |
| --- | ---: | ---: | ---: |
| knob (turning, knurl, chamfer) | 299 146 | 0.001 mm | 0.000 % |
| ring (wrapped grooves, text, V-carve) | 69 354 | 0.001 mm | 0.000 % |

The test also checks that an indexed job falls back and gives the plain CPU result.

**Bug found on the way.** The test caught a portability bug. On this machine's backend,
`%` of a negative `i32` behaved like the unsigned remainder, so cells just before angle 0
were written to the wrong column. The shader now keeps the index positive before wrapping.

**Speed** (`cargo run --release -p marcam-gpu --example bench`; whole job, GPU time
includes upload and readback):

| Example | Grid | Cells | CPU 1 thread | CPU 32 threads | RTX 5080 |
| --- | --- | ---: | ---: | ---: | ---: |
| knob | Normal | 66 480 | 1 980 ms | 240 ms | **22 ms** |
| knob | Fine | 398 880 | 23 402 ms | 2 565 ms | **44 ms** |
| knob | Ultra | 1 595 520 | - | 17 887 ms | **136 ms** |
| ring | Normal | 122 400 | 2 321 ms | 639 ms | **11 ms** |
| ring | Fine | 734 400 | 13 283 ms | 3 693 ms | **16 ms** |
| ring | Ultra | 2 937 600 | - | 27 940 ms | **51 ms** |

The GPU is 50 to 500 times faster than all CPU threads, and the gap widens with the grid.
The shader is the same one a WebGPU path in the browser would use (step D). The prototype
does not yet simulate on the GPU:
- indexed moves;
- 3-axis (block) grids;
- playback (one tool position at a time is a poor fit; it needs batched seeks);
- the report statistics, which still come from the CPU run.

## D: WebGPU in the web app (done)

**What changed.**
- **One shader, two hosts.** `web/gpu/stamp.wgsl` is used natively (`marcam-gpu`) and in
  the browser. It has two modes: rotary (radial stamp) and block (3-axis heightmap).
- **Inputs from the engine.** The tool positions come from `marcam::gpu_input`, which has
  no GPU dependency, so the WASM build provides them: `gpuOpen` returns the positions,
  per-segment and per-program ranges, rapid flags, starting keys and the part map.
- **Report numbers stay in the engine.** `gpuStats` turns a GPU stock into its report
  (material left, gouges, intended-feature cells) with the same rules as the CPU.
- **Rapids through stock, on the GPU too.** Each rapid segment gets a dispatch of its own,
  after the cuts before it (WebGPU keeps dispatches in order). The shader's `atomicMin`
  returns the old height, and a rapid that lowers a cell by more than 1 µm flags its
  segment. A native test checks the GPU flags the same move as the CPU.
- **Browser runner.** `web/gpu/gpusim.js` is used by Simulate and by machining playback.
  Playback keeps the stock on the GPU, reads back only the rows a seek touched, and keeps
  GPU-side snapshots so jumping back is a buffer copy.
- **Fallback.** It falls back to the WASM simulation, and says why next to the grid line
  ("last run: GPU (...)" / "CPU (...)"), in these cases:
  - no WebGPU in the browser;
  - no adapter;
  - "use the GPU" unticked;
  - jobs the engine keeps on the CPU: indexed 3+1 moves, more than 255 operations.

**Why indexed moves stay on the CPU.** Their stamp cuts a cell only when its surface point
is already inside the tool. That depends on the order cuts arrive in, which concurrent GPU
threads can't reproduce.

**Checks.** Native, against the CPU: knob, ring, plate and plaque all within 1 µm, with
identical tags and the same rapids through stock. Browser e2e (headless Chrome with
`--enable-unsafe-webgpu`; on this PC it gets the RTX 5080):

| Check | Result |
| --- | --- |
| Simulate, knob (rotary): GPU vs CPU | within 0.001 mm, identical report numbers |
| Simulate, plate (3-axis): GPU vs CPU | within 0.001 mm, identical report numbers |
| Playback to the end on the GPU vs the CPU's batch stock | within 0.001 mm, label "GPU" |
| Indexed knob with the GPU on | falls back: "CPU (indexed 3+1 moves are simulated on the CPU)" |

Where there is no WebGPU, as on CI, the same checks assert the labelled fallback.

**Speed in the browser.** Knob at Ultra (1.6M cells), machining playback on the GPU: to the
last of 36 898 moves in 0.26 s; back to the middle in 0.16 s.


## The playback's stock drawn exact (ray cast per pixel)

Mar: "there cant be visual artifacts as that defeats the point of the sim". Any mesh made from
the tri-dexel's 0.25 mm grid shows that grid at some zoom, however it is smoothed. So the
stock the playback shows, once the view rests, is not a mesh. Each pixel's ray meets the bar
minus the tool's swept volumes, worked out on the GPU (`web/gpu/exact.js`, WebGL2 inside
three.js's own material, so it is lit and depth-tested like the rest of the scene). The
reference for it, on the CPU, is `crates/marcam-core/src/exact.rs`.

- **Pieces.** Every cutting move becomes pieces. A straight move with A held is one piece,
  exact: the tool (flat, ball or V, as the tri-dexel takes it) swept along a segment is convex,
  so a ray meets it in one interval. Each tool part is one quadric and a few linear constraints
  in (t, s), and the interval's ends are found in closed form. A turning move (rotary, or a
  pose that turns) is cut into pieces of at most 2°, each seen in poses near enough that a ray
  meeting the tool between two of them comes within 0.5 µm of one. The tool's distance to a
  point changes no faster than the piece's speed (a Lipschitz bound), so a few poses prove a
  point out of it, or find the pose holding it (a golden search between poses).
- **A ray.** From where it enters the bar: while its point is inside a piece played before the
  playback's place, it moves on to where that piece's volume ends along it, the furthest of
  those found. Where it stops is the surface: the bar's own, or the exit of the piece that cut
  it there. That tool gives the normal and its operation gives the colour. A BVH over the
  tools' stretches (each node holding the first segment under it, so the playback's place is
  just a cutoff) finds the pieces a point is in.
- **When.** While the view moves (orbiting, scrubbing, playing) the tri-dexel's tiles show. Once
  it has rested 150 ms the exact view is drawn into a target of its own, in 128 px tiles under a
  per-frame pixel budget, so the GPU is never handed a whole frame at once. When it is whole it
  replaces the tiles. Drawing the knob takes about 55 ms, impeller-small about 0.5 µs a pixel
  (RTX 5080). A browser without a GPU (SwiftShader, llvmpipe) keeps the tiles.
- **The GPU compiler.** On Windows, WebGL goes through ANGLE to D3D11's FXC, which inlines every
  call. The shader is therefore written with one call site for each costly function (loops
  over the work instead of calls in many places), which took the first build from 87 s to about
  3 s. It is built off the page (KHR_parallel_shader_compile, ahead of the first playback), and
  the browser keeps it after that (about 10 ms).
- **Checks.** `cargo test -p marcam-core exact` compares straight sweeps with brute force, and
  turning moves with the tool's poses every 0.01° (no miss, no invented material, no material
  passed over). `examples/exact_synth.rs` cuts a slot, a pocket, a diamond knurl and a 4-axis
  helix into a bar and works out each pixel by brute force from the tool's poses. The CPU cast
  matches it exactly, and `web/tests/exact_probe.mjs` holds the GPU's within 1 px at operation
  boundaries at four views, extreme zoom included.

The tri-dexel stays for everything else: the reports (material left, gouges, rapids through the
stock), collisions, rest material and snapshots.
