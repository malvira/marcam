// marcam stock simulation on the GPU, shared by the native crate (marcam-gpu, wgpu) and the
// web app (WebGPU). One invocation per (tool position, cell in its window). The CPU sampled
// the moves and sized each window in f64 (marcam::gpu_input); this evaluates the cut in
// f32 and keeps the lowest with atomicMin on a key, height_um << 8 | operation tag.
//
// mode 0, rotary: the cell's ray from the axis is cut back to where it enters the tool (the
//   CPU's fixed-point iteration, sim::stamp); keys index i * na + j, height = radius.
// mode 1, block: the cell's top comes down to the tool's surface (sim3::Heightmap3::stamp);
//   keys index j * nx + i, height = z - zbase.

struct Grid {
    x0: f32,
    dx: f32,     // rotary X cell, or the block's cell
    y0: f32,
    zbase: f32,
    nx: u32,
    ny: u32,     // rotary: cells around (na); block: rows
    _p0: u32,
    _p1: u32,
};

// One dispatch: positions first..first+count, each covering a win_i x win_j window. A rapid
// segment is dispatched on its own, after the cuts before it, so it sees their stock.
struct Disp {
    first: u32,
    count: u32,
    win_i: u32,
    win_j: u32,
};

struct Pos {
    p0: f32,      // rotary: x, r, part angle under the tool; block: x, y, z
    p1: f32,
    p2: f32,
    p3: f32,
    radius: f32,
    kind: u32,    // 0 flat, 1 ball, 2 cone
    inv_tan: f32, // cone: 1 / tan(half angle)
    tag: u32,
    i0: u32,
    i1: u32,
    j0: i32,      // rotary: angular window, may run past 0 or na (wrapped)
    j1: i32,
    mode: u32,
    seg: u32,     // segment index in the job
    rapid: u32,   // 1: a rapid; cutting with it is reported in `hits`
    _q2: u32,
};

@group(0) @binding(0) var<uniform> grid: Grid;
@group(0) @binding(1) var<storage, read> pos: array<Pos>;
@group(0) @binding(2) var<storage, read_write> keys: array<atomic<u32>>;
@group(0) @binding(3) var<uniform> disp: Disp;
@group(0) @binding(4) var<storage, read_write> hits: array<atomic<u32>>;

const TAU: f32 = 6.283185307179586;

fn height(p: Pos, d: f32) -> f32 {
    if (p.kind == 1u) {
        return p.radius - sqrt(max(p.radius * p.radius - d * d, 0.0));
    }
    if (p.kind == 2u) {
        return d * p.inv_tan;
    }
    return 0.0;
}

fn key(h: f32, tag: u32) -> u32 {
    return (u32(max(h, 0.0) * 1000.0) << 8u) | (tag & 255u);
}

/// Keep the lower key; a rapid that lowers a cell flags its segment: 2 by more than 50 µm
/// (through the stock), 1 by more than 1 µm only (a graze, said as a warning:
/// sim::RAPID_DEPTH).
fn cut(k: u32, nk: u32, p: Pos) {
    let old = atomicMin(&keys[k], nk);
    if (p.rapid == 1u && (old >> 8u) > (nk >> 8u) + 50u) {
        atomicMax(&hits[p.seg], 2u);
    } else if (p.rapid == 1u && (old >> 8u) > (nk >> 8u) + 1u) {
        atomicMax(&hits[p.seg], 1u);
    }
}

@compute @workgroup_size(64)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let n = gid.y;
    if (n >= disp.count) {
        return;
    }
    let cell = gid.x;
    if (cell >= disp.win_i * disp.win_j) {
        return;
    }
    let p = pos[disp.first + n];
    let i = p.i0 + cell / disp.win_j;
    let jj = p.j0 + i32(cell % disp.win_j);
    if (i > p.i1 || jj > p.j1) {
        return;
    }
    let lim = p.radius * (1.0 - 1e-6);
    if (p.mode == 1u) {
        // block: the tool's surface over the cell centre
        let cx = grid.x0 + (f32(i) + 0.5) * grid.dx - p.p0;
        let cy = grid.y0 + (f32(jj) + 0.5) * grid.dx - p.p1;
        let d = sqrt(cx * cx + cy * cy);
        if (d >= lim) {
            return;
        }
        let z = p.p2 + height(p, d) - grid.zbase;
        cut(u32(jj) * grid.nx + i, key(z, p.tag), p);
        return;
    }
    // rotary
    let u = grid.x0 + (f32(i) + 0.5) * grid.dx - p.p0;
    if (abs(u) >= lim) {
        return;
    }
    let dphi = TAU / f32(grid.ny);
    let delta = f32(jj) * dphi + 0.5 * dphi - p.p2;
    let cs = cos(delta);
    if (cs <= 1e-6) {
        return;
    }
    let s = abs(sin(delta));
    var rho = p.p1 / cs;
    var inside = false;
    for (var k = 0; k < 8; k++) {
        let d = sqrt(u * u + (rho * s) * (rho * s));
        if (d >= lim) {
            inside = false;
            break;
        }
        inside = true;
        let next = (p.p1 + height(p, d)) / cs;
        if (abs(next - rho) < 1e-6) {
            rho = next;
            break;
        }
        rho = next;
    }
    if (!inside) {
        return;
    }
    // wrap round the bar; kept positive first: `%` of a negative i32 differed between
    // backends (one here gave the unsigned remainder)
    let j = u32(jj + 2 * i32(grid.ny)) % grid.ny;
    cut(i * grid.ny + j, key(rho, p.tag), p);
}
