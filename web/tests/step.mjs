// The web app importing STEP files (scripts/check-step.sh): the STEP module is fetched only
// when a STEP file is opened; a part on a vise and one on the rotary axis come in with their
// B-rep faces as the face regions (features found on them), are planned, are saved with the
// STEP itself and come back with the same faces when reopened.
//
//   node web/tests/step.mjs     (needs web/pkg from scripts/build-wasm.sh and build-step-wasm.sh)
import { readFileSync } from "node:fs";
import { join } from "node:path";
import { root, sleep, startApp, waitFor } from "./harness.mjs";

const results = [];
function check(name, ok, detail = "") {
  results.push({ name, ok });
  console.log(`${ok ? "PASS" : "FAIL"}  ${name}${detail ? `  (${detail})` : ""}`);
}

const app = await startApp();
const { js } = app;
// (written with an import's starter operations: asked for, as a caller may; Mar: the app
// itself starts a new project with none)
await js("localStorage.setItem('marcam.starters', '1'); window.__marcam.project.settings.starters = true; true");
let failed = false;
try {
  const stepRequests = () => app.requests().filter((u) => /marcam_step\.wasm/.test(u)).length;
  // an STL import, an example: no STEP module
  await js("document.getElementById('example').value = 'plate'; document.getElementById('loadExample').click(); true");
  await app.settle(500);
  check("the STEP module is not fetched until a STEP file is opened", stepRequests() === 0, String(stepRequests()));

  // import a STEP file of a box with a hole, on a vise
  const importStep = async (name, kind) => {
    const b64 = readFileSync(join(root, "tests", "fixtures", "step", name)).toString("base64");
    await js(`await window.__marcam.project.openText(${JSON.stringify(JSON.stringify({ format: "marcam-project", version: 1, project: { schema_version: "0.2", name: "step", models: [], tools: [], setups: [] } }))}, "step.marcam.json").then(() => true)`);
    await app.settle();
    await js(`(() => { const dt = new DataTransfer(); dt.items.add(new File([Uint8Array.from(atob("${b64}"), (c) => c.charCodeAt(0))], "${name}")); const i = document.getElementById('importStlFile'); i.files = dt.files; i.dispatchEvent(new Event('change', { bubbles: true })); return true; })()`);
    // (Mar: no setup dialogs) placed at once (4th axis) and planned; its type then its Setup's
    await waitFor(() => js("document.body.dataset.state === 'done' || document.body.dataset.state === 'error'"), `${name} imported and planned`, 240000);
    await app.settle(500);
    const shown = await js("document.getElementById('meshInfo').textContent");
    if (kind !== "rotary") {
      await js("document.querySelector('#tree .tree-setup.active [data-act=setup]').click(); true");
      await waitFor(() => js("!!document.querySelector('#setupHost #setupDialog[open]')"), "the setup's form in its details", 60000);
      await js(`document.querySelector('#setupHost .sd-kind[data-kind="${kind}"]').click(); true`);
      await waitFor(() => js(`JSON.parse(document.getElementById('job').value).setups[0].kind === '${kind}' && document.body.dataset.state !== 'busy'`), `the setup a ${kind}`, 120000);
      await app.settle(500);
      await js("document.getElementById('generate').click(); true");
      await waitFor(() => js("document.body.dataset.state === 'done' || document.body.dataset.state === 'error'"), `${name} planned as a ${kind}`, 240000);
      await app.settle(500);
    }
    return shown;
  };
  const mark = app.events.length;
  const shown = await importStep("box_with_hole.step", "vise");
  const doc = await js("JSON.parse(document.getElementById('job').value)");
  const regions = await js("await window.__marcam.engine('faceRegions', JSON.parse(document.getElementById('job').value), 'part', window.__marcam.state.mesh).then(({ result }) => result.regions.map((r) => r.id))");
  check("a STEP file imports: the dialog says what is in it, the model is the .step, its 7 B-rep faces are the face regions",
    /STEP, 1 body, 7 faces/.test(shown) && doc.models[0].file === "box_with_hole.step" && regions.length === 7 && regions.every((r) => /^s\d+$/.test(r)),
    `${shown} | ${doc.models[0].file} | ${regions.join(",")}`);
  check("the STEP module is fetched once, for it", stepRequests() === 1, String(stepRequests()));
  const detected = await js("[...document.querySelectorAll('#detected .detected-btn')].map((b) => b.textContent)");
  check("its features are found on the STEP's faces: the 8 mm hole", detected.some((t) => /8\.00 mm through/.test(t)), detected.join(" | "));
  const planned = await js("({ state: document.body.dataset.state, programs: window.__marcam.state.result?.programs.length || 0, time: window.__marcam.state.result?.time_total_s || 0 })");
  check("the import is planned (its starter operations), with a time", planned.state === "done" && planned.programs > 0 && planned.time > 0, JSON.stringify(planned));
  // the hole's detected feature starts an operation, which plans
  await js("[...document.querySelectorAll('#detected .detected-btn')].find((b) => /8\\.00 mm through/.test(b.textContent)).click(); true");
  await app.settle(500);
  await js("document.getElementById('generate').click(); true");
  await sleep(300);
  await app.settle(300);
  const ops = await js("JSON.parse(document.getElementById('job').value).setups[0].operations.map((o) => o.type)");
  const st = await js("document.body.dataset.state");
  check("the detected hole starts its operation, and it plans", st === "done" && ops.some((t) => t === "drill" || t === "bore"), `${st}: ${ops.join(", ")}`);
  await app.shot("step_box_imported");

  // saved: the STEP itself; reopened: converted again, the same faces
  const saved = await js(`JSON.stringify((() => { const st = window.__marcam.state; const project = JSON.parse(document.getElementById('job').value);
    const src = st.meshSource; let bin = ''; for (let i = 0; i < src.bytes.length; i += 8192) bin += String.fromCharCode(...src.bytes.subarray(i, i + 8192));
    return { format: 'marcam-project', version: 1, project, meshes: { [src.name]: btoa(bin) } }; })())`);
  const savedFile = JSON.parse(saved);
  const savedName = Object.keys(savedFile.meshes)[0];
  const isStepSaved = atob(savedFile.meshes[savedName]).startsWith("ISO-10303-21");
  await js(`await window.__marcam.project.openText(${JSON.stringify(saved)}, "box.marcam.json").then(() => true)`);
  await app.settle(500);
  const regions2 = await js("await window.__marcam.engine('faceRegions', JSON.parse(document.getElementById('job').value), 'part', window.__marcam.state.mesh).then(({ result }) => result.regions.map((r) => r.id))");
  check("the project keeps the STEP, and reopened it has the same face ids",
    savedName === "box_with_hole.step" && isStepSaved && JSON.stringify(regions2) === JSON.stringify(regions), `${savedName} ${isStepSaved} ${regions2.join(",")}`);
  // and the save button writes it the same way (the file the app downloads)
  const viaSave = await js(`await (async () => { const { projectFile, meshFileOf } = await import('./project.js'); const st = window.__marcam.state; const project = JSON.parse(document.getElementById('job').value);
    const src = st.meshSource && st.meshSource.name === meshFileOf(project) ? st.meshSource : null; return !!src && new TextDecoder().decode(src.bytes.subarray(0, 12)); })()`);
  check("Save writes the STEP source", viaSave === "ISO-10303-21", String(viaSave));

  // a revolved part on the rotary axis
  const knob = await importStep("knob.step", "rotary");
  const k = await js("({ state: document.body.dataset.state, programs: window.__marcam.state.result?.programs.length || 0, part: window.__marcam.state.three?.groups.part.children.length || 0 })");
  check("a revolved STEP part on the rotary axis imports, plans and is drawn", /STEP, 1 body/.test(knob) && k.state === "done" && k.programs > 0 && k.part > 0, `${knob} | ${JSON.stringify(k)}`);
  await app.shot("step_knob_rotary");

  // two bodies come in together
  const two = await importStep("two_bodies.step", "vise");
  check("a STEP file with two bodies imports both", /STEP, 2 bodies, 9 faces/.test(two), two);

  // a file that is not STEP inside: a clear error, nothing breaks
  await js(`(() => { const dt = new DataTransfer(); dt.items.add(new File([new TextEncoder().encode("ISO-10303-21;\\nHEADER;\\nENDSEC;\\nDATA;\\nENDSEC;\\nEND-ISO-10303-21;\\n")], "empty.step")); const i = document.getElementById('importStlFile'); i.files = dt.files; i.dispatchEvent(new Event('change', { bubbles: true })); return true; })()`);
  await waitFor(() => js("!!document.getElementById('error').textContent"), "the error", 60000);
  const err = await js("document.getElementById('error').textContent");
  check("an empty STEP file is refused with the reason", /empty\.step/.test(err) && /STEP|nothing/.test(err), err);

  const errs = app.pageErrors(mark);
  check("no page errors", errs.length === 0, errs.join(" | ").slice(0, 300));
} catch (e) {
  failed = true;
  console.log(`FAIL  ${e.message}`);
  try { await app.shot("step_failure"); } catch {}
} finally {
  await app.close();
}
const bad = results.filter((r) => !r.ok).length;
console.log(`\n${results.length - bad}/${results.length} STEP checks passed`);
process.exit(failed || bad ? 1 : 0);
