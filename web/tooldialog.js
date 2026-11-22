// Tools for a project, from the libraries: a browser (search, type, size) that adds the tool
// picked to the project without a T number (numbered later, in "Tool numbers"), or a new one
// typed in; and the tool numbers dialog that gives the project's tools their T numbers.

const esc = (s) => String(s ?? "").replace(/[&<>"]/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;" })[c]);
const KINDS = ["flat", "ball", "vbit", "tapered_ball", "bull"];

/** The library tools on offer: the built-in libraries and the person's, flattened. */
function libraryTools(state, userLibraries) {
  const out = [];
  for (const lib of [...(state.registry?.libraries || []), ...(userLibraries() || [])]) {
    for (const t of lib.tools || []) {
      out.push({ library: lib.id, libraryName: lib.name || lib.id, id: t.id, name: t.name || t.id, kind: t.shape?.kind, diameter: t.shape?.diameter ?? t.shape?.tip_diameter, flute: t.flute_length, materials: [...new Set((t.presets || []).map((p) => p.material || "any material"))] });
    }
  }
  return out;
}

import { calcFeeds, feedsSummary } from "./feeds.js";

export function setupToolDialogs({ $, state, readJob, writeJob, userLibraries, engine, machineArg }) {
  // ------------------------------------------------------------ the library browser
  const dlg = document.createElement("dialog");
  dlg.id = "toolPick";
  dlg.className = "tool-dialog";
  dlg.innerHTML = `<form method="dialog" id="toolPickForm">
    <h3>Add a tool to the project</h3>
    <div class="row small"><input id="tpSearch" placeholder="Search (name, size)" aria-label="Search tools">
      <select id="tpKind" aria-label="Tool type"><option value="">every type</option>${KINDS.map((k) => `<option>${k}</option>`).join("")}</select></div>
    <div id="tpList" class="tp-list" role="listbox" aria-label="Library tools"></div>
    <details id="tpNew"><summary>New tool...</summary>
      <div class="row small"><label>Type <select id="tpNewKind">${KINDS.slice(0, 3).map((k) => `<option>${k}</option>`).join("")}</select></label>
        <label>Diameter (mm) <input id="tpNewDia" type="number" step="0.001" value="3.175" size="6"></label>
        <label>Flute (mm) <input id="tpNewFlute" type="number" step="0.1" value="12" size="5"></label>
        <label>Angle (deg, V) <input id="tpNewAngle" type="number" step="1" value="90" size="4"></label></div>
      <div class="row small"><input id="tpNewName" placeholder="Description" aria-label="New tool description"> <button type="button" id="tpNewAdd">Add this tool</button></div>
    </details>
    <div class="muted small">Added as the next free tool number (Tools &gt; Tool numbers to change it).</div>
    <div class="row"><button value="cancel" type="submit">Cancel</button></div>
  </form>`;
  document.body.appendChild(dlg);
  let resolveWith = null;

  function renderList() {
    const q = $("tpSearch").value.trim().toLowerCase();
    const kind = $("tpKind").value;
    const tools = libraryTools(state, userLibraries).filter((t) => (!kind || t.kind === kind) && (!q || `${t.name} ${t.diameter} ${t.kind} ${t.libraryName} ${t.materials.join(" ")}`.toLowerCase().includes(q)));
    $("tpList").innerHTML = tools.length
      ? tools.map((t, i) => `<button type="button" class="tp-row" data-i="${i}" role="option"><strong>${esc(t.name)}</strong> <span class="muted">${esc(t.kind || "")} ${t.diameter ?? ""} mm · ${esc(t.libraryName)}${t.materials.length ? ` · data for ${esc(t.materials.join(", "))}` : ""}</span></button>`).join("")
      : `<div class="muted small">No tool matches.</div>`;
    $("tpList").dataset.tools = JSON.stringify(tools);
  }
  $("tpSearch").addEventListener("input", renderList);
  $("tpKind").addEventListener("change", renderList);

  /** Add `entry` to the project as the next free tool number (the walkthrough: one added
   *  without a number was "written as T3" with a warning, a surprise); resolves to it. */
  function addToProject(entry) {
    const doc = readJob();
    doc.tools = doc.tools || [];
    const number = Math.max(0, ...doc.tools.map((t) => t.number)) + 1;
    doc.tools.push({ number, ...entry });
    writeJob(doc);
    return number;
  }
  $("tpList").addEventListener("click", (ev) => {
    const row = ev.target.closest(".tp-row");
    if (!row) return;
    const t = JSON.parse($("tpList").dataset.tools)[Number(row.dataset.i)];
    const n = addToProject({ library: t.library, tool: t.id, description: t.name });
    dlg.close();
    resolveWith?.(n);
    resolveWith = null;
  });
  $("tpNewAdd").addEventListener("click", () => {
    const kind = $("tpNewKind").value;
    const d = Number($("tpNewDia").value) || 3.175;
    const t = { description: $("tpNewName").value.trim() || `${d} mm ${kind}`, kind, diameter: d, flute_length: Number($("tpNewFlute").value) || 12 };
    if (kind === "vbit") t.angle = Number($("tpNewAngle").value) || 90;
    const n = addToProject(t);
    dlg.close();
    resolveWith?.(n);
    resolveWith = null;
  });
  dlg.addEventListener("close", () => {
    resolveWith?.(null);
    resolveWith = null;
  });

  /** Browse the libraries; resolves to the new tool's number (null: cancelled). */
  function pick(kinds) {
    $("tpSearch").value = "";
    // the operation's tool types first
    $("tpKind").value = kinds?.length === 1 ? kinds[0] : "";
    renderList();
    dlg.showModal();
    return new Promise((r) => (resolveWith = r));
  }

  // ------------------------------------------------------------ tool numbers
  const num = document.createElement("dialog");
  num.id = "toolNumbers";
  num.className = "tool-dialog";
  document.body.appendChild(num);
  num.addEventListener("click", (ev) => {
    const b = ev.target.closest("button");
    if (!b) return;
    if (b.classList.contains("tn-cut")) num.querySelector(`.tn-cutrow[data-i="${b.dataset.i}"]`)?.classList.toggle("hidden");
    if (b.classList.contains("pc-add")) num.querySelector(`.pc-table[data-i="${b.dataset.i}"]`).insertAdjacentHTML("beforeend", cutRow({}));
    if (b.classList.contains("pc-del")) b.closest("tr").remove();
    if (b.classList.contains("pc-calc")) calcRow(b.closest("tr"));
  });

  // ---- a library tool's cutting data for this project only (the project's `cutting`, picked
  // before the library's; the library's rows shown, read only)
  const CUT = [
    ["rpm", "rpm"],
    ["feed", "feed"],
    ["plunge", "plunge"],
    ["stepdown", "depth"],
    ["stepover", "stepover"],
    ["ramp_angle", "ramp °"],
  ];
  /** A row's speeds, feeds and depths from the calculator, for the library tool of its table. */
  async function calcRow(tr) {
    const i = +tr.closest(".pc-table").dataset.i;
    const entry = readJob().tools?.[i];
    const lib = [...(state.registry?.libraries || []), ...(userLibraries() || [])].find((l) => l.id === entry?.library);
    const lt = lib?.tools?.find((x) => x.id === entry?.tool);
    const out = num.querySelector(`.pc-notes[data-i="${i}"]`);
    try {
      if (!lt) throw new Error("the tool is not in its library");
      const m = machineArg?.();
      const f = await calcFeeds(engine, {
        material: tr.querySelector(".pc-material").value.trim(),
        tool: lt.shape,
        flutes: lt.flutes,
        flute_length: lt.flute_length,
        op: tr.querySelector(".pc-op").value || undefined,
        machine: typeof m === "string" || m?.id ? m : null,
      });
      for (const [k] of CUT) tr.querySelector(`.pc-${k}`).value = f.preset[k] ?? "";
      if (out) out.textContent = feedsSummary(f);
    } catch (e) {
      if (out) out.textContent = e.message;
    }
  }
  function libraryRows(t) {
    const lib = [...(state.registry?.libraries || []), ...(userLibraries() || [])].find((l) => l.id === t.library);
    return lib?.tools?.find((x) => x.id === t.tool)?.presets || [];
  }
  function cutRow(p = {}) {
    const types = state.tree?.types || [];
    const val = (k) => (p[k] == null || (["rpm", "feed", "plunge"].includes(k) && !p[k]) ? "" : p[k]);
    return `<tr class="pc-row"><td><input class="pc-material" list="sdMaterials" placeholder="any material" value="${esc(p.material)}" aria-label="Material"></td>
      <td><select class="pc-op" aria-label="Operation type"><option value="">any operation</option>${types.map((ty) => `<option value="${ty.kind}" ${p.op === ty.kind ? "selected" : ""}>${esc(ty.title)}</option>`).join("")}</select></td>
      ${CUT.map(([k, l]) => `<td><input class="pc-${k}" type="number" step="any" min="0" value="${val(k)}" aria-label="${l}" size="5"></td>`).join("")}
      <td class="nowrap"><button type="button" class="pc-calc" title="Calculate: speeds, feeds and depths for this material and operation, within the machine's limits">Calc</button> <button type="button" class="pc-del" aria-label="Remove this row">✕</button></td></tr>`;
  }
  function readRows(box) {
    const out = [];
    for (const tr of box.querySelectorAll("tr.pc-row")) {
      const p = { material: tr.querySelector(".pc-material").value.trim() };
      const op = tr.querySelector(".pc-op").value;
      if (op) p.op = op;
      let any = false;
      for (const [k] of CUT) {
        const v = tr.querySelector(`.pc-${k}`).value;
        if (v === "" || !(+v >= 0) || (["stepdown", "stepover"].includes(k) && !(+v > 0))) {
          if (["rpm", "feed", "plunge"].includes(k)) p[k] = 0;
          continue;
        }
        p[k] = +v;
        any = true;
      }
      if (any) out.push(p);
    }
    return out;
  }

  function openNumbers() {
    const doc = readJob();
    const un = new Set(doc.unindexed || []);
    const label = (t) => t.description || (t.library ? `${t.library}/${t.tool}` : `${t.kind} ${t.diameter}`);
    const libRowsText = (t) => libraryRows(t).map((p) => `${p.material || "any material"}${p.op ? ` · ${p.op.replace(/_/g, " ")}` : ""}: ${p.feed || "-"} mm/min`).join("; ");
    num.innerHTML = `<form method="dialog" id="toolNumbersForm"><h3>Tool numbers</h3>
      <div class="muted small">The T number each tool has in the machine's tool changer. A tool without one is written as the next free number, with a warning.</div>
      <table class="tn-table"><thead><tr><th>Tool</th><th>T number</th><th>Has one</th><th></th></tr></thead><tbody>
      ${(doc.tools || []).map((t, i) => `<tr><td>${esc(label(t))}</td><td><input type="number" min="1" step="1" data-i="${i}" class="tn-num" value="${t.number}" size="4"></td><td><input type="checkbox" data-i="${i}" class="tn-has" ${un.has(t.number) ? "" : "checked"}></td>
        <td>${t.library ? `<button type="button" class="tn-cut" data-i="${i}" title="This project's own cutting data for the tool (before the library's)">Cutting data${t.cutting?.length ? ` (${t.cutting.length})` : ""}</button>` : ""}</td></tr>
        ${t.library ? `<tr class="tn-cutrow hidden" data-i="${i}"><td colspan="4"><div class="small muted">For this project, picked before the library's${libRowsText(t) ? ` (the library's: ${esc(libRowsText(t))})` : ""}. Blank: not given.</div>
          <div class="tablewrap cut-wrap"><table class="tools small cut-table pc-table" data-i="${i}"><tr><th>Material</th><th>Operation</th>${CUT.map(([, l]) => `<th>${l}</th>`).join("")}<th></th></tr>${(t.cutting || []).map(cutRow).join("")}</table></div>
          <button type="button" class="pc-add" data-i="${i}">Add a row</button><div class="small muted pc-notes" data-i="${i}" aria-live="polite"></div></td></tr>` : ""}`).join("")}
      </tbody></table>
      <div id="tnError" class="error small hidden"></div>
      <div class="row"><button type="button" id="tnApply" class="primary">Apply</button> <button value="cancel" type="submit">Cancel</button></div></form>`;
    // the materials offered (the setup dialog's list)
    const dl = document.getElementById("sdMaterials");
    if (dl && !dl.children.length) dl.innerHTML = (state.registry?.materials || []).map((m) => `<option value="${esc(m)}">`).join("");
    num.showModal();
    num.querySelector("#tnApply").addEventListener("click", () => {
      const nums = [...num.querySelectorAll(".tn-num")].map((e) => Math.round(Number(e.value)));
      const has = [...num.querySelectorAll(".tn-has")].map((e) => e.checked);
      if (nums.some((n) => !(n >= 1)) || new Set(nums).size !== nums.length) {
        const e = num.querySelector("#tnError");
        e.textContent = "Each tool needs its own number, 1 or more.";
        e.classList.remove("hidden");
        return;
      }
      // renumber: the operations follow their tool
      const map = new Map(doc.tools.map((t, i) => [t.number, nums[i]]));
      doc.tools.forEach((t, i) => (t.number = nums[i]));
      for (const s of doc.setups || []) for (const o of s.operations || []) if (map.has(o.tool)) o.tool = map.get(o.tool);
      doc.unindexed = doc.tools.filter((t, i) => !has[i]).map((t) => t.number);
      if (!doc.unindexed.length) delete doc.unindexed;
      // each library tool's own cutting data
      for (const box of num.querySelectorAll(".pc-table")) {
        const t = doc.tools[+box.dataset.i];
        const rows = readRows(box);
        if (rows.length) t.cutting = rows;
        else delete t.cutting;
      }
      writeJob(doc);
      num.close();
      window.__marcam.tree?.refresh();
    });
  }

  return { pick, openNumbers };
}
