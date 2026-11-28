// Project tree (document model 0.2): setups -> outputs (G-code files) -> operations.
// Operations can be added (forms built from the engine's operation registry), edited, turned
// on/off, coloured, reordered, deleted, and limited to faces picked on the 3D model. The
// project JSON in the job editor is the single source of truth: every edit rewrites it.

const PALETTE = ["#3b82f6", "#f59e0b", "#10b981", "#ef4444", "#a855f7", "#14b8a6", "#e11d48", "#84cc16"];
const esc = (s) => String(s).replace(/[&<>"]/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;" })[c]);

import { describeStock, stockRadius } from "./project.js";
import { applyFixtureField, applyPartField, applyStockField, carriedFrom, carryQuick, FIXTURES, fixtureOf, fixturePane, fitStock, meshModelOf, partPane, rangesFollow, stockPane } from "./stockpane.js";
import { calcFeeds, feedsSummary } from "./feeds.js";
import { moveSetup } from "./handles.js";
import { canPickFile, chosenFile, exportKey, exportStatus, moveChosen, writeExport } from "./exports.js";

export function setupTree({ engine, $, state, readJob, writeJob, showError, onChange, getThree, machineArg }) {
  // gen: bumped when the project is replaced, so editor work still awaiting the engine then
  // is dropped instead of landing in the new project
  const t = { doc: null, problems: [], types: [], setup: null, selected: null, picking: false, regions: null, gen: 0 };
  state.tree = t;

  async function loadTypes() {
    if (!t.types.length) t.types = (await engine("opTypes")).result;
  }

  /** Re-read the editor: 0.1 jobs are shown upgraded (the editor switches to 0.2 on the first edit). */
  async function refresh() {
    // refreshes overlap (a plan ending while an edit is applied): only the latest one, which
    // read the project last, is shown (an older one would put back what was there before)
    const gen = (t.refreshGen = (t.refreshGen || 0) + 1);
    await loadTypes();
    let raw;
    try {
      raw = readJob();
    } catch {
      setTree('<div class="muted small">(fix the JSON to see the project)</div>');
      return;
    }
    try {
      const { result } = await engine("checkProject", raw);
      if (gen !== t.refreshGen) return;
      t.doc = result.project;
      t.problems = result.problems;
      t.upgraded = result.upgraded;
    } catch (e) {
      setTree(`<div class="error">${esc(e.message)}</div>`);
      return;
    }
    if (!t.doc.setups.some((s) => s.id === t.setup)) t.setup = t.doc.setups[0]?.id ?? null;
    const s = setup();
    const exists = (id) => !!s?.operations.some((o) => o.id === id);
    // an applied operation's pending type change: gone with it
    if (t.retype && !exists(t.retype.op.id)) t.retype = null;
    // a draft whose id has since been taken (an undo brought one back): dropped
    if (t.draft && exists(t.draft.op.id)) t.draft = null;
    if (t.selected && !exists(t.selected) && !(t.draft && t.draft.op.id === t.selected)) {
      if (t.picking) endPicking(false);
      t.selected = null;
    }
    // (an undo, a redo, a plan) picking on: what the operation has picked now
    if (t.picking && t.pick) {
      const op = opById(t.selected);
      if (op) {
        t.pick.sel = new Set(initialSel(op, t.pick.list, t.pick.mode));
        paintPicked();
      }
    }
    t.pickFailed = null;
    render();
  }

  /** Write the project back to the editor and refresh everything that depends on it. */
  async function commit(opts = {}) {
    // (what Edit > Undo will say it undoes)
    if (opts.label) window.__marcam.project?.nameEdit?.(opts.label);
    writeJob(t.doc, opts);
    await refresh();
    onChange?.();
  }

  const setup = () => t.doc?.setups.find((s) => s.id === t.setup);
  // the operation being edited: a new one (draft), an applied one whose type is being changed
  // (retype, written on Apply), else the project's
  const opById = (id) =>
    t.draft && t.draft.op.id === id ? t.draft.op : t.retype && t.retype.op.id === id ? t.retype.op : setup()?.operations.find((o) => o.id === id);
  const typeOf = (kind) => t.types.find((x) => x.kind === kind);
  /** A project tool as it is: inline, or looked up in its library (built-in or the
   *  person's): { number, kind, diameter, name }. */
  function toolInfo(x) {
    if (!x.library) return { number: x.number, kind: x.kind, diameter: x.diameter ?? x.tip_diameter, name: x.description, shape: x, flutes: x.flutes, flute_length: x.flute_length };
    const libs = [...(state.registry?.libraries || []), ...(window.__marcam.planArgs?.().libraries || [])];
    const t = libs.find((l) => l.id === x.library)?.tools.find((y) => y.id === x.tool);
    return { number: x.number, kind: t?.shape?.kind, diameter: t?.shape?.diameter ?? t?.shape?.tip_diameter, name: t?.name || x.tool, shape: t?.shape, flutes: t?.flutes, flute_length: t?.flute_length };
  }
  /** The operation form's feeds from the calculator: its tool, stepover and depth per pass,
   *  in the setup's material at the setup's spindle speed. The fields are set, not applied. */
  async function calcOpFeeds() {
    const f = $("opForm");
    const out = $("opFeedsNotes");
    const op = opById(t.selected);
    const s = setup();
    if (!f || !op || !s) return;
    const ty = typeOf(op.type);
    const val = (k) => (f.elements[k] && f.elements[k].value !== "" ? +f.elements[k].value : undefined);
    const n = val("tool") ?? op.tool;
    const x = (t.doc?.tools || []).find((y) => y.number === n);
    try {
      if (!x) throw new Error(`T${n} is not in the project`);
      const info = toolInfo(x);
      if (!info.shape) throw new Error(`T${n} is not in its library`);
      const d = info.diameter || 0;
      // the stepover in mm (a fraction of the tool where the operation takes one)
      const so = ["stepover", "stepover_mm", "step_x", "rough_step"].map((k) => [k, val(k)]).find(([, v]) => v > 0);
      const unit = so && ty.params.find((p) => p.name === so[0])?.unit;
      const stepover = so ? (String(unit || "").startsWith("mm") ? so[1] : so[1] * d) : undefined;
      const stepdown = val("step_down") ?? val("rough_depth") ?? val("depth_per_rev");
      const adaptive = !!f.elements.adaptive?.checked || f.elements.pattern?.value === "adaptive";
      const m = machineArg?.();
      const r = await calcFeeds(engine, {
        material: s.material || "",
        tool: info.shape,
        flutes: info.flutes,
        flute_length: info.flute_length,
        op: op.type,
        adaptive,
        stepover,
        stepdown,
        rpm: s.spindle_rpm || undefined,
        machine: typeof m === "string" || m?.id ? m : null,
      });
      const set = (k, v) => {
        if (!f.elements[k]) return;
        f.elements[k].value = v;
        f.elements[k].dispatchEvent(new Event("input", { bubbles: true }));
      };
      set("feed", op.type === "drill" ? r.plunge : r.feed);
      set("rough_feed", r.feed);
      set("finish_feed", Math.round(0.85 * r.feed));
      set("plunge_feed", r.plunge);
      if (out) out.textContent = feedsSummary(r);
    } catch (e) {
      if (out) out.textContent = e.message;
    }
  }
  /** A project tool: "T3 · Spiral O Metal 3.175 (flat 3.175)", "— · ..." without a T number. */
  function toolLabel(x) {
    const i = toolInfo(x);
    const unindexed = (t.doc?.unindexed || []).includes(x.number);
    return `${unindexed ? "—" : `T${i.number}`} · ${i.name || i.kind || "tool"} (${i.kind || "tool"}${i.diameter != null ? ` ${i.diameter}` : ""})`;
  }
  /** Why tool `x` does not suit type `ty` ("" when it does). */
  function toolMisfit(ty, x) {
    const kind = toolInfo(x).kind;
    if (!ty?.tool_shapes?.length || !kind || ty.tool_shapes.includes(kind)) return "";
    return `${ty.title} takes ${ty.tool_shapes.join(" or ")} tools`;
  }
  /** Why an operation cannot be planned: its error, or its tool missing from the project. */
  function opProblem(op) {
    if (op.error) return op.error;
    if (op.tool != null && !(t.doc?.tools || []).some((x) => x.number === op.tool)) return `tool T${op.tool} is not in the project`;
    return "";
  }
  const colorOf = (op, i) => op.color || PALETTE[i % PALETTE.length];
  // a vise setup (box stock) takes the 3-axis types; a rotary setup takes everything, the
  // 3-axis types indexed to a fixed A (3+1)
  // a vise setup: a block, or a bar lying in the vise
  const isBox = (s) => s?.stock?.shape === "box" || s?.kind === "vise" || s?.holding?.kind === "vise";
  // the setup's kind (rotary, indexed 3+1 or vise) decides which types fit; a bar without a
  // kind (older projects) takes everything
  const kindOfSetup = (s) => s?.kind ?? (isBox(s) ? "vise" : "rotary");
  /** The picker's entry for 3D roughing at several A angles (indexed 3+1) on a 4th-axis setup. */
  const INDEXED_ROUGH = "rough3d:indexed";
  const fits = (ty, s) => {
    const k = kindOfSetup(s);
    if (k === "rotary") return !s?.kind || !ty.machine_needs.includes("xyz");
    if (k === "indexed") return ty.machine_needs.includes("xyz");
    return !ty.machine_needs.includes("rotary_a");
  };
  const KIND_NAMES = { rotary: "Rotary 4th axis", indexed: "Indexed 3+1", vise: "3-axis vise" };
  function setupSummary(s) {
    const st = s.stock || {};
    const r = (v) => +(+v).toFixed(3);
    const stock = describeStock(st.shape ? st : null, s.holding?.chuck);
    const hold = s.holding?.kind === "vise" ? "vise" : `chuck${s.holding?.tailstock ? " + tailstock" : ""}`;
    return `${KIND_NAMES[kindOfSetup(s)]}${s.kind ? "" : " (any operation)"} · ${s.machine ?? "machine: the selected one"} · ${stock} · ${hold}`;
  }
  const pointsText = (v) => v.map((q) => q.join(" ")).join("; ");

  function render() {
    const box = $("tree");
    // the form being edited is kept (what is typed in it, the focus, the caret), and the
    // panel's scroll: drawn again, the tree must not change what the person did not touch
    const oldEd = $("opEditor");
    if (oldEd) t.keepEd = { el: oldEd, focus: focusIn(oldEd) };
    // (the setup's form in the details: put back after, focus and all)
    const fa = document.activeElement;
    let sdFocus = null;
    if (fa?.closest?.("#setupDialog.embedded") && fa.id) {
      try {
        sdFocus = { id: fa.id, s: fa.selectionStart, e: fa.selectionEnd };
      } catch {
        sdFocus = { id: fa.id, s: null, e: null };
      }
    }
    // (the Stock and Part panes: the field being typed in kept as typed, focus and caret; Mar
    // typed a stock length and the pane drawn again after the first digit kept only it)
    let paneFocus = null;
    if ((fa?.closest?.("#stockPane") || fa?.closest?.("#partPane") || fa?.closest?.("#exportPane")) && fa.id && fa.tagName === "INPUT") {
      paneFocus = { id: fa.id, v: fa.value, s: fa.selectionStart, e: fa.selectionEnd };
    }
    const scrolls = [];
    for (let e = box; e; e = e.parentElement) if (e.scrollTop) scrolls.push([e, e.scrollTop]);
    // (the tree's own list scrolls too: drawn again, as each plan landing does, it stays where
    // it was; it went back to the top, hiding the NC exports under the files)
    const listTop = $("treeList")?.scrollTop || 0;
    if (!t.doc) return;
    const s = setup();
    if (!s) {
      const lib = window.__marcam.tools?.summary?.() || "";
      setTree(`<div class="tree-list"><div class="tree-lib ${t.focus === "library" ? "sel" : ""}"><div class="tree-setuphead"><span class="twisty-pad"></span><span class="tree-label" data-act="library" title="The tools of every project: add, edit, import, export">Tool library</span> <span class="muted">${esc(lib)}</span></div></div>
        <div class="muted small" id="treeEmpty">No setups yet.</div>
        <div class="row small add-setup"><button type="button" id="treeAddSetup" title="A new setup, made at once (its type and where it starts from chosen beside)">+ Add setup</button>
        <select id="treeAddKind" aria-label="The new setup's type" title="The new setup's type"><option value="vise">3-axis</option><option value="rotary">4th axis</option></select>
        ${t.doc?.setups?.length ? `<select id="treeAddFrom" aria-label="Where the new setup starts" title="Continue from the setup before (its cut result and part), or its own new stock and part"><option value="carry">continue from previous</option><option value="own">independent</option></select>` : ""}</div></div>
        ${t.focus === "library" ? '<div class="tree-details" id="treeDetails"><div class="tree-outname">Tool library</div><div id="libHost"></div></div>' : ""}`);
      window.__marcam.tools?.host?.($("libHost"));
      window.__marcam.project?.hostSetup?.(null);
      return;
    }
    const setupSel = t.doc.setups
      .map((x) => `<option value="${esc(x.id)}" ${x.id === t.setup ? "selected" : ""}>${esc(x.name || x.id)}</option>`)
      .join("");
    // (Mar) the setups as a tree, each with its operations under it; the selected item's
    // details below the tree. (The setup chooser stays for scripts: not shown.)
    // folded or not: as the person left each setup; by default the one worked on unfolded
    t.fold ||= new Map();
    let html = `<select id="treeSetup" class="hidden" aria-hidden="true" tabindex="-1">${setupSel}</select><div class="tree-list" id="treeList">`;
    // (Mar) the tool library, for every project, first
    const lib = window.__marcam.tools?.summary?.() || "";
    html += `<div class="tree-lib ${t.focus === "library" ? "sel" : ""}"><div class="tree-setuphead"><span class="twisty-pad"></span><span class="tree-label" data-act="library" title="The tools of every project: add, edit, import, export">Tool library</span> <span class="muted">${esc(lib)}</span></div></div>`;
    for (const x of t.doc.setups) {
      if (x.id === s.id) {
        html += setupHead(x, true);
        html += `<div class="tree-setupbody" ${isOpen(x) ? "" : "hidden"}>`;
        break;
      }
      html += setupHead(x, false) + otherOps(x) + `</div>`;
    }
    html += setupStockRows(s);
    html += setupTools(s);
    html += `<div class="row small eyes"><span class="muted">Toolpaths shown:</span>
        <button type="button" id="eyeAll" title="Show every toolpath of this setup">Show all</button>
        <button type="button" id="eyeNone" title="Hide every toolpath of this setup (view only)">Hide all</button></div>
      <div class="row small detected" id="detected"></div>`;
    if (t.upgraded) html += `<div class="muted small">0.1 job shown as a 0.2 project; it is saved as 0.2 on the first edit.</div>`;
    let n = 0;
    // estimated times from the last Generate of this setup
    const res = state.result;
    const fmt = window.__marcam?.fmtTime || ((x) => `${Math.round(x)} s`);
    const fileTime = (name) => res?.programs?.find((p) => p.name === name)?.time?.total_s;
    // by project id, or (a 0.1 job shown as a project) by planning order: the enabled
    // operations of the outputs in turn
    const planned = s.outputs.flatMap((o) => o.operations).filter((id) => opById(id)?.enabled !== false);
    const opTime = (id) =>
      res?.programs?.flatMap((p) => p.time?.ops || []).find((o) => (o.id ?? planned[o.index]) === id)?.seconds;
    for (const out of s.outputs) {
      const ft = fileTime(out.name);
      const allHidden = out.operations.length && out.operations.every((id) => isHidden(id));
      html += `<div class="tree-out" data-out="${esc(out.name)}"><div class="tree-outname"><button type="button" class="eye ${allHidden ? "off" : ""}" data-out-eye="${esc(out.name)}" aria-label="Show the toolpaths of ${esc(out.name)}" title="Show or hide every toolpath of this file (view only)">${allHidden ? "◌" : "◉"}</button> ${esc(out.name)}.nc <span class="muted">${esc(out.title)}</span>${ft != null ? ` <span class="tree-time" title="estimated machining time">${fmt(ft)}</span>` : ""}</div>`;
      out.operations.forEach((id, k) => {
        const op = opById(id);
        if (!op) return;
        const i = s.operations.indexOf(op);
        const ot = opTime(id);
        const geo = op.geometry?.length ? ` · ${op.geometry.map((g) => (g.kind === "faces" ? `${g.ids.length} face${g.ids.length > 1 ? "s" : ""}` : g.kind.replace("_", " "))).join(", ")}` : "";
        html += `<div class="tree-op ${t.selected === id || t.multi?.has(id) ? "sel" : ""} ${op.enabled === false ? "off" : ""}" data-op="${esc(id)}" draggable="true" title="Drag to reorder (or Alt+Up / Alt+Down; Ctrl-click to pick several, for an NC export)">
          <input type="checkbox" data-act="enable" ${op.enabled === false ? "" : "checked"} aria-label="Enable ${esc(id)}">
          <input type="color" data-act="color" value="${colorOf(op, i)}" aria-label="Colour of ${esc(id)}">
          <button type="button" data-act="eye" class="eye ${isHidden(id) ? "off" : ""}" aria-pressed="${!isHidden(id)}" aria-label="Show the toolpath of ${esc(id)}" title="Show or hide this toolpath (view only; Alt-click: show only this)">${isHidden(id) ? "◌" : "◉"}</button>
          <span class="op-state" data-state-for="${esc(id)}" aria-live="polite"></span>
          <span class="tree-label" data-act="select">${opProblem(op) ? `<span class="op-error" title="${esc(opProblem(op))}">⚠</span> ` : ""}${esc(op.name || typeOf(op.type)?.title || op.type)} <span class="muted">${esc(id)} · T${op.tool}${geo}${ot != null ? ` · <span class="tree-time">${fmt(ot)}</span>` : ""}</span></span>
          <button type="button" data-act="dup" class="op-dup" aria-label="Duplicate ${esc(id)}" title="Duplicate (Ctrl+D; Ctrl+drag onto another setup's operation: a copy there)">⧉</button>
          <button type="button" data-act="delete" aria-label="Delete ${esc(id)}">✕</button></div>`;
        n++;
      });
      html += `</div>`;
    }
    html += exportsList(s);
    let opts = t.types.filter((x) => fits(x, s)).map((x) => `<option value="${x.kind}">${esc(x.title)}</option>`).join("");
    // (Mar: "not seeing the new indexed roughing for 4 axis") on a 4th-axis setup, 3D
    // roughing at several angles in turn, by its name
    if (kindOfSetup(s) === "rotary" && s?.kind && t.types.some((x) => x.kind === "rough3d")) opts += `<option value="${INDEXED_ROUGH}">Indexed roughing (3+1)</option>`;
    html += `<div class="row small"><select id="treeAddType" aria-label="Operation type">${opts}</select>
      <button type="button" id="treeAdd">Add operation</button></div>`;
    if (t.problems.length) html += `<div class="error small">${t.problems.map(esc).join("<br>")}</div>`;
    if (t.draft) html += `<div class="muted small">New operation ${esc(t.draft.op.id)}: added once its fields are complete</div>`;
    html += `</div></div>`;
    // the setups after the one being worked on
    for (const x of t.doc.setups.slice(t.doc.setups.indexOf(s) + 1)) html += setupHead(x, false) + otherOps(x) + `</div>`;
    html += `<div class="row small add-setup"><button type="button" id="treeAddSetup" title="A new setup, made at once (its type and where it starts from chosen beside)">+ Add setup</button>
        <select id="treeAddKind" aria-label="The new setup's type" title="The new setup's type"><option value="vise">3-axis</option><option value="rotary">4th axis</option></select>
        ${t.doc?.setups?.length ? `<select id="treeAddFrom" aria-label="Where the new setup starts" title="Continue from the setup before (its cut result and part), or its own new stock and part"><option value="carry">continue from previous</option><option value="own">independent</option></select>` : ""}</div></div>`;
    // the details of what is selected: the setup (nothing else selected), or the operation
    html += `<div class="tree-details" id="treeDetails">`;
    const tool = t.focus === "tool" && !t.selected ? t.doc.tools.find((x) => x.number === t.tool) : null;
    if (t.focus === "library" && !t.selected) {
      html += `<div class="tree-outname">Tool library</div><div id="libHost"></div>`;
    } else if (tool) {
      html += toolDetails(tool, s);
    } else if (t.focus === "export" && !t.selected && s.exports?.some((e) => e.name === t.export)) {
      html += exportPane(s, s.exports.find((e) => e.name === t.export));
    } else if (t.focus === "stock" && !t.selected) {
      const m = meshModelOf(t.doc, s);
      html += stockPane(t.doc, s, esc, { canFit: !!(m && window.__marcam.meshFor?.(m.file)), margin: t.stMargin, moreOpen: t.stMore, length: carryLength(s) });
    } else if (t.focus === "part" && !t.selected) {
      html += partPane(t.doc, s, esc, { moreOpen: t.ptMore, choices: modelChoices(s) });
    } else if (t.focus === "fixture" && !t.selected) {
      html += fixturePane(t.doc, s, esc);
    } else if (!t.selected) {
      // (Mar) everything about the setup here: its stock, its
      // type, machine, holding, material, G-code options and work zero (applied as changed)
      const m = meshModelOf(t.doc, s);
      html += `<div class="tree-outname">${esc(s.name || s.id)} <span class="muted">setup</span></div>
        <div class="muted small setup-summary" id="setupSummary">${esc(setupSummary(s))}</div>
        ${typeRow(s)}
        ${t.setupNotes?.[s.id] ? `<div class="small setup-note" id="setupNote">${esc(t.setupNotes[s.id])}</div>` : ""}
        ${sourcePick(s)}
        ${stockPane(t.doc, s, esc, { canFit: !!(m && window.__marcam.meshFor?.(m.file)), margin: t.stMargin, moreOpen: t.stMore, length: carryLength(s) })}
        <div class="row small">${kindOfSetup(s) === "vise" ? "" : '<button type="button" id="treeWizard" title="Add turning, knurl and chamfer from the model\'s faces">Knurl wizard</button> <select id="treeWizardPattern" aria-label="The knurl the wizard adds" title="The knurl the wizard adds: diamond (helices crossing) or straight (along the axis)"><option value="diamond">diamond</option><option value="straight">straight</option></select>'}
        ${s.stock?.shape === "from_previous" ? `<button type="button" id="treeStockStl" title="The stock this setup starts with (what the setup before left, turned as here) as an STL">Incoming stock STL</button>` : ""}</div>
        <div id="setupHost" class="setup-host"></div>`;
    }
    html += `<div id="opEditor"></div></div>`;
    setTree(html);
    box.dataset.ops = String(n);
    renderStates();
    renderEditor();
    // the setup's settings (stock, part, holding, output) in its details; the library's
    window.__marcam.project?.hostSetup?.($("setupHost"), t.setup, sdFocus);
    window.__marcam.tools?.host?.($("libHost"));
    for (const [e, y] of scrolls) e.scrollTop = y;
    if (listTop && $("treeList")) $("treeList").scrollTop = listTop;
    detectFeatures();
    if (paneFocus) {
      const el = document.getElementById(paneFocus.id);
      if (el) {
        el.value = paneFocus.v;
        el.focus({ preventScroll: true });
        try {
          if (paneFocus.s != null) el.setSelectionRange(paneFocus.s, paneFocus.e);
        } catch {}
      }
    }
    // (the 3D view's handles: on the selected Part, or hidden)
    window.__marcam.handles?.refresh?.();
  }

  /** A carried piece's length on the 4th axis (for its stick-out): known after the engine
   *  says, then the field filled in. */
  function carryLength(s) {
    if (s.stock?.shape !== "from_previous") return null;
    const key = JSON.stringify([s.id, s.stock, t.doc.setups.indexOf(s)]);
    const got = (t.carryLen ||= new Map()).get(key);
    if (got === undefined) {
      t.carryLen.set(key, null);
      engine("setupFrame", t.doc, { setup: s.id }).then(({ result }) => {
        const len = result.stock?.length || null;
        t.carryLen.set(key, len);
        const f = $("stStickOut");
        if (f && len && f.value === "" && document.activeElement !== f) {
          f.value = String(Math.round(Math.max(0, len - (s.holding?.chuck ?? 0)) * 1000) / 1000);
          f.defaultValue = f.value;
        }
      }).catch(() => {});
    }
    return got || null;
  }

  /** (Mar) The setup's Stock and Part in the tree: selecting one shows its pane (the Part,
   *  the handles in the 3D view too). */
  function setupStockRows(x) {
    const from = carriedFrom(t.doc, x);
    const st = from ? `what ${from.name || from.id} left` : describeStock(x.stock, x.holding?.chuck);
    const m = meshModelOf(t.doc, x);
    const part = from ? "with the stock" : m ? m.file : x.part?.length ? "revolved profile" : "no model";
    const row = (act, name, what, title) =>
      `<div class="tree-stock ${t.focus === act && !t.selected ? "sel" : ""}" data-node="${act}"><span class="tree-label" data-act="${act}" title="${esc(title)}">${name} <span class="muted">${esc(what)}</span></span></div>`;
    return row("stock", "Stock", st, "The stock: its type and sizes") + row("part", "Part", part, "Where the part sits in the stock (handles in the 3D view)") + row("fixture", "Fixture", FIXTURES[fixtureOf(x)], "What holds the stock: its preset and sizes (drawn, and the tool checked against it)");
  }

  /** (Mar: "a way in 4th to center up a part") The part on the rotary axis placed in one edit:
   *  its middle onto the axis (`axis`), halfway along the bar (`along`), its front at the
   *  bar's face (`front`) or its back at the bar's end (`back`). A part of its own is moved;
   *  a carried one moves with its stock (the stock's offset). */
  async function placePreset(which, label, data = {}) {
    const s = setup();
    if (!s) return;
    let lo, hi, len, box, turns;
    try {
      const mt = (await engine("meshTriangles", t.doc, { setup: s.id }, state.mesh)).result;
      [lo, hi] = mt.bounds;
      turns = mt.axis;
      const fr = (await engine("setupFrame", t.doc, { setup: s.id })).result;
      len = fr.stock?.length;
      box = fr.stock_box;
    } catch (e) {
      return showError(`${label}: ${e.message}`);
    }
    // a quarter turn (about the part's middle), or none at all
    if (which === "turn" || which === "unturn") {
      const mid = [0, 1, 2].map((i) => (lo[i] + hi[i]) / 2);
      let what;
      if (which === "turn") {
        what = moveSetup(t.doc, s, { k: "XYZ".indexOf(data.axis), deg: +data.deg, pivot: mid });
      } else if (carriedFrom(t.doc, s)) {
        if (!s.stock.rotate && !s.stock.turn) return;
        delete s.stock.rotate;
        delete s.stock.turn;
        what = "unturned";
      } else {
        const m = t.doc.models.find((x) => s.part?.includes(x.id) && x.kind === "mesh");
        if (!m?.rotate) return;
        delete m.rotate;
        what = "unturned";
      }
      if (!what) return;
      // (on the 4th axis: kept on the axis, its middle where it was, its front where it was)
      const m = !carriedFrom(t.doc, s) && kindOfSetup(s) !== "vise" ? t.doc.models.find((x) => s.part?.includes(x.id) && x.kind === "mesh") : null;
      if (m) {
        let b;
        try {
          b = (await engine("meshTriangles", t.doc, { setup: s.id }, state.mesh)).result.bounds;
        } catch (e) {
          return showError(`${label}: ${e.message}`);
        }
        const r = (v) => Math.round(v * 1000) / 1000;
        const d = [hi[0] - b[1][0], mid[1] - (b[0][1] + b[1][1]) / 2, mid[2] - (b[0][2] + b[1][2]) / 2];
        m.translate = (m.translate || [0, 0, 0]).map((v, i) => r(v + d[i]));
        rangesFollow(s, r(d[0]));
      }
      return commit({ label: `Setup "${s.name || s.id}": the part ${label}.` });
    }
    const d = [0, 0, 0];
    if (box) {
      const c = [0, 1, 2].map((i) => (box.min[i] + box.max[i]) / 2 - (lo[i] + hi[i]) / 2);
      if (which === "cx" || which === "cxy") d[0] = c[0];
      if (which === "cy" || which === "cxy") d[1] = c[1];
      if (which === "cz") d[2] = c[2];
      if (which === "top") d[2] = box.max[2] - hi[2];
      if (which === "bottom") d[2] = box.min[2] - lo[2];
    }
    // (Mar's 5-blade impeller: its box's middle 0.58 mm off where it turns) a part round, or
    // the same every k-th of a turn, by where it turns about (on it within 0.01 mm: left); else
    // by its box's middle
    if (which === "axis") {
      if (turns) {
        if (Math.hypot(turns[0], turns[1]) >= 0.01) (d[1] = -turns[0]), (d[2] = -turns[1]);
      } else (d[1] = -(lo[1] + hi[1]) / 2), (d[2] = -(lo[2] + hi[2]) / 2);
    }
    if (which === "along") d[0] = -len / 2 - (lo[0] + hi[0]) / 2;
    if (which === "front") d[0] = -hi[0];
    if (which === "back") d[0] = -len - lo[0];
    if (d.every((v) => Math.abs(v) < 1e-4)) return;
    const r = (v) => Math.round(v * 1000) / 1000;
    if (carriedFrom(t.doc, s)) {
      const off = (s.stock.offset || [0, 0, 0]).map((v, i) => r(v + d[i]));
      if (off.some((v) => v)) s.stock.offset = off;
      else delete s.stock.offset;
    } else {
      const m = t.doc.models.find((x) => s.part?.includes(x.id) && x.kind === "mesh");
      if (!m) return;
      m.translate = (m.translate || [0, 0, 0]).map((v, i) => r(v + d[i]));
    }
    // (the impeller: moved into the bar, its roughing's X range stayed where the part had
    // been) the setup's X ranges go along with the part
    rangesFollow(s, r(d[0]));
    await commit({ label: `Setup "${s.name || s.id}": the part ${label.toLowerCase()}.` });
  }

  /** (Mar: "need a way to edit the type of setup after it's been added"; "just move up the
   *  icon based one") its type at the top of its details: the setup's own type cards (the
   *  form's, hidden further down), each choosing the form's own type button, so a change
   *  keeps what fits and turns the stock as there. */
  function typeRow(x) {
    const k = kindOfSetup(x) || (isBox(x) ? "vise" : "rotary");
    const cards = [...document.querySelectorAll("#setupDialog .sd-kind")].map((c) => {
      const v = c.dataset.kind;
      const inner = [...c.childNodes].filter((n) => n.nodeName !== "INPUT").map((n) => n.outerHTML ?? esc(n.textContent)).join("");
      return `<button type="button" class="sd-kind ${v === k ? "on" : ""}" data-setkind="${v}" aria-pressed="${v === k}" title="${esc(c.title || "")}">${inner}</button>`;
    });
    return `<fieldset class="sd-kinds setup-type" id="setupType"><legend>Setup type</legend>${cards.join("")}</fieldset>`;
  }

  /** (Mar) Where setup `x` starts (setup 2 and after): continuing from the setup before (its
   *  cut result, its part locked to it), or independent (its own stock and part, as setup 1). */
  function sourcePick(x) {
    const at = t.doc.setups.indexOf(x);
    if (at < 1) return "";
    const from = carriedFrom(t.doc, x);
    const prev = t.doc.setups[at - 1];
    return `<label class="source-pick" title="Continue from a setup before (its cut result, its part with it) or start independent (its own stock and part, placed freely)">Starts <select id="stSource" aria-label="Where this setup starts">
      <option value="carry" ${from ? "selected" : ""}>continuing from ${esc((from || prev).name || (from || prev).id)}</option>
      <option value="own" ${from ? "" : "selected"}>independent (its own stock and part)</option></select></label>`;
  }

  /** The models a setup's part may be: its own, another setup's (by file), or none. */
  function modelChoices(x) {
    const own = meshModelOf(t.doc, x);
    const seen = new Set(own ? [own.file] : []);
    const out = [];
    for (const o of t.doc.setups) {
      if (o === x) continue;
      const om = meshModelOf(t.doc, o);
      if (om && !seen.has(om.file) && window.__marcam.meshFor?.(om.file)) {
        seen.add(om.file);
        out.push({ value: `setup:${o.id}`, label: `as ${o.name || o.id}: ${om.file}` });
      }
    }
    return out;
  }

  /** The tool numbers a setup lists: those it was given, and those its operations use. */
  function toolsOf(x) {
    const ns = new Set([...(x.tools || []), ...x.operations.map((o) => o.tool).filter((n) => n != null)]);
    return [...ns].sort((a, b) => a - b);
  }
  /** A setup's tools in the tree: T number, name, size; one added from the library. */
  function setupTools(x) {
    const rows = toolsOf(x)
      .map((n) => {
        const e = t.doc.tools.find((y) => y.number === n);
        const i = e ? toolInfo(e) : null;
        const sel = t.focus === "tool" && t.tool === n && !t.selected;
        return `<div class="tree-tool ${sel ? "sel" : ""}" data-tool="${n}"><span class="tree-label" data-act="tool">T${n} <span class="muted">${esc(i ? `${i.name || i.kind} · Ø${+(+i.diameter || 0).toFixed(3)}` : "not in the project")}</span></span></div>`;
      })
      .join("");
    return `<div class="tree-tools"><div class="tree-outname">Tools <button type="button" id="treeAddTool" class="small" title="A tool from the library for this setup (it gets the next tool number)">+ from library</button></div>${rows}</div>`;
  }
  /** A tool's details: as the project has it (its number, what it is, its sizes). */
  function toolDetails(e, x) {
    const i = toolInfo(e);
    const sh = i.shape || {};
    const rows = [
      ["Number", `T${i.number}`],
      ["Name", i.name || ""],
      ["Kind", i.kind || ""],
      ["Diameter", sh.diameter != null ? `${sh.diameter} mm` : i.diameter != null ? `${i.diameter} mm` : ""],
      ["Tip diameter", sh.tip_diameter != null ? `${sh.tip_diameter} mm` : ""],
      ["Angle", sh.angle != null ? `${sh.angle}°` : ""],
      ["Flute length", i.flute_length != null ? `${i.flute_length} mm` : ""],
      ["Flutes", i.flutes != null ? String(i.flutes) : ""],
      ["Stickout", sh.stickout != null ? `${sh.stickout} mm` : ""],
      ["From", e.library ? `library ${e.library}` : "this project"],
      ["Used by", x.operations.filter((o) => o.tool === i.number).map((o) => o.id).join(", ") || "no operation yet"],
    ].filter(([, v]) => v);
    return `<div class="tree-outname">T${i.number} <span class="muted">tool</span></div><table class="small tool-details">${rows.map(([k, v]) => `<tr><td class="muted">${esc(k)}</td><td>${esc(v)}</td></tr>`).join("")}</table>
      <div class="muted small">Its sizes and cutting data are edited in the Tool library.</div>`;
  }

  // ------------------------------------------------------------ NC exports (see exports.js)
  /** The plan's entry for export `name` of setup `s`, when the results are this setup's. */
  function exportResult(s, name) {
    const r = state.result;
    if (!r || (r.setupId ?? null) !== s.id) return null;
    return r.exports?.find((e) => e.name === name) || null;
  }
  /** The setup's operations in the order they are machined (the files' in turn). */
  const opsInOrder = (s) => s.outputs.flatMap((o) => o.operations).map((id) => s.operations.find((o) => o.id === id)).filter(Boolean);
  /** The setup's NC exports, under its files: each with whether its file is in sync. */
  function exportsList(s) {
    const rows = (s.exports || [])
      .map((e) => {
        const st = exportStatus(e, exportResult(s, e.name));
        const n = e.operations.length;
        return `<div class="tree-exp ${t.focus === "export" && !t.selected && t.export === e.name ? "sel" : ""}" data-export="${esc(e.name)}">
          <span class="tree-label" data-act="export" title="This export's operations and file">${esc(e.name)}.nc <span class="muted">${n} op${n === 1 ? "" : "s"}</span></span>
          <span class="exp-state ${st.kind}" data-exp-state="${esc(e.name)}" title="${esc(st.long)}">${esc(st.text)}</span>
          <button type="button" data-act="writeexp" class="exp-write" aria-label="Write ${esc(e.name)}.nc" title="Write ${esc(e.name)}.nc${canPickFile() ? " (to the file chosen for it)" : ""}">⤓</button></div>`;
      })
      .join("");
    const picked = t.multi?.size ? ` (the ${t.multi.size} picked)` : t.selected ? " (the selected operation)" : " (every operation; Ctrl-click operations to pick some)";
    return `<div class="tree-exports" id="treeExports"><div class="tree-exphead"><span class="muted">NC exports</span>
      <button type="button" id="treeAddExport" title="A named set of operations written to a .nc file of its own${esc(picked)}">+ New</button></div>${rows}</div>`;
  }
  /** An export's details: its name, its operations (ticked), its state, Write. */
  function exportPane(s, e) {
    const st = exportStatus(e, exportResult(s, e.name));
    const on = new Set(e.operations);
    const ops = opsInOrder(s)
      .map((op) => `<label class="exp-op ${op.enabled === false ? "off" : ""}"><input type="checkbox" data-exp-op="${esc(op.id)}" ${on.has(op.id) ? "checked" : ""}> ${esc(op.name || typeOf(op.type)?.title || op.type)} <span class="muted">${esc(op.id)} · T${op.tool}</span></label>`)
      .join("");
    setTimeout(() => showChosen(s, e), 0);
    return `<div id="exportPane"><div class="tree-outname">${esc(e.name)}.nc <span class="muted">NC export</span></div>
      <div class="row small"><label for="expName">Name</label> <input id="expName" name="expName" value="${esc(e.name)}" spellcheck="false" autocomplete="off"><span class="muted">.nc</span></div>
      <div class="small exp-status ${st.kind}" id="expStatus">${esc(st.long)}</div>
      <div class="small muted" id="expFile">${esc(chosenText(t.chosen?.get(expKey(s, e.name))))}</div>
      <div class="small muted">Its operations, in the order they are machined:</div>
      <div class="exp-ops" id="expOps">${ops}</div>
      <div class="row small"><button type="button" id="expWrite" class="primary">Write ${esc(e.name)}.nc</button>
      ${canPickFile() ? '<button type="button" id="expWriteAs" title="Write it to another file (later writes go there)">Save as...</button>' : ""}
      <button type="button" id="expDelete" title="Delete this export (its operations stay; a file written stays)">Delete</button></div></div>`;
  }
  const expKey = (s, name) => exportKey(t.doc?.name || "untitled", s.id, name);
  const chosenText = (f) => (f ? `Writes to ${f} (the file chosen for it).` : canPickFile() ? "The first write asks where to save it; later writes go to that file." : "Each write downloads it.");
  /** Say which file an export writes to (looked up, kept for the next drawing). */
  async function showChosen(s, e) {
    const key = expKey(s, e.name);
    const f = await chosenFile(key);
    (t.chosen ||= new Map()).set(key, f);
    const el = document.getElementById("expFile");
    const text = chosenText(f);
    if (el && t.export === e.name && el.textContent !== text) el.textContent = text;
  }
  async function newExport() {
    const s = setup();
    if (!s) return;
    const order = opsInOrder(s).map((o) => o.id);
    const want = t.multi?.size ? [...t.multi] : t.selected && order.includes(t.selected) ? [t.selected] : opsInOrder(s).filter((o) => o.enabled !== false).map((o) => o.id);
    s.exports ||= [];
    const used = new Set(s.exports.map((e) => e.name));
    let k = s.exports.length + 1;
    while (used.has(`${s.id}_export${k}`)) k++;
    const name = `${s.id}_export${k}`;
    s.exports.push({ name, operations: order.filter((id) => want.includes(id)) });
    t.multi = null;
    t.focus = "export";
    t.export = name;
    t.selected = null;
    await commit({ label: `Added the NC export ${name}.` });
  }
  async function doWriteExport(name, pick) {
    const s = setup();
    const e = s?.exports?.find((x) => x.name === name);
    if (!s || !e) return;
    const sid = s.id;
    try {
      const done = await writeExport({
        name,
        key: expKey(s, name),
        pick,
        whenSettled: () => window.__marcam.whenSettled(),
        current: () => exportResult(t.doc.setups.find((x) => x.id === sid) || s, name),
      });
      if (!done) return;
      // the fingerprint written: in sync until the toolpaths change
      const x = t.doc.setups.find((y) => y.id === sid)?.exports?.find((y) => y.name === name);
      if (!x) return;
      x.posted = done.fingerprint;
      x.posted_at = new Date().toISOString();
      await commit({ label: `Wrote ${name}.nc.`, display: true });
      window.__marcam.setStatus?.(`Wrote ${done.file} (${done.lines.toLocaleString("en")} lines)`, "done");
    } catch (err) {
      showError(`Could not write ${name}.nc: ${err.message}`);
    }
  }

  /** A setup's line in the tree (its `<div class="tree-setup">` left open): a fold button,
   *  its name (selecting it shows the setup's details), its kind. */
  function setupHead(x, active) {
    const open = isOpen(x);
    return `<div class="tree-setup ${active ? "active" : ""} ${active && !t.selected && (!t.focus || t.focus === "setup") ? "sel" : ""}" data-setup="${esc(x.id)}">
      <div class="tree-setuphead"><button type="button" class="twisty" data-act="fold" aria-expanded="${open}" aria-label="Show the operations of ${esc(x.name || x.id)}">${open ? "▾" : "▸"}</button>
      <span class="tree-label" data-act="setup" title="This setup's details">${esc(x.name || x.id)}</span> <span class="muted">${esc(KIND_NAMES[kindOfSetup(x)])} · ${x.operations.length} op${x.operations.length === 1 ? "" : "s"}</span>
      <button type="button" class="setup-del" data-act="delsetup" aria-label="Delete the setup ${esc(x.name || x.id)}" title="Delete this setup (and its operations)">✕</button></div>`;
  }
  /** Another setup's operations (when unfolded): selecting one goes to that setup. */
  function otherOps(x) {
    if (!isOpen(x)) return "";
    return `<div class="tree-setupbody">${x.outputs
      .flatMap((o) => o.operations)
      .map((id) => x.operations.find((o) => o.id === id))
      .filter(Boolean)
      .map((op) => `<div class="tree-op other ${op.enabled === false ? "off" : ""}" data-op="${esc(op.id)}"><span class="tree-label" data-act="select">${esc(op.name || typeOf(op.type)?.title || op.type)} <span class="muted">${esc(op.id)} · T${op.tool}</span></span></div>`)
      .join("")}</div>`;
  }
  const isOpen = (x) => (t.fold?.has(x.id) ? t.fold.get(x.id) : x.id === t.setup);
  /** Delete setup `id` (Mar): with its operations, and the tools and models nothing else
   *  uses; asked first when it has operations. The last one too: what is left is a new
   *  project's (no setups, models, tools or part; the tool library kept). The setups that took their
   *  stock from it take it from the one before it (or, with none, the stock it had), and say
   *  so. Undoable. */
  async function deleteSetup(id) {
    const doc = t.doc;
    const k = doc.setups.findIndex((x) => x.id === id);
    if (k < 0) return;
    const s = doc.setups[k];
    const n = s.operations.length;
    if (n && !window.confirm(`Delete "${s.name || s.id}" and its ${n} operation${n === 1 ? "" : "s"}? (Edit > Undo brings it back.)`)) return;
    if (t.picking) endPicking(false);
    const name = s.name || s.id;
    if (doc.setups.length === 1) {
      // (Mar) the last: as a new project, its name kept (the part goes with it; Undo
      // brings it all back)
      doc.setups = [];
      doc.models = [];
      doc.tools = [];
      t.setup = null;
      t.focus = null;
      t.selected = null;
      t.draft = null;
      t.retype = null;
      // (the part goes as an edit: the snapshot before it keeps the part, for Undo)
      const h = window.__marcam.project?.history;
      if (h) h.loading = false;
      state.extraMeshes?.clear();
      window.__marcam.setMesh?.(null, "");
      await commit({ label: `Deleted the setup "${name}" (the last).` });
      window.__marcam.clearResults?.("No setups yet.", ["add"]);
      return;
    }
    const before = doc.setups[k - 1] || null;
    doc.setups.splice(k, 1);
    t.setupNotes ||= {};
    // the setups that took their stock from it
    doc.setups.forEach((x, i) => {
      if (x.stock?.shape !== "from_previous") return;
      const named = x.stock.from;
      const fromIt = named ? named === id : i === k;
      if (!fromIt) return;
      if (before) {
        // from the one before it: named, unless it is now just before this one
        if (doc.setups[i - 1]?.id === before.id) delete x.stock.from;
        else x.stock.from = before.id;
        t.setupNotes[x.id] = `Its stock came from "${name}", deleted: now from "${before.name || before.id}".`;
      } else {
        // nothing before: the stock the deleted one had (its own, not carried)
        x.stock = structuredClone(s.stock);
        t.setupNotes[x.id] = `Its stock came from "${name}", deleted: now new stock, as "${name}" had (Generate to plan it).`;
      }
    });
    // the tools and models nothing else uses
    const used = new Set(doc.setups.flatMap((x) => [...(x.tools || []), ...x.operations.map((o) => o.tool)]));
    doc.tools = (doc.tools || []).filter((tl) => used.has(tl.number));
    const models = new Set(doc.setups.flatMap((x) => x.part || []));
    doc.models = (doc.models || []).filter((m) => models.has(m.id));
    if (t.setup === id) switchSetup(doc.setups[Math.max(0, k - 1)].id);
    t.focus = "setup";
    t.selected = null;
    await commit({ label: `Deleted the setup "${name}".` });
  }

  /** Another setup is worked on: what was being edited here is dropped. */
  function switchSetup(id) {
    if (id === t.setup) return;
    if (t.picking) endPicking(false);
    t.setup = id;
    // (its toolpaths are not the ones shown: planned (live, from the cache) or marked out of
    // date; the eyes and the playback were another setup's, Mar's hide/show trouble)
    queueMicrotask(() => window.__marcam.setupSwitched?.());
    t.focus = null;
    t.selected = null;
    t.draft = null;
    t.retype = null;
    t.typeNote = null;
  }

  /** The tree's contents replaced: what lives in it but is not drawn by it (the tool library's
   *  panel, the setup's form) taken out first, to the page, as they are (never left detached:
   *  the app looks their fields up by id); the tree puts them back where they belong. */
  /** The add controls' ids: a setup's type and where it starts, an operation's type. */
  const ADD_CONTROLS = ["treeAddSetup", "treeAddKind", "treeAddFrom", "treeAddType", "treeAdd"];
  function setTree(html) {
    window.__marcam.tools?.host?.(null);
    const dlg = document.getElementById("setupDialog");
    if (dlg && $("tree").contains(dlg)) document.body.appendChild(dlg);
    // (the add controls' choices kept when the tree is drawn again, as it is when a plan
    // lands: a type picked, then Add clicked, added the list's first type instead) the new
    // setup's type and start as they were, the operation type picked in this setup
    const kept = { treeAddKind: $("treeAddKind")?.value, treeAddFrom: $("treeAddFrom")?.value, treeAddType: t.addTypes?.[t.setup] };
    $("tree").innerHTML = html;
    for (const [id, v] of Object.entries(kept)) {
      const e = $(id);
      if (e && v != null && [...e.options].some((o) => o.value === v)) e.value = v;
    }
    // (a setup being added: nothing to add to until it is there, then its own types; the
    // controls only greyed, nothing moves)
    if (t.addingSetup) for (const id of ADD_CONTROLS) if ($(id)) $(id).disabled = true;
  }

  /** The field focused inside `el`, with its caret (to be put back after drawing again). */
  function focusIn(el) {
    const a = document.activeElement;
    if (!el || !a || !el.contains(a) || !a.name) return null;
    let s = null, e = null;
    try {
      s = a.selectionStart;
      e = a.selectionEnd;
    } catch {}
    return { name: a.name, s, e };
  }
  function refocus(el, f) {
    if (!f) return;
    const x = el.querySelector(`[name="${CSS.escape(f.name)}"]`);
    if (!x || document.activeElement === x) return;
    x.focus({ preventScroll: true });
    try {
      if (f.s != null) x.setSelectionRange(f.s, f.e);
    } catch {}
  }
  /** The fields changed in `from` (not applied yet) put into `to`, where `to` was drawn from
   *  the same value (the operation's own value did not change meanwhile). */
  function carryEdits(from, to) {
    if (!from || !to) return;
    for (const a of from.querySelectorAll("input, select, textarea")) {
      if (!a.name) continue;
      const b = to.querySelector(`[name="${CSS.escape(a.name)}"]`);
      if (!b || b.type !== a.type) continue;
      if (a.type === "checkbox" || a.type === "radio") {
        if (a.checked !== a.defaultChecked && b.defaultChecked === a.defaultChecked) b.checked = a.checked;
      } else if (a.tagName === "SELECT") {
        // (the tool and the type act at once, through their own handlers; "__" values are
        // commands, such as adding a tool from the library: not edits waiting for Apply)
        if (a.name === "tool" || a.name === "__type" || a.value.startsWith("__")) continue;
        const was = [...a.options].find((o) => o.defaultSelected)?.value;
        const now = [...b.options].find((o) => o.defaultSelected)?.value;
        if (a.value !== was && now === was && [...b.options].some((o) => o.value === a.value)) b.value = a.value;
      } else if (a.value !== a.defaultValue && (b.defaultValue === a.defaultValue || a === document.activeElement)) {
        b.value = a.value;
      }
    }
  }

  // ------------------------------------------------------------ visibility (view only)
  const eyeKey = (id) => `${t.setup}/${id}`;
  function isHidden(id) {
    return !!state.hidden?.has(eyeKey(id));
  }
  function setupOps() {
    return setup()?.operations.map((o) => o.id) || [];
  }
  function setEyes(fn) {
    const next = new Set(state.hidden);
    fn(next);
    window.__marcam.setHidden(next);
  }
  /** Repaint the eye buttons (after the view changed them). */
  function renderEyes() {
    if (t.doc) render();
  }

  // ------------------------------------------------------------ what the operation uses, in 3D
  // The selected operation's picks drawn over the part (faint; the one hovered in the
  // Geometry list brighter), so you see what it works on without picking.
  let overlay = null;
  let overlayKey = null;
  function dropOverlay() {
    const v = state.three;
    if (overlay && v && !v.failed) {
      v.scene.remove(overlay);
      overlay.traverse((o) => {
        o.geometry?.dispose();
        o.material?.dispose?.();
      });
    }
    overlay = null;
    overlayKey = null;
  }
  async function showUses(op) {
    const sigs = op?.picks || [];
    const key = op && sigs.length && !t.picking ? JSON.stringify([op.id, sigs, t.doc?.models, state.mesh?.length]) : null;
    if (key === overlayKey) return;
    dropOverlay();
    if (!key) return;
    overlayKey = key;
    const v = await getThree();
    if (!v || v.failed) return;
    const s = setup();
    const model = partModel(s);
    let regions, found;
    try {
      [{ result: regions }, { result: found }] = await Promise.all([
        engine("faceRegions", t.doc, JSON.stringify({ model, setup: s.id }), state.mesh),
        engine("resolvePicks", t.doc, { setup: s.id, model, index_a: op.index_a ?? 0, signatures: sigs }, state.mesh),
      ]);
    } catch {
      return;
    }
    if (overlayKey !== key) return;
    const bytes = (b64) => (typeof b64 === "string" ? Uint8Array.from(atob(b64), (c) => c.charCodeAt(0)) : b64);
    const tri = new Float32Array(bytes(regions.triangles).buffer);
    const of = new Uint16Array(bytes(regions.region_of).buffer);
    const { THREE } = v;
    const grp = new THREE.Group();
    found.picks.forEach((p, i) => {
      if (!p.faces) return;
      const want = new Set(p.faces);
      const ids = new Set(regions.regions.map((r, k) => (want.has(r.id) ? k : -1)).filter((k) => k >= 0));
      const pos = [];
      for (let k = 0; k < of.length; k++) if (ids.has(of[k])) for (let q = 0; q < 9; q++) pos.push(tri[9 * k + q]);
      if (!pos.length) return;
      const g = new THREE.BufferGeometry();
      g.setAttribute("position", new THREE.Float32BufferAttribute(pos, 3));
      g.computeVertexNormals();
      const m = new THREE.Mesh(g, new THREE.MeshBasicMaterial({ color: 0x3b82f6, transparent: true, opacity: 0.35, depthTest: false, side: THREE.DoubleSide }));
      m.userData.geom = i;
      m.renderOrder = 5;
      grp.add(m);
    });
    overlay = grp;
    v.scene.add(grp);
  }
  /** The Geometry list's row `i` hovered (null: none): brighter in 3D. */
  function hoverUse(i) {
    for (const m of overlay?.children || []) m.material.opacity = m.userData.geom === i ? 0.75 : 0.35;
  }

  /** Remove pick `i` (or every pick) of the selected operation: the rest picked again (their
   *  fields follow); none left, the operation is as before picking. */
  async function removeUse(i) {
    const op = opById(t.selected);
    if (!op?.picks?.length) return;
    const keep = i == null ? [] : op.picks.filter((_, k) => k !== i);
    const before = JSON.stringify(t.doc, null, 2);
    const ty = typeOf(op.type);
    const mode = pickModeOf(ty);
    const s = setup();
    const model = partModel(s);
    if (keep.length && !mode?.faces) {
      try {
        const { result: found } = await engine("resolvePicks", t.doc, { setup: s.id, model, index_a: op.index_a ?? 0, signatures: keep }, state.mesh);
        const ids = found.picks.filter((p) => p.id).map((p) => p.id);
        const { result } = await engine("applyPicks", t.doc, { type: op.type, ids, setup: s.id, model, tool: op.tool, index_a: op.index_a ?? 0, op }, state.mesh);
        Object.assign(op, result.params);
        if (op.type === "pocket" && result.params.paths) delete op.rect;
      } catch (e) {
        return showError(`Remove: ${e.message}`);
      }
    } else if (mode?.faces) {
      // faces: the ones the rest cover
      const { result: found } = await engine("resolvePicks", t.doc, { setup: s.id, model, index_a: op.index_a ?? 0, signatures: keep }, state.mesh);
      const ids = [...new Set(found.picks.flatMap((p) => p.faces || []))];
      op.geometry = (op.geometry || []).filter((g) => g.kind !== "faces");
      if (ids.length) op.geometry.push({ kind: "faces", model, ids });
      if (!op.geometry.length) delete op.geometry;
    } else {
      // none left: back to the fields alone (a facing: the whole top again)
      if (op.type === "face") delete op.paths;
      delete op.regions;
    }
    if (keep.length) op.picks = keep;
    else delete op.picks;
    delete op.error;
    if (t.draft?.op.id === op.id || t.retype?.op.id === op.id) return renderEditor();
    window.__marcam.project?.keepUndo?.(i == null ? "Cleared the picks." : "Removed a pick.", before);
    await commit();
  }

  /** Form for the selected operation, built from the registry's parameter specs. */
  /** The operation being added, planned live in the background and drawn over the others
   *  (the app's preview); none: the preview cleared. */
  function previewDraft() {
    const pv = window.__marcam?.previewDraft;
    if (!pv) return;
    if (!t.draft || !t.doc) return pv(null);
    let op = t.draft.op;
    try {
      if (t.selected === t.draft.op.id && $("opForm")) op = readForm();
    } catch {}
    if (unpicked(op)) return pv(null);
    const doc = structuredClone(t.doc);
    const s = doc.setups.find((x) => x.id === t.setup);
    if (!s) return pv(null);
    s.operations.push(op);
    let out = s.outputs.find((o) => o.name === t.draft.out);
    if (!out) s.outputs.push((out = { name: `${s.id}_${op.id}`, title: t.draft.title || op.type, operations: [] }));
    out.operations.push(op.id);
    pv(doc, s.id, op.id);
  }

  function renderEditor() {
    // (after the form is drawn)
    queueMicrotask(previewDraft);
    // placing tabs is for the contour selected
    if (t.tabMode && opById(t.selected)?.type !== "contour") t.tabMode = false;
    const ed = $("opEditor");
    // (drawn again by the tree: the form as it was there)
    const kept = t.keepEd;
    t.keepEd = null;
    const op = t.selected && opById(t.selected);
    if (!op) {
      if (t.picking) endPicking(false);
      ed.innerHTML = "";
      ed.__html = "";
      dropOverlay();
      return;
    }
    autoPick(op);
    const ty = typeOf(op.type);
    let typeOpts = t.types
      .filter((x) => fits(x, setup()) || x.kind === op.type)
      .map((x) => `<option value="${x.kind}" ${x.kind === op.type ? "selected" : ""}>${esc(x.title)}</option>`)
      .join("");
    if (op.type !== "rough3d" && kindOfSetup(setup()) === "rotary" && t.types.some((x) => x.kind === "rough3d")) typeOpts += `<option value="${INDEXED_ROUGH}">Indexed roughing (3+1)</option>`;
    let html = `<form class="opform" id="opForm"><div class="tree-outname">${esc(ty?.title || op.type)} <span class="muted">${esc(op.id)}</span></div>
      ${opProblem(op) ? `<div class="error small" id="opError">Not planned: ${esc(opProblem(op))}. ${op.error ? "Pick it again or change its type." : "Choose another tool."}</div>` : ""}
      <label>Type <select name="__type" aria-label="Operation type of ${esc(op.id)}">${typeOpts}</select></label>
      ${t.typeNote && t.typeNote.id === op.id ? `<div class="muted small" id="typeNote">${esc(t.typeNote.text)}</div>` : ""}
      ${t.retype && t.retype.op.id === op.id ? `<div class="muted small">${t.retype.from !== op.type ? `Type changed from ${esc(typeOf(t.retype.from)?.title || t.retype.from)}: ` : "Changed: "}not applied yet (see below)</div>` : ""}
      <label>Name <input name="__name" value="${esc(op.name || "")}"></label>`;
    // (the rarely changed settings folded away under More)
    let more = "";
    // (Mar: "typing in manual coordinates for paths should be an advanced option that's
    // hidden away") the geometry typed as numbers folded away under Advanced: the form shows
    // how the geometry is chosen, picked on the model or the part's defaults
    let adv = "";
    let svg = "";
    const field = (p) => {
      const v = op[p.name];
      // rotary_finish (and rotary parallel) use one stepover or the other, depending on the
      // pattern
      if (p.name === "stepover_deg" && ["around", "circular", "spiral"].includes(op.pattern)) return;
      if (p.name === "stepover_mm" && ["along", "line"].includes(op.pattern) && op.stepover_deg != null) return;
      // a vise has no rotary axis to index
      if (p.name === "index_a" && isBox(setup())) return;
      // a straight knurl: no helix (its angle and families unused)
      if (ty.kind === "helical_knurl" && op.pattern === "straight" && (p.name === "helix_angle" || p.name === "families")) return;
      // several picked features: shown in the Geometry list, set by picking
      if (p.kind === "regions") return;
      const label = `${p.label || p.name.replace(/_/g, " ")}${p.unit ? ` (${p.unit})` : ""}`;
      if (p.name === "tool") {
        // the project's tools, those the type cannot use greyed with the reason; a tool the
        // project no longer has stays listed, as missing
        const list = t.doc.tools || [];
        const opts = list.map((x) => {
          const why = toolMisfit(ty, x);
          return `<option value="${x.number}" ${x.number === v ? "selected" : ""} ${why ? `disabled title="${esc(why)}"` : ""}>${esc(toolLabel(x))}${why ? ` (${esc(why)})` : ""}</option>`;
        });
        if (v != null && !list.some((x) => x.number === v)) opts.unshift(`<option value="${v}" selected>T${v} (not in the project)</option>`);
        // a tool from the libraries (or a new one), added to the project
        opts.push(`<option value="__add">Add tool from library...</option>`);
        html += `<label>${label}<select name="tool">${opts.join("")}</select></label>`;
      } else if (p.kind === "choice") {
        html += `<label>${label}<select name="${p.name}">${p.choices.map((c) => `<option ${c === v ? "selected" : ""}>${c}</option>`).join("")}</select></label>`;
      } else if (p.kind === "boolean") {
        html += `<label>${label}<input type="checkbox" name="${p.name}" ${(v ?? p.default) ? "checked" : ""}></label>`;
      } else if (p.kind === "string") {
        html += `<label title="${esc(p.help)}">${label}<input name="${p.name}" value="${esc(v ?? "")}"></label>`;
      } else if (p.kind === "paths") {
        const n = Array.isArray(v) ? v.length : 0;
        const text = Array.isArray(v) ? v.map(pointsText).join(" | ") : "";
        // (the SVG loader stays in the form: a file, not typed numbers; the numbers under
        // Advanced)
        svg += `<div class="row small"><label class="filebtn">Load SVG into ${p.name.replace(/_/g, " ")}<input type="file" accept=".svg,image/svg+xml" data-svg="${p.name}" hidden></label>
          <label>height (mm) <input name="__svg_height" value="20" size="4"></label></div>`;
        html += `<label title="${esc(p.help)}">${label} <span class="muted">${n} path${n === 1 ? "" : "s"}; x y; x y | ...</span><textarea name="${p.name}" rows="2" spellcheck="false">${esc(text)}</textarea></label>`;
      } else if (p.kind === "gcode") {
        // (a custom toolpath) its program: pasted, or a file chosen; its lines read as the
        // planner reads them, what it cannot read or plan listed with the line
        const text = typeof v === "string" ? v : "";
        const n = text ? text.split("\n").length : 0;
        html += `<label title="${esc(p.help)}">${label} <span class="muted">${n} line${n === 1 ? "" : "s"}</span><textarea name="${p.name}" rows="8" spellcheck="false" placeholder="Paste G-code here, or choose a file" class="gcode-text">${esc(text)}</textarea></label>
          <div class="row small"><label class="filebtn">Choose a G-code file<input type="file" accept=".nc,.gcode,.ngc,.tap,.cnc,.txt" data-gcode="${p.name}" hidden></label></div>
          <div id="gcodeLines" class="gcode-lines small">${gcodeLinesHtml(op)}</div>`;
      } else if (p.kind === "moves") {
        html += `<label title="${esc(p.help)}">${label} <span class="muted">JSON</span><textarea name="${p.name}" rows="3" spellcheck="false">${esc(Array.isArray(v) && v.length ? JSON.stringify(v) : "")}</textarea></label>`;
      } else if (p.kind === "points") {
        html += `<label title="${esc(p.help)}">${label} <span class="muted">x y; x y; ...</span><input name="${p.name}" value="${esc(Array.isArray(v) ? pointsText(v) : "")}" placeholder="required"></label>`;
      } else if (p.kind === "points3") {
        html += `<label title="${esc(p.help)}">${label} <span class="muted">x y diameter; ...</span><input name="${p.name}" value="${esc(Array.isArray(v) ? v.map((q) => q.join(" ")).join("; ") : "")}" placeholder="none"></label>`;
      } else {
        // (a number left at its default, not written in the project: its default shown)
        const text = v == null ? (typeof p.default === "number" ? p.default : "") : Array.isArray(v) ? v.join(", ") : v;
        html += `<label title="${esc(p.help)}">${label}<input name="${p.name}" value="${esc(text)}" placeholder="${p.default != null ? esc(Array.isArray(p.default) ? p.default.join(", ") : p.default) : p.required ? "required" : ""}"></label>`;
      }
    };
    for (const p of ty.params) {
      const at = html.length;
      field(p);
      if (TYPED_GEOMETRY.has(p.name)) {
        adv += html.slice(at);
        html = html.slice(0, at);
      } else if (p.advanced) {
        more += html.slice(at);
        html = html.slice(0, at);
      }
    }
    html += svg;
    if (more) html += `<details class="op-more" id="opMore" ${t.moreOpen ? "open" : ""}><summary>More</summary>${more}</details>`;
    const faces = op.geometry?.find((g) => g.kind === "faces");
    const ax = op.geometry?.find((g) => g.kind === "axial_range");
    if (op.type === "raster3d") html += `<div class="muted small" id="rasterInfo">${esc(rasterInfo(op))}</div>`;
    // tabs placed by hand: a click on the outline adds one there, a click on one removes it
    if (op.type === "contour" && !(t.draft && t.draft.op.id === op.id)) {
      const n = op.tab_at?.length || 0;
      html += `<div class="row small"><button type="button" id="placeTabs" title="Click on the outline in the 3D view to add a tab there (on a tab: remove it)">${t.tabMode ? "Done placing tabs" : "Place tabs on the model"}</button>
        <span id="tabInfo" class="muted">${n ? `${n} tab${n > 1 ? "s" : ""} placed` : "no tabs placed"}${op.tabs ? `, ${op.tabs} evenly round` : ""}${t.tabMode ? " · click the outline" : ""}</span>
        ${n ? '<button type="button" id="clearTabs">Clear</button>' : ""}</div>`;
    }
    const mode = pickModeOf(ty);
    if (mode) {
      const pk = t.picking ? t.pick : null;
      // (Mar: "I don't want a done picking button ... as you pick the toolpaths should
      // update") picking is on while the operation is selected: a click on the part picks or
      // unpicks, each one applied (one undo step) and planned again
      html += `<div class="row small" id="pickRow" title="Click ${esc(mode.kinds.map((k) => k.replace(/_/g, " ")).join(", "))} on the part in the 3D view to pick or unpick">
        ${pk && pk.sel.size && !mode.single ? '<button type="button" id="pickSimilar" title="Add every feature like the ones picked">Select all similar</button>' : ""}
        <span id="pickInfo">${esc(pickInfoText(op, mode))}</span>
        ${mode.faces && faces ? '<button type="button" id="clearFaces">Clear</button>' : ""}</div>`;
      // nothing picked: said in one line (what to pick, or what it uses meanwhile)
      if (!op.picks?.length && !faces && !pk?.sel.size) html += `<div class="small muted" id="pickNone">${esc(nothingPicked(op, ty, mode))}</div>`;
      // what it uses (when not picking): a row per pick, × to remove it, hover to see it
      if (!pk && op.picks?.length) {
        const missing = (label) => op.error && op.error.includes(label);
        html += `<div id="geometryList" class="geometry-list"><div class="small muted">Geometry (${op.picks.length}) <button type="button" id="geomClear" title="Clear every pick">Clear all</button></div>${op.picks
          .map((sg, i) => `<div class="geom-row ${missing(sg.label) ? "missing" : ""}" data-geom="${i}" title="${missing(sg.label) ? "Not on the model now: pick it again" : "Hover to see it on the model"}">${esc(sg.label || sg.kind)}${missing(sg.label) ? " (missing)" : ""}<button type="button" data-ungeom="${i}" aria-label="Remove ${esc(sg.label || sg.kind)}">×</button></div>`)
          .join("")}</div>`;
      }
      if (pk) {
        html += `<div id="pickChips" class="pick-chips">${[...pk.sel]
          .map((id) => pk.list.find((x) => x.id === id))
          .filter(Boolean)
          .map((x) => `<span class="pick-chip" data-chip="${esc(x.id)}">${esc(x.label)}<button type="button" data-unpick="${esc(x.id)}" aria-label="Remove ${esc(x.label)}">×</button></span>`)
          .join("")}</div>`;
        if (pk.notes.length) html += `<div id="pickNotes" class="pick-notes small">${pk.notes.map(esc).join("<br>")}</div>`;
        if (op.type === "drill" && pk.notes.some((n) => /Bore/.test(n))) html += '<div class="row small"><button type="button" id="boreInstead" title="Bore the picked holes (the tool interpolates them)">Bore instead</button></div>';
      }
    }
    // face and axial selections bound rotary operations (X/A) and the 3D raster
    if (mode?.legacy) adv += `<label>axial range (mm)<input name="__axial" value="${ax ? ax.x.join(", ") : ""}" placeholder="all"></label>`;
    if (adv) html += `<details class="op-advanced" id="opAdvanced" ${t.advOpen ? "open" : ""}><summary>Advanced: coordinates typed by hand</summary>${adv}</details>`;
    const feedy = ty.params.some((p) => ["feed", "rough_feed", "finish_feed", "plunge_feed"].includes(p.name));
    html += `<div id="opErrors" class="error small hidden"></div>${feedy ? '<div id="opFeedsNotes" class="small muted" aria-live="polite"></div>' : ""}<div class="row">${feedy ? '<button type="button" id="opCalcFeeds" title="Calculate the feeds for this tool, its stepover and depth in the setup\'s material at the setup\'s spindle speed (chip load, chip thinning), within the machine\'s cap">Calculate feeds</button>' : ""}</div></form>`;
    const cur = kept?.el || ed;
    const focus = kept?.focus || focusIn(ed);
    if (cur.__html === html) {
      // nothing it shows changed: the same form, as the person left it
      if (cur !== ed) ed.replaceWith(cur);
      refocus(cur, focus);
      showUses(op);
      return;
    }
    // (the same operation drawn anew: what was typed in it and not applied carries over)
    const before = cur.__html && cur.__op === `${op.id}:${op.type}` ? cur.querySelector("#opForm") : null;
    ed.innerHTML = html;
    ed.__html = html;
    ed.__op = `${op.id}:${op.type}`;
    carryEdits(before, ed.querySelector("#opForm"));
    refocus(ed, focus);
    showUses(op);
  }

  function readForm() {
    const op = opById(t.selected);
    const ty = typeOf(op.type);
    const f = $("opForm");
    const out = { ...op };
    out.name = f.elements.__name.value.trim();
    if (!out.name) delete out.name;
    // (Mar typing an X range "-12 0": "-", "-1", "-12 " on the way, each planned or refused in
    // red as it came) the fields not a whole value yet: the form waits for them
    const halfTyped = [];
    const whole = { vec2: 2, rect: 4 };
    for (const p of ty.params) {
      const el = f.elements[p.name];
      if (!el) continue;
      if (p.kind === "boolean") out[p.name] = el.checked;
      else if (p.name === "tool" || p.kind === "integer") out[p.name] = el.value === "" || el.value === "__add" ? (p.name === "tool" ? op.tool : undefined) : parseInt(el.value, 10);
      else if (p.kind === "choice") out[p.name] = el.value;
      else if (p.kind === "string" || p.kind === "gcode") out[p.name] = el.value;
      else if (p.kind === "moves") {
        const v = el.value.trim();
        if (v) {
          try {
            out[p.name] = JSON.parse(v);
          } catch (e) {
            throw new Error(`${p.name}: not JSON (${e.message})`);
          }
        } else out[p.name] = [];
      } else if (p.kind === "paths") {
        out[p.name] = el.value
          .split("|")
          .map((path) => path.split(";").map((q) => q.split(/[,\s]+/).filter(Boolean).map(Number)).filter((q) => q.length === 2))
          .filter((path) => path.length >= 2);
      } else if (p.kind === "points") {
        const pts = el.value.split(";").map((q) => q.split(/[,\s]+/).filter(Boolean).map(Number)).filter((q) => q.length);
        out[p.name] = pts.length ? pts : undefined;
      } else if (p.kind === "points3") {
        out[p.name] = el.value.split(";").map((q) => q.split(/[,\s]+/).filter(Boolean).map(Number)).filter((q) => q.length === 3);
      } else if (["vec2", "rect", "numbers", "integers"].includes(p.kind)) {
        const nums = el.value.split(/[,\s]+/).filter(Boolean).map(Number);
        if (nums.some((v) => !Number.isFinite(v)) || (whole[p.kind] && nums.length && nums.length !== whole[p.kind])) halfTyped.push(p.name);
        out[p.name] = nums.length ? nums : undefined;
      } else {
        out[p.name] = el.value === "" ? undefined : Number(el.value);
        if (out[p.name] !== undefined && !Number.isFinite(out[p.name])) halfTyped.push(p.name);
      }
      if (out[p.name] === undefined) delete out[p.name];
    }
    const axial = f.elements.__axial;
    if (axial) {
      out.geometry = (out.geometry || []).filter((g) => g.kind !== "axial_range");
      const v = axial.value.split(/[,\s]+/).filter(Boolean).map(Number);
      if (v.length === 2) out.geometry.push({ kind: "axial_range", x: v });
      if (!out.geometry.length) delete out.geometry;
    }
    if (halfTyped.length) {
      const need = (n) => (whole[ty.params.find((p) => p.name === n)?.kind] ? `${whole[ty.params.find((p) => p.name === n).kind]} numbers` : "a number");
      const e = new Error(halfTyped.map((n) => `${n}: ${need(n)}`).join("; "));
      e.halfTyped = true;
      throw e;
    }
    return out;
  }

  /** A custom toolpath's program as the planner reads it: what it can't read or plan, each
   *  with its line, and the program's lines around them; read again when the text changes. */
  const gcodeChecks = new Map();
  function gcodeLinesHtml(op) {
    const text = op.gcode || "";
    if (!text.trim()) return '<span class="muted">No program yet: paste it above, or choose a file.</span>';
    const frame = isBox(setup()) ? "three" : "rotary";
    const key = `${frame}|${text}`;
    const got = gcodeChecks.get(op.id);
    if (!got || got.key !== key) {
      gcodeChecks.set(op.id, { key, res: null });
      engine("readGcode", text, frame).then(({ result }) => {
        const c = gcodeChecks.get(op.id);
        if (c && c.key === key) {
          c.res = result;
          const el = $("gcodeLines");
          if (el && t.selected === op.id) el.innerHTML = gcodeLinesHtml(opById(op.id) || op);
        }
      }).catch(() => {});
      return '<span class="muted">Reading the program...</span>';
    }
    if (!got.res) return '<span class="muted">Reading the program...</span>';
    const r = got.res;
    const lines = text.split("\n");
    const msgs = [...r.errors.map((m) => ["error", m]), ...r.warnings.map((m) => ["warn", m])];
    const head = `<div class="${r.errors.length ? "error" : "muted"}">${r.feeds} feed move${r.feeds === 1 ? "" : "s"} read${r.tools.length ? `, tools ${r.tools.map((x) => "T" + x).join(", ")}` : ""}; ${r.errors.length} error${r.errors.length === 1 ? "" : "s"}, ${r.warnings.length} warning${r.warnings.length === 1 ? "" : "s"}</div>`;
    const rows = msgs
      .map(([kind, m]) => {
        const ln = +(/^line (\d+)/.exec(m)?.[1] || 0);
        const src = ln ? `<code>${ln}: ${esc((lines[ln - 1] || "").trim())}</code> ` : "";
        return `<div class="gcode-msg ${kind === "error" ? "error" : "warn"}">${src}${esc(m.replace(/^line \d+: /, ""))}</div>`;
      })
      .join("");
    return head + rows;
  }

  /** What a 3D parallel finish will do: its stepover, rows and levels (and a hint when no 3D
   *  roughing comes before it, so it also clears the stock). */
  function rasterInfo(op) {
    const s = setup();
    const span = Math.abs((op.y_range?.[1] ?? 0) - (op.y_range?.[0] ?? 0));
    const step = op.stepover > 0 ? op.stepover : 0;
    const rows = step ? Math.ceil(span / step) + 1 : 0;
    const top = s.stock?.shape === "box" ? s.stock.max[2] : isBox(s) ? 0 : stockRadius(s.stock);
    const depth = top - ((op.floor ?? 0) + (op.leave ?? 0));
    const levels = op.step_down > 0 ? Math.max(1, Math.ceil(depth / op.step_down - 1e-9)) : 1;
    const roughed = s.operations.some((o) => o.type === "rough3d" && o.enabled !== false);
    let text = `stepover ${step} mm${op.scallop > 0 ? ` (a ${op.scallop} mm cusp)` : ""} · ${rows} rows · up to ${levels} level${levels === 1 ? "" : "s"}`;
    if (!roughed && levels > 1) text += " — no 3D roughing before it, so it clears the stock too: add a 3D roughing to finish in one pass";
    return text;
  }

  // ------------------------------------------------------------ picking
  // One picking mode for every operation: the features it takes light up, a click toggles one
  // (pocket, contour and face take one), "select all similar" adds the ones alike. Finishing
  // operations record the picked faces (a faces selection); the others take the fields the
  // engine fills from the picks (drill points and depth, a pocket's outline, ...).
  const FACE_TYPES = ["raster3d", "rough3d", "waterline", "rotary_finish", "rotary_parallel"];
  const SINGLE = ["helical_knurl", "flank_chamfer", "engrave", "vcarve"];
  /** The parameters that are geometry typed as numbers (paths, points, corners, ranges, the
   *  body's ends, an edge, a cut's place, tabs' places): under Advanced, not in the form. */
  const TYPED_GEOMETRY = new Set(["paths", "points", "holes", "rect", "path", "x_range", "y_range", "a_range", "body_back", "body_front", "edge", "x", "tab_at"]);

  /** (the part imported into a stock it didn't fit) the red "bigger than the stock" said
   *  then: gone once the stock is changed (Fit to part, a size typed); the next plan says
   *  what is wrong if anything still is. (With nothing planned before a pick it stayed up,
   *  over the form's buttons.) */
  function staleFitWarning() {
    const el = $("error");
    if (el && /is bigger than the stock/.test(el.textContent)) showError("");
  }

  /** The types whose geometry is a placeholder until something is picked (a contour round
   *  the whole bar, a pocket's rectangle): a new one is not planned before a pick. */
  const NEEDS_PICK = new Set(["contour", "pocket", "drill", "chamfer", "thread_mill", "bore"]);
  /** (Mar's picking rule) a new operation of those types with nothing picked and no
   *  coordinates typed under Advanced: its form and the one-line prompt only, not planned (a
   *  contour round the whole bar showed a red holder collision before its pick). */
  function unpicked(op) {
    // (a new custom toolpath: nothing to plan until its program is given)
    if (op?.type === "custom") return !!(t.draft && t.draft.op.id === op.id) && !op.gcode?.trim() && !op.moves?.length;
    if (!op || !NEEDS_PICK.has(op.type) || !(t.draft && t.draft.op.id === op.id) || op.picks?.length) return false;
    return ![...(t.edited || [])].some((k) => TYPED_GEOMETRY.has(k));
  }

  function pickModeOf(ty) {
    if (!ty) return null;
    // before picking existed per type: rotary operations (and the raster) were bounded by faces
    const legacy = ty.kind === "raster3d" || (!ty.machine_needs.includes("xyz") && ty.geometry.some((g) => g === "mesh" || g === "profile" || g === "axial_range"));
    const own = ty.picks || [];
    const kinds = own.length ? own : legacy ? ["cylindrical_band", "revolved_band", "surface"] : [];
    if (!kinds.length) return null;
    return { kinds, faces: FACE_TYPES.includes(ty.kind) || !own.length, single: SINGLE.includes(ty.kind), legacy };
  }

  /** An operation with nothing picked, in one line: what to pick on the part, and what it
   *  uses meanwhile (the coordinates typed under Advanced, the part's own defaults). */
  function nothingPicked(op, ty, mode) {
    const names = { flat_face: "a flat face", side_face: "a side face", pocket: "a pocket", hole: "a hole", outline: "an outline", edge_loop: "an edge loop", surface: "a surface", cylindrical_band: "a turned diameter", revolved_band: "a turned profile", circular_edge: "a round edge" };
    const list = [...new Set(mode.kinds.map((k) => names[k] || k.replace(/_/g, " ")))];
    const what = list.length > 1 ? `${list.slice(0, -1).join(", ")} or ${list[list.length - 1]}` : list[0];
    const typed = ty.params.some((p) => TYPED_GEOMETRY.has(p.name) && (Array.isArray(op[p.name]) ? op[p.name].length : op[p.name] != null));
    // (Mar's knob4: rotary roughing planned under "Nothing picked. Pick ...") an operation
    // that needs no pick works on the whole part meanwhile: said so, the pick only limits it
    if (!NEEDS_PICK.has(op.type)) return `Nothing picked: it works on the whole part${typed ? " (as far as the coordinates under Advanced say)" : ""}. Pick ${what} to limit it.`;
    const meanwhile = unpicked(op) ? "" : typed ? " (meanwhile: the coordinates under Advanced)" : mode.faces || mode.legacy ? " (meanwhile: the whole part)" : "";
    return `Nothing picked. Pick ${what} on the part${meanwhile}.`;
  }

  function pickInfoText(op, mode) {
    const pk = t.picking ? t.pick : null;
    if (pk?.hover) return pk.list.find((x) => x.id === pk.hover)?.label || "";
    if (pk) return pk.sel.size ? `${pk.sel.size} picked` : `click ${mode.kinds.map((k) => k.replace(/_/g, " ")).join(" / ")}`;
    if (mode.faces) {
      const f = op.geometry?.find((g) => g.kind === "faces");
      return f ? `${f.ids.length} face(s): ${f.ids.join(", ")}` : "whole model";
    }
    return "";
  }

  /** Which of `list` the operation already uses (its faces, or its fields). */
  function initialSel(op, list, mode) {
    // its picks, as it keeps them (each feature's signature)
    if (op.picks?.length) {
      const keys = new Set(op.picks.map((sg) => JSON.stringify(sg)));
      const hit = list.filter((x) => x.signature && keys.has(JSON.stringify(x.signature))).map((x) => x.id);
      if (hit.length) return hit;
    }
    if (mode.faces) {
      const f = op.geometry?.find((g) => g.kind === "faces");
      if (!f) return [];
      return list.filter((x) => x.faces.every((id) => f.ids.includes(id))).map((x) => x.id);
    }
    const near = (a, b) => Math.abs(a - b) < 0.01;
    if (op.type === "drill") return list.filter((h) => (op.points || []).some((q) => near(q[0], h.data.center[0]) && near(q[1], h.data.center[1]))).map((h) => h.id);
    const same = (a, b) => Array.isArray(a) && Array.isArray(b) && a.length === b.length && a.every((q, k) => near(q[0], b[k][0]) && near(q[1], b[k][1]));
    if (op.type === "pocket") return list.filter((x) => same(x.data.outline, op.paths?.[0])).map((x) => x.id);
    if (op.type === "contour") return list.filter((x) => same(x.data.path, op.path)).map((x) => x.id);
    return [];
  }

  function partModel(s) {
    return s.part.find((id) => t.doc.models.find((m) => m.id === id)?.kind === "mesh") || s.part[0];
  }

  async function startPicking(quiet = false) {
    const v = await getThree();
    if (!v || v.failed) return quiet ? null : showError("Picking needs the 3D view");
    const s = setup();
    const op = opById(t.selected);
    const mode = pickModeOf(typeOf(op.type));
    if (!mode) return;
    const model = partModel(s);
    const gen = t.gen, sel = t.selected;
    let regions, picks;
    try {
      [{ result: regions }, { result: picks }] = await Promise.all([
        engine("faceRegions", t.doc, JSON.stringify({ model, setup: s.id }), state.mesh),
        engine("pickables", t.doc, { setup: s.id, model, index_a: op.index_a ?? 0 }, state.mesh),
      ]);
    } catch (e) {
      t.pickFailed = sel;
      return quiet ? null : showError(`Picking: ${e.message}`);
    }
    // the project was replaced meanwhile (New, Open, an example), or another operation or
    // setup selected (a plan landing meanwhile only reads the same project again)
    if (gen !== t.gen || t.selected !== sel || t.setup !== s.id) return;
    const bytes = (b64) => (typeof b64 === "string" ? Uint8Array.from(atob(b64), (c) => c.charCodeAt(0)) : b64);
    t.regions = { model, list: regions.regions, tri: new Float32Array(bytes(regions.triangles).buffer), of: new Uint16Array(bytes(regions.region_of).buffer) };
    const list = picks.pickables.filter((x) => mode.kinds.includes(x.kind));
    // a face may belong to several features: the one the operation prefers comes first
    const byFace = new Map();
    for (const x of list) for (const f of x.faces) (byFace.get(f) || byFace.set(f, []).get(f)).push(x);
    for (const l of byFace.values()) l.sort((p, q) => mode.kinds.indexOf(p.kind) - mode.kinds.indexOf(q.kind));
    if (t.picking) return;
    t.pick = { mode, list, byFace, sel: new Set(initialSel(op, list, mode)), hover: null, notes: [], opId: op.id };
    const { THREE } = v;
    const g = new THREE.BufferGeometry();
    g.setAttribute("position", new THREE.BufferAttribute(t.regions.tri, 3));
    g.setAttribute("color", new THREE.BufferAttribute(new Float32Array(t.regions.tri.length).fill(0.7), 3));
    g.computeVertexNormals();
    t.pickMesh = new THREE.Mesh(g, new THREE.MeshStandardMaterial({ vertexColors: true, side: THREE.DoubleSide }));
    v.scene.add(t.pickMesh);
    v.groups.part.visible = false;
    // nothing generated yet, so the view was never framed: frame the part
    if (!v.fitted) {
      g.computeBoundingBox();
      const b = g.boundingBox;
      const c = b.getCenter(new THREE.Vector3());
      const span = Math.max(b.max.x - b.min.x, b.max.y - b.min.y, b.max.z - b.min.z);
      v.camera.position.set(c.x - span * 0.3, c.y - span * 1.2, c.z + span * 0.9);
      v.controls.target.copy(c);
      v.controls.update();
      v.fitted = "picking";
    }
    v.renderer.render(v.scene, v.camera);
    t.picking = true;
    dropOverlay();
    paintPicked();
    renderEditor();
  }

  /** Picking follows the selection: on while an operation that takes picks is selected (its
   *  own picks shown), off for anything else. */
  function autoPick(op) {
    const mode = pickModeOf(typeOf(op.type));
    const same = t.pick && t.pick.opId === op.id && mode && t.pick.mode.kinds.join() === mode.kinds.join();
    if (t.picking && !same) endPicking(false);
    if (!mode || t.picking || t.pickStarting === op.id || t.pickFailed === op.id || t.tabMode) return;
    t.pickStarting = op.id;
    queueMicrotask(async () => {
      try {
        if (t.selected === op.id && !t.picking) await startPicking(true);
      } finally {
        if (t.pickStarting === op.id) t.pickStarting = null;
      }
    });
  }

  /** Nothing selected (Escape, a click on empty space): picking ends with it. */
  function deselect() {
    if (!t.selected && !t.picking) return;
    if (t.picking) endPicking(false);
    t.selected = null;
    t.draft = null;
    t.retype = null;
    render();
  }

  function stopPicking() {
    endPicking(true);
    renderEditor();
  }

  /** Leave picking: the highlight goes; `write`: an applied operation's picks are saved. */
  function endPicking(write) {
    // the selection is applied now: one edit
    if (t.pickedChanged && write) writeJob(t.doc);
    t.pickedChanged = false;
    const v = state.three;
    if (t.pickMesh) {
      v.scene.remove(t.pickMesh);
      t.pickMesh.geometry.dispose();
      t.pickMesh.material.dispose();
      t.pickMesh = null;
    }
    if (v && !v.failed) window.__marcam.legacyLayers?.();
    t.picking = false;
    t.pick = null;
  }

  // colours: picked, under the pointer, can be picked, cannot
  const COLORS = { sel: [0.15, 0.39, 0.92], hover: [0.96, 0.62, 0.04], ok: [0.55, 0.8, 0.76], off: [0.72, 0.74, 0.78] };
  function paintPicked() {
    if (!t.pickMesh || !t.pick) return;
    const { byFace, sel, hover } = t.pick;
    const col = t.pickMesh.geometry.attributes.color.array;
    const colorOfRegion = t.regions.list.map((r) => {
      const ps = byFace.get(r.id);
      if (!ps) return COLORS.off;
      if (ps.some((x) => sel.has(x.id))) return COLORS.sel;
      if (ps.some((x) => x.id === hover)) return COLORS.hover;
      return COLORS.ok;
    });
    for (let k = 0; k < t.regions.of.length; k++) {
      const c = colorOfRegion[t.regions.of[k]];
      for (let v = 0; v < 3; v++) col.set(c, 9 * k + 3 * v);
    }
    t.pickMesh.geometry.attributes.color.needsUpdate = true;
  }

  /** The face region under a canvas point (CSS pixels relative to the canvas), or null. */
  function regionAt(px, py) {
    const v = state.three;
    if (!t.picking || !t.pickMesh) return null;
    const el = v.renderer.domElement;
    const ndc = new v.THREE.Vector2((px / el.clientWidth) * 2 - 1, -(py / el.clientHeight) * 2 + 1);
    const ray = new v.THREE.Raycaster();
    ray.setFromCamera(ndc, v.camera);
    const hit = ray.intersectObject(t.pickMesh)[0];
    return hit ? { ...t.regions.list[t.regions.of[hit.faceIndex]], point: hit.point } : null;
  }

  /** What an operation keeps of its picks: each feature's signature (kind, position, size). */
  const sigsOf = (list, ids) => ids.map((id) => list.find((x) => x.id === id)?.signature).filter(Boolean);
  function keepPicks(op, list, ids) {
    const sigs = sigsOf(list, ids);
    if (sigs.length) op.picks = sigs;
    else delete op.picks;
    // picked again: whatever was missing is not any more
    delete op.error;
  }

  /** The picks become the operation's faces, or the fields the engine fills from them. */
  async function applySel() {
    const op = opById(t.selected);
    const pk = t.pick;
    const { mode } = pk;
    keepPicks(op, pk.list, [...pk.sel]);
    if (mode.faces) {
      const ids = [...new Set([...pk.sel].flatMap((id) => pk.list.find((x) => x.id === id)?.faces || []))];
      op.geometry = (op.geometry || []).filter((g) => !(g.kind === "faces" && g.model === t.regions.model));
      if (ids.length) op.geometry.push({ kind: "faces", model: t.regions.model, ids });
      if (!op.geometry.length) delete op.geometry;
      pk.notes = [];
      // a side face (indexed): the index that turns it up
      const side = [...pk.sel].map((id) => pk.list.find((x) => x.id === id)).find((x) => x?.kind === "side_face");
      if (side) op.index_a = side.data.index_a;
    } else if (pk.sel.size) {
      try {
        const { result } = await engine("applyPicks", t.doc, { type: op.type, ids: [...pk.sel], setup: t.setup, model: t.regions.model, tool: op.tool, index_a: op.index_a ?? 0, op }, state.mesh);
        Object.assign(op, result.params);
        // (the picked feature's numbers, not what was typed in those fields before: kept as
        // an edit not yet applied, a field cleared stayed blank under the pick and the form
        // said it was missing)
        const f = $("opForm");
        if (f) for (const k of Object.keys(result.params)) for (const e of f.querySelectorAll(`[name="${CSS.escape(k)}"]`)) if ("defaultValue" in e) e.value = e.defaultValue;
        // a rectangle no longer applies once the outline is picked
        if (op.type === "pocket" && result.params.paths) delete op.rect;
        pk.notes = result.notes;
      } catch (e) {
        pk.notes = [e.message];
      }
    }
    paintPicked();
    // each pick an edit of its own (one undo step), planned again as it is made
    if (!t.draft) {
      const what = pk.sel.size ? `${pk.sel.size} picked` : "nothing picked";
      await commit({ label: `Picked for ${op.id} (${what}).` });
    }
    if (t.draft && mode.faces) await refreshDefaults();
    else renderEditor();
    // (a new one picked for the first time joins the project here: picking goes on, more
    // features may be picked)
    if (t.draft) await applyForm({ quiet: true, keepPicking: true });
  }

  /** Toggle the feature under a canvas point; returns the face region hit (with the feature
   *  picked as `.pickable`), or null. */
  async function pickAt(px, py) {
    const region = regionAt(px, py);
    if (!region) return null;
    const pk = t.pick;
    // the operation's preferred kind on this face; of several (a face bordered by two round
    // edges), the one nearest the click along X
    const cands = pk.byFace.get(region.id) || [];
    const best = cands.filter((c) => c.kind === cands[0]?.kind);
    const x = best.length > 1 && best.every((c) => typeof c.data.x === "number") ? best.reduce((a, c) => (Math.abs(c.data.x - region.point.x) < Math.abs(a.data.x - region.point.x) ? c : a)) : best[0];
    if (!x) {
      pk.notes = [`not a ${pk.mode.kinds.map((k) => k.replace(/_/g, " ")).join(" or ")}`];
      renderEditor();
      return { ...region, pickable: null };
    }
    if (pk.mode.single) pk.sel = pk.sel.has(x.id) ? new Set() : new Set([x.id]);
    else if (pk.sel.has(x.id)) pk.sel.delete(x.id);
    else pk.sel.add(x.id);
    await applySel();
    // parting on a band: where the click is along X, snapped to the band's end within 1 mm
    const op = opById(t.selected);
    if (op?.type === "parting" && pk.sel.has(x.id) && /band$/.test(x.kind) && region.point && Array.isArray(x.data.x)) {
      const [lo, hi] = x.data.x;
      const px = region.point.x;
      const snap = [lo, hi].find((e) => Math.abs(e - px) < 1.0);
      op.x = Math.round((snap ?? px) * 1000) / 1000;
      pk.notes = [snap !== undefined ? `at the band's ${snap === lo ? "back" : "front"}, X${op.x.toFixed(3)}` : `at X${op.x.toFixed(3)}, where you clicked`];
      renderEditor();
    }
    return { ...region, pickable: x };
  }

  /** The drilling being picked becomes a bore of the same holes (same id, same file). */
  async function boreInstead() {
    const pk = t.pick;
    const op = opById(t.selected);
    const ids = [...pk.sel];
    const d = Math.max(...ids.map((id) => pk.list.find((x) => x.id === id)?.data.diameter || 0));
    const s = setup();
    try {
      const { result: def } = await engine("opDefaults", t.doc, { type: "bore", setup: s.id, tool: boreTool(d), machine: machineArg?.() ?? null }, state.mesh);
      const { result: filled } = await engine("applyPicks", t.doc, { type: "bore", ids, setup: s.id, model: t.regions.model, tool: def.tool, index_a: op.index_a ?? 0 }, state.mesh);
      const bore = { id: op.id, ...(op.name ? { name: op.name } : {}), ...(op.color ? { color: op.color } : {}), ...def, ...filled.params, index_a: op.index_a ?? 0 };
      keepPicks(bore, pk.list, ids);
      if (t.draft) {
        t.draft.op = bore;
        t.draft.title = typeOf("bore")?.title || "bore";
      } else {
        s.operations[s.operations.findIndex((o) => o.id === op.id)] = bore;
        t.pickedChanged = true;
      }
      stopPicking();
      if (!t.draft) await commit();
      else renderEditor();
      if (filled.notes.length) showError(filled.notes.join("; "));
    } catch (e) {
      showError(`Bore instead: ${e.message}`);
    }
  }

  /** Add every feature alike the picked ones. */
  async function pickSimilar() {
    const pk = t.pick;
    const keys = new Set([...pk.sel].map((id) => pk.list.find((x) => x.id === id)?.similar));
    for (const x of pk.list) if (keys.has(x.similar)) pk.sel.add(x.id);
    await applySel();
  }

  /** The feature under the pointer lights up, its label shows. */
  let hoverFrame = 0;
  function hoverAt(px, py) {
    if (!t.picking || !t.pick || hoverFrame) return;
    hoverFrame = requestAnimationFrame(() => {
      hoverFrame = 0;
      if (!t.pick) return;
      const r = regionAt(px, py);
      const id = r ? (t.pick.byFace.get(r.id)?.[0]?.id ?? null) : null;
      if (id === t.pick.hover) return;
      t.pick.hover = id;
      paintPicked();
      const info = $("pickInfo");
      if (info) info.textContent = pickInfoText(opById(t.selected), t.pick.mode);
    });
  }

  // ------------------------------------------------------------ detected features
  // A vise or indexed setup's holes (grouped by "similar") and pockets, each with a button that
  // adds the operation for it
  let detected = { key: null, list: [] };
  async function detectFeatures() {
    const s = setup();
    if (!$("detected") || !s || !state.mesh || !["vise", "indexed"].includes(kindOfSetup(s))) return;
    const key = JSON.stringify([s.id, s.part, t.doc.models, state.mesh.length]);
    if (detected.key !== key) {
      try {
        const { result } = await engine("pickables", t.doc, { setup: s.id, model: partModel(s) }, state.mesh);
        detected = { key, list: result.pickables };
      } catch {
        detected = { key, list: [] };
      }
    }
    const groups = new Map();
    for (const x of detected.list) if (x.kind === "hole") (groups.get(x.similar) || groups.set(x.similar, []).get(x.similar)).push(x);
    let html = "";
    for (const hs of groups.values()) {
      const h = hs[0].data;
      // a flat tool of the holes' size drills them; else they are bored
      const how = drillTool(h.diameter) != null ? "drill" : "bore";
      html += `<button type="button" class="detected-btn" data-start="${how}" data-ids="${esc(hs.map((x) => x.id).join(","))}">${how === "drill" ? "Drill" : "Bore"} ${hs.length} × ${h.diameter.toFixed(2)} mm ${h.through ? "through" : "blind"}</button>`;
    }
    for (const x of detected.list.filter((x) => x.kind === "pocket")) html += `<button type="button" class="detected-btn" data-start="pocket" data-ids="${esc(x.id)}">${esc(x.label)}</button>`;
    if (setup() === s && $("detected")) $("detected").innerHTML = html ? `<span class="muted">Detected:</span> ${html}` : "";
  }

  /** The project's flat tool of a hole's size (drilling), or null. */
  function drillTool(d) {
    return (t.doc.tools || []).find((x) => x.kind === "flat" && Math.abs((x.diameter ?? -1) - d) <= 0.05)?.number ?? null;
  }

  /** The project's largest flat tool that fits a hole with room to bore it, or undefined. */
  function boreTool(d) {
    const fits = (t.doc.tools || []).filter((x) => x.kind === "flat" && x.diameter && x.diameter <= d - 0.2);
    return fits.sort((a, b) => b.diameter - a.diameter)[0]?.number;
  }

  /** A new operation id for setup `s`: unused in the whole project (ids are project-wide). */
  function newOpId(s) {
    const used = new Set(t.doc.setups.flatMap((x) => x.operations.map((o) => o.id)));
    for (let k = s.operations.length + 1; ; k++) if (!used.has(`op${k}`)) return `op${k}`;
  }

  /** A new operation of `type` for the features `ids`: engine defaults, then the picks. */
  async function startFromDetected(type, ids) {
    const s = setup();
    const id = newOpId(s);
    try {
      // drilling: the flat tool of the holes' size; boring: the largest that fits inside
      let tool;
      const d = detected.list.find((x) => x.id === ids[0])?.data.diameter;
      if (type === "drill") tool = drillTool(d) ?? undefined;
      if (type === "bore") tool = boreTool(d);
      const { result: op } = await engine("opDefaults", t.doc, { type, setup: s.id, tool, machine: machineArg?.() ?? null }, state.mesh);
      const { result: filled } = await engine("applyPicks", t.doc, { type, ids, setup: s.id, model: partModel(s), tool: op.tool }, state.mesh);
      const full = { id, ...op, ...filled.params };
      if (type === "pocket") delete full.rect;
      keepPicks(full, detected.list, ids);
      s.operations.push(full);
      let out = s.outputs[0];
      if (!out) s.outputs.push((out = { name: `${s.id}_${id}`, title: typeOf(type)?.title || type, operations: [] }));
      out.operations.push(id);
      t.selected = id;
      await commit({ label: `Added ${id}.` });
      if (filled.notes.length) showError(filled.notes.join("; "));
    } catch (e) {
      showError(`Cannot add ${type}: ${e.message}`);
    }
  }

  // ------------------------------------------------------------ events
  $("tree").addEventListener("click", async (ev) => {
    // empty space of the tree (no row, no form): the operation deselected, picking ended
    if (t.selected && ev.target.matches?.("#tree, .tree-list, .tree-setupbody, .tree-setup")) return deselect();
    // (a line's grey text is part of its label: a click there is a click on the line; the
    // Stock and Part lines did nothing clicked on their sizes or file name, as a person does)
    // (Mar: "the setup line is hard to click on ... different than the other lines in the
    // tree where you can click anywhere on the row") a click anywhere on a row (a setup, the
    // Tool library, Stock, Part, Fixture, a tool, an operation), not on one of its controls,
    // is a click on its label
    const control = ev.target.closest?.("button, input, select, textarea, a");
    const rowEl = !control && ev.target.closest?.(".tree-setuphead, .tree-stock, .tree-tool, .tree-op");
    const label = !control && (ev.target.closest?.(".tree-label[data-act]") || rowEl?.querySelector(".tree-label[data-act]"));
    const el = label || ev.target;
    if (el.id === "treeAdd") {
      const picked = $("treeAddType").value;
      // (indexed roughing: 3D roughing, adaptive, at six angles between the blades)
      const indexed = picked === INDEXED_ROUGH;
      const kind = indexed ? "rough3d" : picked;
      const ty = indexed ? { ...typeOf(kind), title: "Indexed roughing (3+1)" } : typeOf(kind);
      const s = setup();
      const id = newOpId(s);
      // a draft with every field filled by the engine's defaults (from the setup's tools,
      // spindle, machine limits and part faces); it joins the project on Apply
      // (into the setup's last output file, or a new one when it has none: Mar dropped the
      // chooser; an operation dragged in the tree goes to another file)
      const out = s.outputs[s.outputs.length - 1]?.name ?? "__new";
      // a draft already open is replaced; picking for it ends without writing anything
      if (t.picking) endPicking(false);
      t.retype = null;
      t.typeNote = null;
      t.selected = id;
      t.edited = new Set();
      const gen = t.gen;
      try {
        const { result } = await engine("opDefaults", t.doc, { type: kind, setup: s.id, machine: machineArg?.() ?? null }, state.mesh);
        // the project was replaced meanwhile (New, Open, an example), another operation or
        // setup selected, or the id taken (an undo brought one back). (A plan landing
        // meanwhile reads the same project again: t.doc is a new copy, not another project.)
        if (gen !== t.gen || t.selected !== id || t.setup !== s.id || setup()?.operations.some((o) => o.id === id)) return;
        t.draft = { op: { id, ...result, ...(indexed ? { name: "Indexed roughing (3+1)", pattern: "adaptive", index_count: 6, indexes: [] } : {}) }, out, title: ty.title };
        render();
        await applyForm({ quiet: true });
      } catch (e) {
        t.draft = null;
        t.selected = null;
        render();
        // (the walkthrough: a knurl in a setup with no V-bit said so, below the fold) the
        // library opened on the tools that suit it; one picked, the operation is added
        if (/needs an? [\w ]+ tool; the job has none/.test(e.message) && window.__marcam.tools?.pick && !ev.__retried) {
          const n = await window.__marcam.tools.pick(ty.tool_shapes);
          await refresh();
          if (n) {
            $("treeAddType").value = picked;
            ev.__retried = true;
            return $("treeAdd").dispatchEvent(Object.assign(new MouseEvent("click", { bubbles: true }), { __retried: true }));
          }
        }
        showError(`Cannot add ${ty.title}: ${e.message}`);
      }
      return;
    }
    if (el.dataset?.preset && el.closest?.(".part-presets")) {
      await placePreset(el.dataset.preset, el.textContent.trim(), el.dataset);
      return;
    }
    if (el.id === "ptInch") {
      await window.__marcam.project.scalePart(t.setup, 25.4);
      return;
    }
    if (el.id === "ptReplace") {
      $("replaceModelFile").click();
      return;
    }
    if (el.dataset?.act === "library") {
      if (t.picking) endPicking(false);
      t.focus = "library";
      t.selected = null;
      t.draft = null;
      t.retype = null;
      return render();
    }
    if (el.dataset?.act === "stock" || el.dataset?.act === "part" || el.dataset?.act === "fixture") {
      if (t.picking) endPicking(false);
      t.focus = el.dataset.act;
      t.selected = null;
      t.draft = null;
      t.retype = null;
      render();
      window.__marcam.handles?.refresh?.();
      return;
    }
    // the setup's type, from the row at the top of its details: its own type button chosen
    // a carried piece on the 4th axis turned by a quick button
    const cq = el.closest?.("#carryQuick button");
    if (cq) {
      const s = setup();
      const label = s && carryQuick(s, cq.dataset.carryAxis || cq.dataset.carry);
      if (label) await commit({ label });
      return;
    }
    const kindCard = el.closest?.("[data-setkind]");
    if (kindCard) {
      const radio = document.querySelector(`#setupDialog.embedded input[name="sdKind"][value="${kindCard.dataset.setkind}"]`);
      if (!radio) return;
      if (radio.disabled) return showError(radio.closest(".sd-kind")?.title || "This setup can't be that type: a vise holds a part's mesh, a block or the stock a setup before left.");
      if (!radio.checked) radio.click();
      return;
    }
    if (el.id === "stFit") {
      const s = setup();
      const m = meshModelOf(t.doc, s);
      const bytes = m && window.__marcam.meshFor?.(m.file);
      if (!bytes) return;
      const { parseStl } = await import("./project.js");
      const label = fitStock(t.doc, s, parseStl(bytes).tris, t.stMargin ?? (s.kind === "vise" || s.holding?.kind === "vise" ? 2 : 0.5));
      if (label) {
        staleFitWarning();
        await commit({ label });
      }
      return;
    }
    if (el.dataset?.act === "tool") {
      if (t.picking) endPicking(false);
      t.focus = "tool";
      t.tool = +el.closest(".tree-tool").dataset.tool;
      t.selected = null;
      t.draft = null;
      t.retype = null;
      return render();
    }
    if (el.id === "treeAddTool") {
      // a library tool for this setup: into the project (the next number), listed here
      const n = await window.__marcam.tools?.pick();
      if (!n) return;
      await refresh();
      const s = setup();
      if (!s) return;
      s.tools = [...new Set([...(s.tools || []), n])].sort((a, b) => a - b);
      t.focus = "tool";
      t.tool = n;
      t.selected = null;
      return commit({ label: `Added T${n} to ${s.name || s.id}.` });
    }
    if (el.id === "treeAddExport") return newExport();
    const expRow = el.closest?.(".tree-exp");
    if (expRow && el.dataset?.act === "writeexp") return doWriteExport(expRow.dataset.export, false);
    if (expRow && el.dataset?.act === "export") {
      if (t.picking) endPicking(false);
      t.focus = "export";
      t.export = expRow.dataset.export;
      t.selected = null;
      t.draft = null;
      t.retype = null;
      t.multi = null;
      return render();
    }
    if (el.id === "expWrite" || el.id === "expWriteAs") return doWriteExport(t.export, el.id === "expWriteAs");
    if (el.id === "expDelete") {
      const s = setup();
      const name = t.export;
      if (!s?.exports) return;
      s.exports = s.exports.filter((e) => e.name !== name);
      t.focus = "setup";
      moveChosen(expKey(s, name), null);
      return commit({ label: `Deleted the NC export ${name}.` });
    }
    const head = el.closest?.(".tree-setup");
    if (el.dataset?.act === "delsetup" && head) return deleteSetup(head.dataset.setup);
    if (el.dataset?.act === "fold" && head) {
      const id = head.dataset.setup;
      const x = t.doc.setups.find((y) => y.id === id);
      if (x) t.fold.set(id, !isOpen(x));
      return render();
    }
    if (el.dataset?.act === "setup" && head) {
      // a setup's name: it is worked on, its details shown
      t.focus = "setup";
      if (t.picking) endPicking(false);
      switchSetup(head.dataset.setup);
      t.selected = null;
      t.draft = null;
      t.retype = null;
      t.typeNote = null;
      return render();
    }
    if (el.dataset?.act === "select" && head && head.dataset.setup !== t.setup) {
      // an operation of another setup: that setup, the operation selected in it
      const id = el.closest(".tree-op")?.dataset.op;
      switchSetup(head.dataset.setup);
      t.selected = id || null;
      return render();
    }
    if (el.id === "treeAddSetup") {
      const kind = $("treeAddKind")?.value || "vise";
      const independent = $("treeAddFrom")?.value === "own";
      // (until the new setup is there, the operation list is the old setup's: an operation
      // picked meanwhile went to the old one, or was lost when the list changed under it)
      t.addingSetup = true;
      for (const id of ADD_CONTROLS) if ($(id)) $(id).disabled = true;
      try {
        await window.__marcam.project.addSetup({ kind, independent });
      } finally {
        t.addingSetup = false;
        for (const id of ADD_CONTROLS) if ($(id)) $(id).disabled = false;
      }
      return;
    }
    if (el.id === "treeStockStl") {
      // (Mar) the stock this setup starts with, as a file
      try {
        const s = setup();
        el.disabled = true;
        const { result } = await engine("carriedMesh", t.doc, { setup: s.id, stl: true }, state.mesh);
        const a = document.createElement("a");
        a.href = URL.createObjectURL(new Blob([result.stl], { type: "model/stl" }));
        a.download = `${s.id}_incoming_stock.stl`;
        document.body.appendChild(a);
        a.click();
        a.remove();
        setTimeout(() => URL.revokeObjectURL(a.href), 1000);
      } catch (e) {
        showError(`The incoming stock: ${e.message}`);
      } finally {
        el.disabled = false;
      }
      return;
    }
    if (el.id === "opCalcFeeds") {
      await calcOpFeeds();
      return;
    }
    if (el.id === "cancelDraft") {
      if (t.picking) endPicking(false);
      // a new operation goes; an applied one keeps its type and stays selected
      if (t.draft && t.draft.op.id === t.selected) {
        t.draft = null;
        t.selected = null;
      }
      t.retype = null;
      t.typeNote = null;
      render();
      return;
    }
    if (el.id === "treeWizard") {
      try {
        const s = setup();
        const doc = structuredClone(t.doc);
        const used = new Set(doc.setups.flatMap((x) => x.outputs.map((o) => o.name)));
        const params = { setup: s.id, pattern: document.getElementById("treeWizardPattern")?.value || "diamond" };
        if (used.has("knob_1_profile") || used.has("knob_2_knurl")) Object.assign(params, { turn_output: `${s.id}_turn`, knurl_output: `${s.id}_knurl` });
        const { result } = await engine("wizardKnurl", doc, params, state.mesh);
        t.doc = result.project;
        await commit({ label: "Added the knurling." });
      } catch (e) {
        showError(e.message);
      }
      return;
    }
    if (el.id === "geomClear") return removeUse(null);
    if (el.dataset?.ungeom != null) return removeUse(Number(el.dataset.ungeom));

    if (el.id === "placeTabs") {
      t.tabMode = !t.tabMode;
      return renderEditor();
    }
    if (el.id === "clearTabs") {
      const op = setup()?.operations.find((o) => o.id === t.selected);
      if (op) delete op.tab_at;
      return commit();
    }
    if (el.id === "pickSimilar") return pickSimilar();
    if (el.id === "boreInstead") return boreInstead();
    if (el.dataset?.unpick && t.pick) {
      t.pick.sel.delete(el.dataset.unpick);
      return applySel();
    }
    if (el.dataset?.start) return startFromDetected(el.dataset.start, el.dataset.ids.split(","));
    if (el.id === "clearFaces") {
      const op = opById(t.selected);
      op.geometry = (op.geometry || []).filter((g) => g.kind !== "faces");
      if (!op.geometry.length) delete op.geometry;
      if (t.pick) t.pick.sel.clear();
      paintPicked();
      return commit();
    }
    if (el.id === "eyeAll") return setEyes((h) => setupOps().forEach((id) => h.delete(eyeKey(id))));
    if (el.id === "eyeNone") return setEyes((h) => setupOps().forEach((id) => h.add(eyeKey(id))));
    if (el.dataset?.outEye) {
      const out = setup().outputs.find((o) => o.name === el.dataset.outEye);
      const all = out.operations.every((id) => isHidden(id));
      return setEyes((h) => out.operations.forEach((id) => (all ? h.delete(eyeKey(id)) : h.add(eyeKey(id)))));
    }
    const row = el.closest(".tree-op");
    if (!row) return;
    const id = row.dataset.op;
    if (el.dataset?.act === "eye") {
      // Alt-click: this one only
      if (ev.altKey) return setEyes((h) => setupOps().forEach((o) => (o === id ? h.delete(eyeKey(o)) : h.add(eyeKey(o)))));
      return setEyes((h) => (h.has(eyeKey(id)) ? h.delete(eyeKey(id)) : h.add(eyeKey(id))));
    }
    const s = setup();
    const out = s.outputs.find((o) => o.operations.includes(id));
    const act = el.dataset.act;
    if (act === "select" && (ev.ctrlKey || ev.metaKey)) {
      // (NC exports) Ctrl-click: several operations picked (+ New in NC exports takes them)
      t.multi ||= new Set(t.selected ? [t.selected] : []);
      if (t.multi.has(id)) t.multi.delete(id);
      else t.multi.add(id);
      t.selected = null;
      t.focus = null;
      return render();
    }
    if (act === "select") {
      t.multi = null;
      t.focus = null;
      if (t.picking) stopPicking();
      // another operation: an unsaved new one or a pending type change is dropped
      t.draft = null;
      t.retype = null;
      t.typeNote = null;
      t.selected = t.selected === id ? null : id;
      render();
      // (Alt+Up / Down move the selected operation: the keys go to the tree, so the focus is
      // put there, where the click left it on the page)
      const tr = document.getElementById("tree");
      if (t.selected && tr && !tr.contains(document.activeElement)) tr.focus({ preventScroll: true });
    } else if (act === "dup") {
      await duplicateOp(id);
    } else if (act === "delete") {
      if (t.selected === id && t.picking) endPicking(false);
      if (t.retype?.op.id === id) t.retype = null;
      s.operations = s.operations.filter((o) => o.id !== id);
      for (const o of s.outputs) o.operations = o.operations.filter((x) => x !== id);
      s.outputs = s.outputs.filter((o) => o.operations.length);
      for (const e of s.exports || []) e.operations = e.operations.filter((x) => x !== id);
      if (t.selected === id) t.selected = null;
      await commit({ label: `Deleted ${id}.` });
    }
  });
  // Escape: the operation deselected (picking ends with it)
  document.addEventListener("keydown", (ev) => {
    if (ev.key !== "Escape" || ev.ctrlKey || ev.metaKey || ev.altKey) return;
    const tg = ev.target;
    if (tg && (tg.isContentEditable || /^(INPUT|TEXTAREA|SELECT)$/.test(tg.tagName))) return;
    if (document.querySelector("dialog[open]:not(.embedded)")) return;
    if (!t.selected) return;
    ev.preventDefault();
    deselect();
  });
  // (Mar) Ctrl+D: the selected operation duplicated (not while typing, not with a dialog up)
  document.addEventListener("keydown", (ev) => {
    if (!(ev.key === "d" || ev.key === "D") || !(ev.ctrlKey || ev.metaKey) || ev.altKey || ev.shiftKey) return;
    const tg = ev.target;
    if (tg && (tg.isContentEditable || /^(INPUT|TEXTAREA|SELECT)$/.test(tg.tagName))) return;
    if (document.querySelector("dialog[open]:not(.embedded)")) return;
    if (!t.doc || !t.selected || !setup()?.operations.some((o) => o.id === t.selected)) return;
    ev.preventDefault();
    duplicateOp(t.selected);
  });
  document.addEventListener("keydown", (ev) => {
    if (ev.key !== "Delete" || ev.ctrlKey || ev.metaKey || ev.altKey) return;
    const tg = ev.target;
    if (tg && (tg.isContentEditable || /^(INPUT|TEXTAREA|SELECT)$/.test(tg.tagName))) return;
    if (document.querySelector("dialog[open]:not(.embedded)")) return;
    if (!t.doc || t.selected || (t.focus && t.focus !== "setup")) return;
    ev.preventDefault();
    deleteSetup(t.setup);
  });

  // ------------------------------------------------------------ reordering
  /**
   * Move operation `id` before (or after) operation `target`, in the target's file (a file of
   * the same setup); its old file goes if it is left empty. A real edit (the order is the
   * G-code's), undoable.
   */
  /** (Mar) Operation `id` of the setup worked on duplicated: right after it in its file (or,
   *  `into` another setup, last there; its picks looked for on that setup's part, flagged
   *  when not found), a new id, "<name> copy", everything else the same; selected, its form
   *  open; one edit. */
  async function duplicateOp(id, into) {
    const s = setup();
    const op = s?.operations.find((o) => o.id === id);
    if (!op) return;
    if (t.picking) endPicking(false);
    const dest = into ? t.doc.setups.find((x) => x.id === into) : s;
    if (!dest) return;
    const copy = structuredClone(op);
    copy.id = newOpId(dest);
    copy.name = `${op.name || typeOf(op.type)?.title || op.type} copy`;
    delete copy.error;
    if (dest === s) {
      s.operations.splice(s.operations.indexOf(op) + 1, 0, copy);
      const file = s.outputs.find((o) => o.operations.includes(id));
      if (file) file.operations.splice(file.operations.indexOf(id) + 1, 0, copy.id);
      else s.outputs.push({ name: `${s.id}_${copy.id}`, title: copy.name, operations: [copy.id] });
    } else {
      dest.operations.push(copy);
      const file = dest.outputs[dest.outputs.length - 1];
      if (file) file.operations.push(copy.id);
      else dest.outputs.push({ name: `${dest.id}_${copy.id}`, title: copy.name, operations: [copy.id] });
      // its picks on that setup's part (as a new model's are found again)
      if (copy.picks?.length) {
        try {
          const { result } = await engine("resolvePicks", t.doc, { setup: dest.id, model: partModel(dest), index_a: copy.index_a ?? 0, signatures: copy.picks }, state.mesh);
          const lost = result.picks.filter((p) => p.error).map((p) => p.label || "a pick");
          if (lost.length) copy.error = `not on this setup's part: ${lost.join(", ")}`;
        } catch (e) {
          copy.error = `its picks could not be looked for here: ${e.message}`;
        }
      }
      switchSetup(dest.id);
    }
    t.draft = null;
    t.retype = null;
    t.focus = null;
    t.selected = copy.id;
    await commit({ label: `Duplicated ${id} as ${copy.id}${dest === s ? "" : ` in ${dest.name || dest.id}`}.` });
  }

  async function moveOp(id, target, after) {
    if (id === target) return;
    const s = setup();
    const before = JSON.stringify(t.doc, null, 2);
    const from = s.outputs.find((o) => o.operations.includes(id));
    const to = s.outputs.find((o) => o.operations.includes(target));
    if (!from || !to) return;
    from.operations = from.operations.filter((x) => x !== id);
    const k = to.operations.indexOf(target) + (after ? 1 : 0);
    to.operations.splice(k, 0, id);
    s.outputs = s.outputs.filter((o) => o.operations.length);
    window.__marcam.project?.keepUndo?.(`Moved ${id}.`, before);
    await commit();
  }
  /** Alt+Up / Alt+Down: the selected operation one place up or down (across its setup's files). */
  async function nudge(id, dir) {
    const order = setup().outputs.flatMap((o) => o.operations);
    const k = order.indexOf(id);
    const j = k + dir;
    if (k < 0 || j < 0 || j >= order.length) return;
    await moveOp(id, order[j], dir > 0);
  }
  const tree = $("tree");
  let dragging = null;
  const clearMarks = () => tree.querySelectorAll(".drop-before, .drop-after").forEach((r) => r.classList.remove("drop-before", "drop-after"));
  tree.addEventListener("dragstart", (ev) => {
    const row = ev.target.closest?.(".tree-op");
    if (!row) return;
    dragging = row.dataset.op;
    ev.dataTransfer.effectAllowed = "copyMove";
    ev.dataTransfer.setData("text/plain", dragging);
    row.classList.add("dragging");
  });
  tree.addEventListener("dragover", (ev) => {
    const row = ev.target.closest?.(".tree-op");
    // (Ctrl over another setup's line: a copy goes there)
    if (dragging && !row && ev.ctrlKey && ev.target.closest?.(".tree-setuphead")) return ev.preventDefault();
    if (!dragging || !row) return;
    ev.preventDefault();
    const r = row.getBoundingClientRect();
    clearMarks();
    // the drop line: above or below the row under the pointer
    row.classList.add(ev.clientY < r.top + r.height / 2 ? "drop-before" : "drop-after");
  });
  tree.addEventListener("dragleave", (ev) => {
    if (!tree.contains(ev.relatedTarget)) clearMarks();
  });
  tree.addEventListener("drop", async (ev) => {
    const row = ev.target.closest?.(".tree-op");
    const id = dragging || ev.dataTransfer.getData("text/plain");
    clearMarks();
    tree.querySelectorAll(".dragging").forEach((r) => r.classList.remove("dragging"));
    dragging = null;
    // (Ctrl: a copy, into the setup dropped on: on one of its operations, or its line)
    const into = (row || ev.target.closest?.(".tree-setup"))?.closest(".tree-setup")?.dataset.setup;
    if (!row && id && ev.ctrlKey && into && into !== t.setup) {
      ev.preventDefault();
      return duplicateOp(id, into);
    }
    if (!row || !id) return;
    ev.preventDefault();
    if (ev.ctrlKey && into && into !== t.setup) return duplicateOp(id, into);
    const r = row.getBoundingClientRect();
    await moveOp(id, row.dataset.op, ev.clientY >= r.top + r.height / 2);
  });
  tree.addEventListener("dragend", () => {
    clearMarks();
    tree.querySelectorAll(".dragging").forEach((r) => r.classList.remove("dragging"));
    dragging = null;
  });
  tree.addEventListener("keydown", (ev) => {
    if (!ev.altKey || !(ev.key === "ArrowUp" || ev.key === "ArrowDown") || !t.selected) return;
    if (!setup()?.operations.some((o) => o.id === t.selected)) return;
    ev.preventDefault();
    nudge(t.selected, ev.key === "ArrowUp" ? -1 : 1);
  });

  $("tree").addEventListener("mouseover", (ev) => {
    const row = ev.target.closest?.(".geom-row");
    hoverUse(row ? Number(row.dataset.geom) : null);
  });

  // fields the user typed into keep their values when defaults are refreshed
  $("tree").addEventListener("input", (ev) => {
    const f = ev.target.closest("#opForm");
    if (f && ev.target.name && t.edited) t.edited.add(ev.target.name);
  });

  // what a type takes from picks: cleared (with a note) when the new type has no such field
  const PICKED = ["points", "paths", "path", "rect", "x_range", "edge", "body_back", "body_front", "diameter", "index_a"];
  /**
   * The operation being edited becomes a `kind`: the form, the defaults and the picker follow
   * the new type's registry entry. Values the two types share (same name, kind and unit) are
   * kept when the person set them (an applied operation's are all theirs); the tool if it fits;
   * picked faces if the new type takes faces. What it cannot take is cleared, with a note.
   */
  async function changeType(kind) {
    const cur = readForm();
    const from = cur.type;
    // (indexed roughing: 3D roughing, adaptive, at six angles, as the Add list makes it)
    const indexed = kind === INDEXED_ROUGH;
    if (indexed) kind = "rough3d";
    if (kind === from) return;
    const was = typeOf(from), ty = typeOf(kind);
    if (!ty) return;
    if (t.picking) endPicking(false);
    const isDraft = !!(t.draft && t.draft.op.id === cur.id);
    const notes = [];
    // the tool: kept if its shape suits the new type
    const tool = (t.doc.tools || []).find((x) => x.number === cur.tool);
    const toolKind = tool && toolInfo(tool).kind;
    const shapeOk = !toolKind || !ty.tool_shapes?.length || ty.tool_shapes.includes(toolKind);
    // picked faces: kept if the new type bounds itself by faces
    const facesMode = pickModeOf(ty)?.faces;
    const geometry = (cur.geometry || []).filter((g) => (g.kind === "faces" ? facesMode : g.kind === "axial_range" ? !!pickModeOf(ty)?.legacy : true));
    if ((cur.geometry || []).some((g) => g.kind === "faces") && !facesMode) notes.push(`the picked faces were cleared (${ty.title} does not take faces)`);
    let result;
    const gen = t.gen;
    try {
      ({ result } = await engine("opDefaults", t.doc, { type: kind, setup: t.setup, tool: shapeOk ? cur.tool : undefined, geometry, machine: machineArg?.() ?? null }, state.mesh));
      // the project replaced, or another operation selected, meanwhile: nothing to change
      if (gen !== t.gen || t.selected !== cur.id) return;
    } catch (e) {
      t.typeNote = { id: cur.id, text: `Cannot change to ${ty.title}: ${e.message}` };
      return renderEditor();
    }
    if (!shapeOk) notes.push(`tool ${cur.tool} (${toolKind}) does not suit ${ty.title}: T${result.tool} instead`);
    const next = { id: cur.id, ...result, type: kind, ...(indexed ? { pattern: "adaptive", index_count: 6, indexes: [] } : {}) };
    for (const k of ["name", "color", "enabled"]) if (cur[k] !== undefined) next[k] = cur[k];
    if (geometry.length) next.geometry = geometry;
    else delete next.geometry;
    const kept = new Set();
    for (const p of ty.params) {
      if (p.name === "tool" || cur[p.name] === undefined) continue;
      const q = was?.params.find((x) => x.name === p.name);
      if (!q || q.kind !== p.kind || (q.unit || "") !== (p.unit || "")) continue;
      // a draft's untouched fields were the old type's defaults: the new type's are better
      if (isDraft && !t.edited?.has(p.name)) continue;
      next[p.name] = cur[p.name];
      kept.add(p.name);
    }
    const lost = PICKED.filter((k) => cur[k] !== undefined && !ty.params.some((p) => p.name === k) && k !== "index_a");
    if (lost.length) notes.push(`cleared what ${ty.title} cannot take: ${lost.join(", ")}`);
    t.typeNote = { id: cur.id, text: `Now ${ty.title}.${kept.size ? ` Kept: ${[...kept].join(", ")}.` : ""}${notes.length ? ` ${notes.join("; ")}.` : ""}` };
    if (isDraft) {
      t.draft.op = next;
      t.draft.title = ty.title;
      t.edited = new Set([...(t.edited || [])].filter((k) => kept.has(k)));
    } else {
      const applied = setup().operations.find((o) => o.id === cur.id);
      // back to the type it has: no pending change
      t.retype = applied?.type === kind ? null : { op: next, from: t.retype?.from || from };
      t.edited = t.retype ? new Set(kept) : null;
    }
    renderEditor();
    // (applied at once: its note stays on the screen, what was kept and what was cleared)
    await applyForm({ keepNote: true });
  }

  /**
   * An applied operation's tool changed in its editor: the values that were the old tool's
   * defaults (stepover, depths, feeds ... derived from the tool) become the new tool's; the
   * values the person set stay. Applied with Apply, like any edit.
   */
  async function retool(tool) {
    const cur = readForm();
    const old = opById(t.selected);
    if (!old || old.tool === tool) return;
    const ask = (n) => engine("opDefaults", t.doc, { type: cur.type, setup: t.setup, tool: n, geometry: cur.geometry || [], machine: machineArg?.() ?? null }, state.mesh).then((r) => r.result);
    try {
      const [was, now] = await Promise.all([ask(old.tool), ask(tool)]);
      const next = { ...cur, tool };
      const same = (a, b) => JSON.stringify(a) === JSON.stringify(b);
      for (const [k, v] of Object.entries(now)) if (k !== "tool" && k in cur && same(cur[k], was[k])) next[k] = v;
      t.retype = { op: next, from: t.retype?.from || cur.type };
      t.typeNote = { id: cur.id, text: `Now T${tool}: the values that came from the old tool follow it; the ones you set stay.` };
      renderEditor();
      await applyForm();
    } catch (e) {
      showError(`Tool T${tool}: ${e.message}`);
    }
  }

  /** Re-derive the draft's defaults (tool or geometry changed), keeping edited fields. */
  async function refreshDefaults() {
    if (!t.draft) return;
    const cur = readForm();
    const gen = t.gen, draft = t.draft;
    try {
      const { result } = await engine(
        "opDefaults",
        t.doc,
        { type: cur.type, setup: t.setup, tool: cur.tool, geometry: cur.geometry || [], machine: machineArg?.() ?? null },
        state.mesh,
      );
      if (gen !== t.gen || draft !== t.draft) return;
      const next = { ...cur };
      for (const [k, v] of Object.entries(result)) if (!t.edited.has(k) && k !== "tool") next[k] = v;
      t.draft.op = next;
      renderEditor();
    } catch (e) {
      const box = $("opErrors");
      if (box) {
        box.textContent = e.message;
        box.classList.remove("hidden");
      }
    }
  }
  state.tree.refreshDefaults = refreshDefaults;

  $("tree").addEventListener("change", async (ev) => {
    const el = ev.target;
    // (NC exports) an operation ticked or not, the name
    if (el.dataset?.expOp || el.id === "expName") {
      const s = setup();
      const e = s?.exports?.find((x) => x.name === t.export);
      if (!e) return;
      if (el.dataset.expOp) {
        const on = new Set(e.operations);
        if (el.checked) on.add(el.dataset.expOp);
        else on.delete(el.dataset.expOp);
        e.operations = opsInOrder(s).map((o) => o.id).filter((id) => on.has(id));
        return commit({ label: `${el.checked ? "Added" : "Took"} ${el.dataset.expOp} ${el.checked ? "to" : "out of"} ${e.name}.nc.` });
      }
      const name = el.value.trim().replace(/[.]nc$/i, "").replace(/[\\/:*?"<>|]/g, "_");
      if (!name || name === e.name) return render();
      if (s.exports.some((x) => x.name === name)) {
        showError(`There is already an NC export named ${name} in this setup.`);
        return render();
      }
      showError("");
      moveChosen(expKey(s, e.name), expKey(s, name));
      e.name = name;
      t.export = name;
      return commit({ label: `Renamed the NC export to ${name}.` });
    }
    // the operation type picked, kept for this setup (see setTree)
    if (el.id === "treeAddType") {
      (t.addTypes ||= {})[t.setup] = el.value;
      return;
    }
    // (Mar) the Stock and Part panes: each field an edit (undoable), planned again
    if (el.id === "stSource") {
      const s = setup();
      if (!s) return;
      await window.__marcam.project.setSource(s.id, el.value);
      return;
    }
    if (el.closest?.("#fixturePane")) {
      const s = setup();
      const label = s && applyFixtureField(t.doc, s, el.id, el.value);
      if (label) await commit({ label });
      return;
    }
    if (el.closest?.("#stockPane") || el.closest?.("#partPane")) {
      const s = setup();
      if (!s) return;
      if (el.id === "stMargin") {
        t.stMargin = +el.value || 0;
        return;
      }
      let box = null;
      if (el.id === "stType" && s.stock?.shape === "from_previous") {
        try {
          box = (await engine("setupFrame", t.doc, { setup: s.id })).result.stock_box;
        } catch {}
      }
      // (the stick-out: the piece's length less what the chuck holds)
      if (el.id === "stStickOut") {
        try {
          box = (await engine("setupFrame", t.doc, { setup: s.id })).result.stock;
        } catch {}
      }
      const label = el.closest("#stockPane") ? applyStockField(t.doc, s, el.id, el.value, box) : applyPartField(t.doc, s, el.id, el.value);
      if (label && el.closest("#stockPane")) staleFitWarning();
      if (label) await commit({ label });
      return;
    }
    // an SVG file for a paths field: it replaces the current outline, placed at its corner
    // (else the stock's corner), scaled to the height asked for
    if (el.dataset?.svg && el.files?.[0] && t.selected) {
      const f = $("opForm");
      try {
        const text = await el.files[0].text();
        const op = readForm();
        const s = setup();
        const cur = (op[el.dataset.svg] || []).flat();
        const at = cur.length
          ? [Math.min(...cur.map((q) => q[0])), Math.min(...cur.map((q) => q[1]))]
          : s?.stock?.shape === "box" ? [s.stock.min[0] + 5, s.stock.min[1] + 5] : [op.text_at?.[0] ?? -20, -5];
        const { result } = await engine("svgPaths", text, { at, height: Number(f.elements.__svg_height?.value) || 0 });
        f.elements[el.dataset.svg].value = result.paths.map(pointsText).join(" | ");
        if (t.edited) t.edited.add(el.dataset.svg);
      } catch (e) {
        showError(`SVG: ${e.message}`);
      }
      return;
    }
    // a G-code file for a custom toolpath: its text in the paste area, applied; its T word
    // choosing the tool when the project has it
    if (el.dataset?.gcode && el.files?.[0] && t.selected) {
      const f = $("opForm");
      try {
        const text = await el.files[0].text();
        f.elements[el.dataset.gcode].value = text;
        if (t.edited) t.edited.add(el.dataset.gcode);
        const m = /(?:^|[^A-Z(;])T(\d+)/i.exec(text.replace(/\([^)]*\)|;.*$/gm, ""));
        const tl = m && (t.doc.tools || []).find((x) => x.number === +m[1]);
        if (tl && f.elements.tool) f.elements.tool.value = String(tl.number);
        await applyForm();
      } catch (e) {
        showError(`G-code: ${e.message}`);
      }
      return;
    }
    if (el.closest("#opForm") && el.name === "__type") return changeType(el.value);
    if (el.closest("#opForm") && el.name === "tool" && el.value === "__add") {
      // the library browser; the tool it adds becomes this operation's
      const op = opById(t.selected);
      const ty = op && typeOf(op.type);
      const n = await window.__marcam.tools?.pick(ty?.tool_shapes);
      await refresh();
      if (!n) return renderEditor();
      if (t.draft && t.draft.op.id === t.selected) {
        t.draft.op.tool = n;
        t.edited?.delete("tool");
        renderEditor();
        return refreshDefaults();
      }
      return retool(n);
    }
    if (el.closest("#opForm") && el.name === "tool" && t.draft && t.draft.op.id === t.selected) {
      t.edited.delete("tool");
      return refreshDefaults();
    }
    if (el.closest("#opForm") && el.name === "tool") return retool(parseInt(el.value, 10));
    if (el.closest("#opForm") && el.name === "__axial" && t.draft) {
      await refreshDefaults();
      return applyForm();
    }
    if (el.closest("#opForm") && el.name && !el.dataset?.svg && el.type !== "file") {
      clearTimeout(opTimer);
      return applyForm({ typing: el.tagName === "INPUT" && el.type !== "checkbox" && el.type !== "radio", left: true });
    }
    if (el.id === "treeSetup") {
      // another setup: what was being edited here is dropped
      switchSetup(el.value);
      return render();
    }
    const row = el.closest(".tree-op");
    if (!row) return;
    // the project's operation, not a pending edit of it (whose copy follows)
    const op = setup().operations.find((o) => o.id === row.dataset.op);
    if (!op) return;
    const pend = t.retype?.op.id === op.id ? t.retype.op : null;
    if (el.dataset.act === "enable") {
      op.enabled = el.checked;
      if (pend) pend.enabled = el.checked;
    }
    if (el.dataset.act === "color") {
      // a colour is for the eye only: saved, but the toolpaths stay as they are
      op.color = el.value;
      if (pend) pend.color = el.value;
      await commit({ display: true, label: `Changed the colour of ${op.id}.` });
      // (Mar: "the toolpaths don't always update"; a colour drew only after a hide and show)
      // the paths drawn again in it, in the still view and the playback
      return window.__marcam.opColorsChanged?.();
    }
    await commit({ label: el.dataset.act === "enable" ? `${el.checked ? "Enabled" : "Disabled"} ${op.id}.` : `Edited ${op.id}.` });
  });
  // a field of the operation being added: its preview again
  $("tree").addEventListener("input", (ev) => {
    if (t.draft && ev.target.closest?.("#opForm")) previewDraft();
  });
  // (Mar) the Stock and Part panes apply as they are typed in (the stock drawn again at once,
  // planned a moment later as any edit is),
  // as the other forms do; the field typed in keeps the focus
  let paneTimer = null;
  $("tree").addEventListener("input", (ev) => {
    const el = ev.target;
    if (!(el.closest?.("#stockPane") || el.closest?.("#partPane") || el.closest?.("#fixturePane")) || el.tagName !== "INPUT" || el.id === "stMargin") return;
    clearTimeout(paneTimer);
    paneTimer = setTimeout(async () => {
      const s = setup();
      if (!s || !el.isConnected || el.value === "" || !Number.isFinite(+el.value)) return;
      const id = el.id;
      const label = el.closest("#fixturePane") ? applyFixtureField(t.doc, s, id, el.value) : el.closest("#stockPane") ? applyStockField(t.doc, s, id, el.value, null) : applyPartField(t.doc, s, id, el.value);
      if (!label) return;
      // (a burst of typing one edit: Undo takes it back whole)
      await commit({ label, typing: true });
    }, 150);
  });
  // (Mar: no Apply) the operation's form applies as it is typed in, half a second after the
  // typing stops (a burst one Undo step); the toolpath planned again in the background
  let opTimer = null;
  $("tree").addEventListener("input", (ev) => {
    const el = ev.target;
    if (!el.closest?.("#opForm") || !el.name || el.tagName === "SELECT" || el.type === "checkbox" || el.type === "radio" || el.type === "file") return;
    clearTimeout(opTimer);
    opTimer = setTimeout(() => {
      if (el.isConnected) applyForm({ typing: true });
    }, 500);
  });
  // (the operation form's More: kept open or shut as the person left it)
  $("tree").addEventListener("toggle", (ev) => {
    if (ev.target.id === "opMore") t.moreOpen = ev.target.open;
    if (ev.target.id === "opAdvanced") t.advOpen = ev.target.open;
    if (ev.target.id === "stMore") t.stMore = ev.target.open;
    if (ev.target.id === "ptMore") t.ptMore = ev.target.open;
  }, true);
  $("tree").addEventListener("submit", async (ev) => {
    // (the operation's form; the setup's form in the details is the setup dialog's own)
    if (ev.target.id !== "opForm") return;
    ev.preventDefault();
    clearTimeout(opTimer);
    await applyForm();
  });
  /** The operation's form written to the project (one edit; a typing burst one Undo step),
   *  when the engine takes it; else the reason shown at the form (at the field it names) and
   *  the last good plan kept. One at a time: a change made meanwhile applied after. */
  let applying = null, applyAgain = null;
  async function applyForm(opts = {}) {
    if (applying) {
      applyAgain = { ...(applyAgain || {}), ...opts };
      return applying;
    }
    applying = applyOnce(opts);
    try {
      return await applying;
    } finally {
      applying = null;
      if (applyAgain) {
        const o = applyAgain;
        applyAgain = null;
        await applyForm(o);
      }
    }
  }
  async function applyOnce(opts) {
    if (!$("opForm") || !t.selected || !opById(t.selected)) return false;
    let op;
    try {
      op = readForm();
    } catch (e) {
      // (still being typed: nothing planned, nothing said, the last plan kept; said when the
      // field is left so)
      if (e.halfTyped && opts.typing && !opts.left) return showFormError(null);
      return showFormError(e.message, opts);
    }
    const doc = structuredClone(t.doc);
    const s = doc.setups.find((x) => x.id === t.setup);
    if (!s) return false;
    const isNew = !!(t.draft && t.draft.op.id === op.id);
    if (isNew && unpicked(op)) {
      showFormError(null);
      return false;
    }
    if (!isNew) {
      const was = s.operations.find((o) => o.id === op.id);
      // (a number the form shows at its default, not written in the project: left unwritten)
      for (const p of typeOf(op.type)?.params || []) if (was && was[p.name] === undefined && op[p.name] === p.default) delete op[p.name];
      // nothing changed: nothing to write (nor a pending type change to drop)
      if (was && JSON.stringify(was) === JSON.stringify(op) && !t.retype) {
        showFormError(null);
        return true;
      }
    }
    if (isNew) {
      s.operations.push(op);
      let outName = t.draft.out;
      if (outName === "__new") {
        outName = `${s.id}_${op.id}`;
        s.outputs.push({ name: outName, title: t.draft.title, operations: [] });
      }
      let file = s.outputs.find((o) => o.name === outName);
      if (!file) s.outputs.push((file = { name: `${s.id}_${op.id}`, title: t.draft.title, operations: [] }));
      file.operations.push(op.id);
    } else {
      const i = s.operations.findIndex((o) => o.id === op.id);
      const strip = (o) => JSON.stringify({ ...o, name: undefined, color: undefined });
      t.displayOnly = strip(s.operations[i]) === strip(op);
      s.operations[i] = op;
    }
    // the engine parses and checks the result before it replaces the project
    try {
      const { result } = await engine("checkProject", doc);
      if (result.problems.length) throw new Error(result.problems.join("; "));
      t.doc = result.project;
      t.draft = null;
      t.retype = null;
      if (!opts.keepNote) t.typeNote = null;
      if (t.picking && !opts.keepPicking && t.pick?.opId !== op.id) endPicking(false);
      const display = !!t.displayOnly;
      t.displayOnly = false;
      showFormError(null);
      await commit({ display, typing: !isNew && !!opts.typing, label: `${isNew ? "Added" : "Edited"} ${op.id}.` });
      return true;
    } catch (e) {
      t.displayOnly = false;
      return showFormError(e.message.replace(/^project 0\.2: /, ""), opts);
    }
  }
  /** The reason a form's values are refused, shown under the form and at the field it names
   *  (`msg` null: none). */
  function showFormError(msg, opts = {}) {
    const f = $("opForm");
    for (const x of f?.querySelectorAll(".invalid") || []) x.classList.remove("invalid");
    for (const x of f?.querySelectorAll(".field-error") || []) x.remove();
    const box = $("opErrors");
    if (!box) return false;
    if (!msg || opts.quiet) {
      box.textContent = "";
      box.classList.add("hidden");
      return false;
    }
    box.textContent = msg;
    box.classList.remove("hidden");
    const named = [...(f?.elements || [])].find((x) => x.name && !x.name.startsWith("__") && new RegExp(`\\b${x.name}\\b`).test(msg));
    if (named) {
      named.classList.add("invalid");
      const note = document.createElement("div");
      note.className = "field-error error small";
      note.textContent = msg;
      (named.closest("label") || named).after(note);
    }
    return false;
  }
  state.tree.applyForm = applyForm;
  // clicks on the 3D view pick faces while picking; a drag (orbit, pan) is not a pick
  const host = $("view3d");
  let down = null;
  host.addEventListener("pointerdown", (ev) => {
    down = [ev.clientX, ev.clientY];
  });
  host.addEventListener("pointermove", (ev) => {
    if (!t.picking || ev.buttons) return;
    const r = state.three.renderer.domElement.getBoundingClientRect();
    hoverAt(ev.clientX - r.left, ev.clientY - r.top);
  });
  host.addEventListener("click", (ev) => {
    if (!t.picking && !t.tabMode) return;
    if (down && Math.hypot(ev.clientX - down[0], ev.clientY - down[1]) > 4) return;
    const r = state.three.renderer.domElement.getBoundingClientRect();
    if (t.tabMode) return tabAt(ev.clientX - r.left, ev.clientY - r.top);
    // (off the part: empty space, the operation deselected)
    if (!regionAt(ev.clientX - r.left, ev.clientY - r.top)) {
      if (ev.target === state.three.renderer.domElement) deselect();
      return;
    }
    pickAt(ev.clientX - r.left, ev.clientY - r.top);
  });

  /** A closed outline's length, and the fraction of the way round it nearest `q` (and how
   *  far `q` is from it). */
  function outlineFraction(path, q) {
    const n = path.length;
    let total = 0, best = Infinity, at = 0;
    for (let i = 0; i < n; i++) {
      const a = path[i], b = path[(i + 1) % n];
      const dx = b[0] - a[0], dy = b[1] - a[1], l2 = dx * dx + dy * dy, l = Math.sqrt(l2);
      const u = l2 > 0 ? Math.max(0, Math.min(1, ((q[0] - a[0]) * dx + (q[1] - a[1]) * dy) / l2)) : 0;
      const d = Math.hypot(a[0] + dx * u - q[0], a[1] + dy * u - q[1]);
      if (d < best) {
        best = d;
        at = total + u * l;
      }
      total += l;
    }
    return { f: total > 0 ? at / total : 0, total, dist: best };
  }

  /** Tabs placed by hand: the click's point on the cut's top (a horizontal plane) to the
   *  nearest place on the operation's outline; a tab there within 3 mm is removed, else one is
   *  added (kept as a fraction of the way round the outline as written). */
  async function tabAt(px, py) {
    const v = state.three;
    const op = setup()?.operations.find((o) => o.id === t.selected);
    if (!v || v.failed || op?.type !== "contour") return null;
    const path = op.regions?.length ? op.regions[0].path : op.path;
    if (!path || path.length < 3) return null;
    const el = v.renderer.domElement;
    const ndc = new v.THREE.Vector2((px / el.clientWidth) * 2 - 1, -(py / el.clientHeight) * 2 + 1);
    const ray = new v.THREE.Raycaster();
    ray.setFromCamera(ndc, v.camera);
    const sb = state.job?.stock_box || (setup()?.stock?.shape === "box" ? setup().stock : null);
    const z = op.top ?? sb?.max?.[2] ?? 0;
    const hit = new v.THREE.Vector3();
    if (!ray.ray.intersectPlane(new v.THREE.Plane(new v.THREE.Vector3(0, 0, 1), -z), hit)) return null;
    const { f, total, dist } = outlineFraction(path, [hit.x, hit.y]);
    // too far from the outline: not a tab
    if (dist > Math.max(5, 2 * (op.tab_width || 4))) {
      t.tabNote = "not on the outline";
      renderEditor();
      return { f: null };
    }
    const tabs = [...(op.tab_at || [])];
    const near = tabs.findIndex((g) => Math.min(Math.abs(g - f), 1 - Math.abs(g - f)) * total < 3);
    if (near >= 0) tabs.splice(near, 1);
    else tabs.push(Math.round(f * 1e5) / 1e5);
    if (tabs.length) op.tab_at = tabs.sort((a, b) => a - b);
    else delete op.tab_at;
    await commit({ label: `Changed the tabs of ${op.id}.` });
    return { f, removed: near >= 0, tabs: op.tab_at || [] };
  }

  /** A new project: stop picking without writing anything, drop the face regions and the
   *  pick highlight, and forget the selection and any unsaved new operation. */
  function reset() {
    dropOverlay();
    const v = state.three;
    if (t.pickMesh) {
      v?.scene?.remove(t.pickMesh);
      t.pickMesh.geometry.dispose();
      t.pickMesh.material.dispose();
      t.pickMesh = null;
    }
    if (v && !v.failed) window.__marcam.legacyLayers?.();
    Object.assign(t, { doc: null, problems: [], setup: null, selected: null, picking: false, pickedChanged: false, regions: null, pick: null, draft: null, retype: null, typeNote: null, edited: null, upgraded: false, gen: t.gen + 1 });
    detected = { key: null, list: [] };
    setTree("");
  }

  /** Each operation's badge: planned, waiting to be planned again, being planned, or failed
   *  (from the app's opState; in place, nothing else redrawn). */
  const STATE_BADGES = {
    planned: ["✓", "Planned: the toolpath and time shown are this operation's as it is"],
    pending: ["•", "Changed: it is planned again in a moment"],
    planning: ["", "Being planned"],
    error: ["⚠", "Not planned: see the message"],
  };
  function renderStates() {
    if (!t.doc) return;
    const s = setup();
    const of = window.__marcam?.opState;
    if (!s || !of) return;
    // (Mar: "a real progress wheel") being planned: a ring filled as far as the plan has got
    // (operation i of n, and angle k of m in indexed roughing); turning where that is unknown
    const pr = window.__marcam?.planProgress;
    for (const el of document.querySelectorAll("#tree .op-state")) {
      const st = of(s.id, el.dataset.stateFor);
      const [mark, title] = STATE_BADGES[st] || ["", ""];
      if (st === "planning") {
        const known = pr && pr.f != null;
        el.classList.toggle("spin", !known);
        el.style.setProperty("--p", known ? pr.f.toFixed(3) : "0.25");
        const label = known ? `${title}: ${Math.round(pr.f * 100)}%` : title;
        if (el.title !== label) {
          el.title = label;
          el.setAttribute("aria-label", label);
        }
      }
      if (el.dataset.state === st) continue;
      el.dataset.state = st;
      el.className = `op-state ${st}${st === "planning" && !(pr && pr.f != null) ? " spin" : ""}`;
      el.textContent = mark;
      if (st !== "planning") {
        el.title = title;
        el.setAttribute("aria-label", title);
      }
    }
  }

  /** An edit from outside the tree (the 3D view's handles): `fn` changes the project and
   *  says what it did (the Undo label), or returns null for nothing. */
  async function edit(fn) {
    if (!t.doc) return;
    const label = fn(t.doc);
    if (label) await commit({ label });
  }
  return { refresh, pickAt, tabAt, pickSimilar, reset, renderEyes, renderStates, changeType, moveOp, nudge, edit, state: t };
}
