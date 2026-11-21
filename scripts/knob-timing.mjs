// How long the knob takes to plan here, natively (the CLI) and as WebAssembly in Node, with
// its T1 as written (3.175 mm) and as a 3 mm tool: a diagnostic for the CI runners (the
// Windows one has taken over two minutes for this plan in the browser, seconds elsewhere).
// Prints one line per run; with --notice, as GitHub notices (readable through the API).
//
//   node scripts/knob-timing.mjs [--notice]
import { execFileSync } from "node:child_process";
import { existsSync, mkdtempSync, readFileSync, writeFileSync, copyFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { fileURLToPath } from "node:url";
import { load } from "../crates/marcam-wasm/js/marcam.js";

const notice = process.argv.includes("--notice");
const say = (msg) => console.log(notice ? `::notice title=knob timing::${msg}` : msg);
const root = new URL("..", import.meta.url);
const path = (p) => new URL(p, root);
const text = readFileSync(path("examples/knob/knob.marcam.toml"), "utf8");
const text30 = text.replace("diameter = 3.175", "diameter = 3.0");

// natively
const exe = ["target/release/marcam.exe", "target/release/marcam"].map((p) => fileURLToPath(path(p))).find((p) => existsSync(p));
if (exe) {
  const dir = mkdtempSync(join(tmpdir(), "knob-timing-"));
  copyFileSync(path("examples/knob/knob.stl"), join(dir, "knob.stl"));
  for (const [name, t] of [["3.175", text], ["3.0", text30]]) {
    const f = join(dir, `knob_${name}.marcam.toml`);
    writeFileSync(f, t);
    const t0 = performance.now();
    try {
      execFileSync(exe, ["run", f, "--machine", "makera-z1-4axis", "--out", join(dir, `out_${name}`)], { stdio: "ignore" });
      say(`native, T1 ${name} mm: ${Math.round(performance.now() - t0)} ms`);
    } catch (e) {
      say(`native, T1 ${name} mm: failed after ${Math.round(performance.now() - t0)} ms (${e.message.slice(0, 100)})`);
    }
  }
} else say("native: no release build of the CLI");

// WebAssembly in Node (the engine the page runs)
const wasm = path("web/pkg/marcam.wasm");
if (existsSync(wasm)) {
  const phases = [];
  let last = performance.now();
  const cam = await load(readFileSync(wasm), {
    onPhase: (p) => {
      const now = performance.now();
      phases.push(`${p} @${Math.round(now - last)}`);
      last = now;
    },
  });
  for (const [name, t] of [["3.175", text], ["3.0", text30]]) {
    phases.length = 0;
    last = performance.now();
    const t0 = performance.now();
    const r = cam.planAndPost(t, "makera-z1-4axis", undefined, { max_points: 150000, cache: false });
    say(`wasm in Node, T1 ${name} mm: ${Math.round(performance.now() - t0)} ms${r.error ? ` (error: ${r.error})` : ""}; phases: ${phases.join(", ")}`);
  }
} else say("wasm: web/pkg/marcam.wasm not built");

// in the browser (the page's engine worker, as the e2e runs it), with --browser
if (process.argv.includes("--browser")) {
  const { startApp } = await import("../web/tests/harness.mjs");
  const app = await startApp();
  try {
    const info = await app.js("JSON.stringify({ ua: navigator.userAgent, cores: navigator.hardwareConcurrency, hidden: document.hidden })");
    say(`browser: ${info}`);
    for (const name of ["3.175", "3.0"]) {
      const t = name === "3.0" ? text30 : text;
      const r = await app.js(`await (async () => {
        const M = window.__marcam, seen = [];
        let last = performance.now();
        const iv = setInterval(() => { for (const c of M.inFlight()) if (c.phase && seen[seen.length - 1]?.p !== c.phase) { const now = performance.now(); seen.push({ p: c.phase, at: Math.round(now - last) }); last = now; } }, 20);
        const t0 = performance.now();
        try {
          await M.engine('planAndPost', ${JSON.stringify(t)}, 'makera-z1-4axis', null, { max_points: 150000, cache: false });
        } catch (e) {
          return 'error ' + e.message;
        } finally {
          clearInterval(iv);
        }
        return Math.round(performance.now() - t0) + ' ms; phases: ' + seen.map((s) => s.p + ' @' + s.at).join(', ');
      })()`);
      say(`browser worker, T1 ${name} mm: ${r}`);
    }
  } finally {
    await app.close();
  }
}
