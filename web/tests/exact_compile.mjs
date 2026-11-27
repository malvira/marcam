// How long the GPU takes to build the exact stock's shader (web/gpu/exact.js): its ray cast
// in a bare WebGL2 program, compiled and linked in a fresh page, timed (ms). Variants with
// parts stubbed out show where the time goes.
//
//   node web/tests/exact_compile.mjs
import { startApp } from "./harness.mjs";

const app = await startApp();
const { js } = app;
try {
  const res = await js(`await (await import('./tests/exact_compile_page.js')).run()`);
  console.log(res);
} catch (e) { console.log("error", e.message); } finally {
  await app.close?.();
  process.exit(0);
}
