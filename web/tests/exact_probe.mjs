// The GPU's exact stock against the engine's reference, pixel by pixel (see
// exact_probe_page.js): `node web/tests/exact_probe.mjs <dump.json under web/> [--frames 5]`.
// The dump: `cargo run --release -p marcam-core --example exact_view -- <project> --dump ...`.
import { startApp } from "./harness.mjs";

const url = process.argv[2];
const frames = Number(process.argv.includes("--frames") ? process.argv[process.argv.indexOf("--frames") + 1] : 5);
const app = await startApp();
let out;
try {
  out = await app.js(`await (await import('./tests/exact_probe_page.js')).run(${JSON.stringify(url)}, { frames: ${frames}, std: ${process.argv.includes('--std')}, rot: ${process.argv.includes('--rot') ? Number(process.argv[process.argv.indexOf('--rot') + 1]) : 0}, cut: ${process.argv.includes('--cut') ? Number(process.argv[process.argv.indexOf('--cut') + 1]) : null}, debug: ${process.argv.includes('--debug') ? Number(process.argv[process.argv.indexOf('--debug') + 1]) || 1 : 0} })`);
  console.log(out);
  const errs = app.pageErrors();
  if (errs.length) console.log("page errors:", errs.join(" | ").slice(0, 3000));
} catch (e) {
  console.log("error", e.message, app.pageErrors().join(" | ").slice(0, 2000));
} finally {
  await app.close();
  process.exit(out ? 0 : 1);
}
