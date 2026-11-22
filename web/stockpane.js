// The setup's Stock and Part in the tree (Mar: "a place to type in the stock dimensions and
// click on the type of stock ... a text pane in the setup tree"): the stock's type in a
// dropdown (a plate, a bar, or what an earlier setup left), that type's sizes, a bar's axis
// in a vise, Fit to part; the rest folded under More. Where the part sits in the stock (and
// a carried piece in the machine) is moved with the 3D view's handles (see handles.js) or
// typed under More.

import { fitBar, kindOf, lyingBar, parseStl, placePart, stockSpecOf, UP_AXIS } from "./project.js";

const TYPES = [
  ["block", "Plate"],
  ["round", "Round bar"],
  ["square", "Square bar"],
  ["hex", "Hex bar"],
  ["rect", "Rect bar"],
];
const r3 = (x) => Math.round(x * 1000) / 1000;
const num = (v) => (v === "" || v == null || !Number.isFinite(+v) ? null : +v);

/** The setup whose stock `s` takes (carried), or null. */
export function carriedFrom(doc, s) {
  if (s.stock?.shape !== "from_previous") return null;
  const i = doc.setups.indexOf(s);
  return doc.setups.find((x) => x.id === s.stock.from) || doc.setups[i - 1] || null;
}

/** The mesh model a setup places (its own), or null. */
export function meshModelOf(doc, s) {
  return doc.models.find((m) => s.part?.includes(m.id) && m.kind === "mesh") || null;
}

/** The Stock pane's HTML for setup `s`. */
export function stockPane(doc, s, esc, opts = {}) {
  const kind = kindOf(s);
  const vise = kind === "vise";
  const st = s.stock || {};
  const from = carriedFrom(doc, s);
  const at = doc.setups.indexOf(s);
  const spec = from ? null : stockSpecOf(st);
  const cur = from ? `cut:${from.id}` : spec?.shape;
  const opts1 = TYPES.filter(([k]) => vise || k !== "block").map(([k, n]) => `<option value="${k}" ${k === cur ? "selected" : ""}>${n}</option>`);
  for (const x of doc.setups.slice(0, at)) opts1.push(`<option value="cut:${esc(x.id)}" ${`cut:${x.id}` === cur ? "selected" : ""}>Cut result of ${esc(x.name || x.id)}</option>`);
  const f = (id, label, v, title = "") => `<label ${title ? `title="${esc(title)}"` : ""}>${label} <input id="${id}" type="text" inputmode="decimal" value="${v == null ? "" : r3(v)}"> <span class="u">mm</span></label>`;
  let html = `<div id="stockPane" class="stock-pane" data-setup="${esc(s.id)}"><div class="tree-outname">Stock <span class="muted">${esc(s.name || s.id)}</span></div>
    <label>Stock <select id="stType" aria-label="The stock's type">${opts1.join("")}</select></label>`;
  if (from) {
    html += `<div class="muted small">What ${esc(from.name || from.id)} left, cut, as a mesh: move and turn it in the ${vise ? "vise" : "chuck"} with the handles in the 3D view (its part goes with it).</div>`;
  } else {
    const d = spec?.dims || {};
    if (spec?.shape === "block") {
      html += f("stX", "X", d.x) + f("stY", "Y", d.y) + f("stZ", "Z (height)", d.z);
    } else {
      if (spec?.shape === "round") html += f("stDia", "Diameter Ø", d.radius != null ? 2 * d.radius : null);
      if (spec?.shape === "square") html += f("stSide", "Side", d.side);
      if (spec?.shape === "hex") html += f("stAf", "Across flats", d.across_flats);
      if (spec?.shape === "rect") html += f("stW", "Width", d.width) + f("stH", "Height", d.height);
      html += f("stLen", "Length", spec?.length);
      if (spec?.shape !== "round") html += `<label title="Turned about its own axis (0: a flat on top)">Turned <input id="stAngle" type="text" inputmode="decimal" value="${r3(d.angle || 0)}"> <span class="u">°</span></label>`;
      if (vise) {
        const al = s.holding?.along || "x";
        html += `<label title="Which way the bar lies in the vise (Z: standing up)">Axis <select id="stAlong">${["x", "y", "z"].map((a) => `<option value="${a}" ${a === al ? "selected" : ""}>${a.toUpperCase()}${a === "z" ? " (standing)" : ""}</option>`).join("")}</select></label>`;
      }
    }
    if (opts.canFit) html += `<div class="row small"><button type="button" id="stFit" title="The smallest stock of this type round the part, with the margin (More)">Fit to part</button></div>`;
  }
  // the rest, folded
  const more = [];
  if (!from) more.push(`<label title="Fit to part leaves this much round the part">Margin <input id="stMargin" type="text" inputmode="decimal" value="${r3(opts.margin ?? (vise ? 2 : 0.5))}"> <span class="u">mm</span></label>`);
  if (vise) more.push(`<label title="The depth the vise's jaws grip, up from the stock's bottom (empty: the default)">Jaw grip <input id="stJaws" type="text" inputmode="decimal" value="${s.holding?.jaws ?? ""}" placeholder="default"> <span class="u">mm</span></label>`);
  else if (!from) more.push(`<label title="The bar's length held in the chuck">In the chuck <input id="stChuck" type="text" inputmode="decimal" value="${s.holding?.chuck ?? ""}" placeholder="0"> <span class="u">mm</span></label>`);
  if (from) {
    const rot = st.rotate || [0, 0, 0], off = st.offset || [0, 0, 0];
    more.push(`<div class="small muted">Turned (degrees about X, then Y, then Z), then moved:</div>`);
    more.push(["X", "Y", "Z"].map((a, i) => `<label>${a} <input id="stRot${a}" type="text" inputmode="decimal" value="${r3(rot[i])}"> <span class="u">°</span></label>`).join(""));
    more.push(`<label>Turned over <select id="stTurn">${[["none", "no"], ["y180", "left to right (Y)"], ["x180", "front to back (X)"], ["z180", "end for end (Z)"]].map(([v, n]) => `<option value="${v}" ${(st.turn || "none") === v ? "selected" : ""}>${n}</option>`).join("")}</select></label>`);
    more.push(["X", "Y", "Z"].map((a, i) => `<label>${a} <input id="stOff${a}" type="text" inputmode="decimal" value="${r3(off[i])}"> <span class="u">mm</span></label>`).join(""));
  }
  html += `<details class="op-more" id="stMore" ${opts.moreOpen ? "open" : ""}><summary>More</summary>${more.join("")}</details>`;
  return html + `</div>`;
}

/** The Part pane: its model, and where it sits (typed, or the handles in the 3D view). */
export function partPane(doc, s, esc, opts = {}) {
  const m = meshModelOf(doc, s);
  const from = carriedFrom(doc, s);
  let html = `<div id="partPane" class="stock-pane" data-setup="${esc(s.id)}"><div class="tree-outname">Part <span class="muted">${esc(m?.file || (s.part?.length ? "revolved profile" : "no model"))}</span></div>`;
  // (Mar) its model: its own, another setup's, none, or a new file (Replace model)
  if (!from) {
    const own = m ? `<option value="own" selected>its own: ${esc(m.file)}</option>` : s.part?.length ? `<option value="own" selected>its revolved profile</option>` : "";
    const others = (opts.choices || []).map((c) => `<option value="${esc(c.value)}">${esc(c.label)}</option>`).join("");
    html += `<label>Model <select id="ptModel" aria-label="This setup's part">${own}${others}<option value="none" ${s.part?.length ? "" : "selected"}>no model</option></select></label>
      <div class="row small"><button type="button" id="ptReplace" title="A new STL or STEP for the part: its placement, the setups and the operations stay; picked features are found again on it">Replace model...</button>
      ${m ? '<button type="button" id="ptInch" title="The file was drawn in inches: the part made 25.4 times bigger, its stock fitted again (Undo takes it back)">File in inches (x25.4)</button>' : ""}</div>`;
  }
  // (Mar: "a way in 4th to center up a part"): on the rotary axis, placed in one click
  const presets = kindOf(s) !== "vise" ? `<div class="row small part-presets" role="group" aria-label="Place the part on the rotary axis">
      <button type="button" data-preset="axis" title="The part's middle (across X) onto the rotary axis">Center on axis</button>
      <button type="button" data-preset="along" title="The part halfway along the bar">Center along stock</button>
      <button type="button" data-preset="front" title="The part's front at the bar's front face (X0)">Flush to front</button>
      <button type="button" data-preset="back" title="The part's back at the bar's back end">Flush to back</button></div>` : "";
  if (from) return html + `<div class="muted small">It goes with the stock ${esc(from.name || from.id)} left: move and turn them together with the Stock's handles.</div>${presets}</div>`;
  if (!m) return html + `<div class="muted small">${s.part?.length ? "A revolved profile stays on the rotary axis." : "This setup has no model: its operations work on the stock."}</div></div>`;
  const t = m.translate || [0, 0, 0], r = m.rotate || [0, 0, 0];
  const rotary = kindOf(s) !== "vise";
  html += `<div class="muted small">Move it in the stock with the handles in the 3D view (Shift: free angles).${rotary ? " On the rotary axis: turned about X; moved along X, or across it off the axis." : ""}</div>${presets}`;
  html += `<details class="op-more" id="ptMore" ${opts.moreOpen ? "open" : ""}><summary>More</summary>
    <div class="small muted">Moved (mm):</div>${["X", "Y", "Z"].map((a, i) => `<label>${a} <input id="ptT${a}" type="text" inputmode="decimal" value="${r3(t[i])}"></label>`).join("")}
    <div class="small muted">Turned (degrees about X, then Y, then Z):</div>${["X", "Y", "Z"].map((a, i) => `<label>${a} <input id="ptR${a}" type="text" inputmode="decimal" value="${r3(r[i])}"></label>`).join("")}</details>`;
  return html + `</div>`;
}

/** The stock a type change starts from: its sizes kept as near as they go. */
function stockOfType(type, st, box) {
  const sp = stockSpecOf(st) || {};
  const d = sp.dims || {};
  // the old stock's sizes: a block's, a bar's (its length, its width), or the carried box's
  const size = box ? [box.max[0] - box.min[0], box.max[1] - box.min[1], box.max[2] - box.min[2]] : sp.shape === "block" ? [d.x, d.y, d.z] : null;
  const len = sp.length ?? (size ? size[0] : 50);
  const across = size ? Math.min(size[1], size[2]) : d.radius ? 2 * d.radius : d.side ?? d.across_flats ?? d.width ?? 20;
  switch (type) {
    case "block": {
      const [x, y, z] = size ?? [len, across, across];
      return { shape: "box", min: [0, 0, -r3(z)], max: [r3(x), r3(y), 0] };
    }
    case "round": return { shape: "cylinder", radius: r3(across / 2), length: r3(len) };
    case "square": return { shape: "square", side: r3(across), length: r3(len), angle: 0 };
    case "hex": return { shape: "hex", across_flats: r3(across), length: r3(len), angle: 0 };
    default: return { shape: "rect", width: r3(size ? size[1] : across), height: r3(size ? size[2] : across), length: r3(len), angle: 0 };
  }
}

/**
 * A field of the Stock pane changed (`id`): setup `s` of `doc` changed to match (in place).
 * `frameBox`: the setup's stock box as the engine has it (a type change from carried stock
 * starts from its size). Returns a label for the edit, or null when nothing changed.
 */
export function applyStockField(doc, s, id, val, frameBox) {
  const st = s.stock;
  const v = num(val);
  const set = (k, x) => {
    if (x == null || x <= 0) return false;
    st[k] = r3(x);
    return true;
  };
  if (id === "stType") {
    // (its kind as it was: a vise's block made a bar stays in the vise)
    if (!s.kind) s.kind = kindOf(s);
    if (val.startsWith("cut:")) {
      const src = val.slice(4);
      const at = doc.setups.indexOf(s);
      s.stock = { shape: "from_previous" };
      if (doc.setups[at - 1]?.id !== src) s.stock.from = src;
      return `Setup "${s.name || s.id}": its stock is what ${src} left.`;
    }
    s.stock = stockOfType(val, st, st?.shape === "from_previous" ? frameBox : st?.shape === "box" ? { min: st.min, max: st.max } : null);
    if (val === "block") delete s.holding?.along;
    return `Setup "${s.name || s.id}": the stock a ${TYPES.find(([k]) => k === val)?.[1].toLowerCase()}.`;
  }
  const what = `Setup "${s.name || s.id}": the stock's sizes.`;
  if (st?.shape === "box") {
    // (its low corner and its top stay where they are)
    const k = { stX: 0, stY: 1, stZ: 2 }[id];
    if (k == null || v == null || v <= 0) return null;
    if (k === 2) st.min[2] = r3(st.max[2] - v);
    else {
      // (the walkthrough: 34 typed down to 30 left the part 2 mm off the block's middle, and
      // off the 4th axis when carried there; the part moves half the change, with the block)
      const grow = v - (st.max[k] - st.min[k]);
      st.max[k] = r3(st.min[k] + v);
      for (const m of doc.models || []) {
        if (!s.part?.includes(m.id) || !m.translate) continue;
        m.translate[k] = r3(m.translate[k] + grow / 2);
      }
    }
    return what;
  }
  const ok = {
    stDia: () => set("radius", v / 2),
    stSide: () => set("side", v),
    stAf: () => set("across_flats", v),
    stW: () => set("width", v),
    stH: () => set("height", v),
    stLen: () => set("length", v),
    stAngle: () => v != null && ((st.angle = r3(v)), true),
  }[id];
  if (ok) return ok() ? what : null;
  if (id === "stAlong") {
    s.holding ||= { kind: "vise" };
    if (val === "x") delete s.holding.along;
    else s.holding.along = val;
    return `Setup "${s.name || s.id}": the bar along ${val.toUpperCase()}.`;
  }
  if (id === "stJaws") {
    s.holding ||= { kind: "vise" };
    if (v == null) delete s.holding.jaws;
    else s.holding.jaws = r3(v);
    return `Setup "${s.name || s.id}": the jaws' grip.`;
  }
  if (id === "stChuck") {
    s.holding ||= { kind: "rotary_chuck" };
    if (v == null || v <= 0) delete s.holding.chuck;
    else s.holding.chuck = r3(v);
    return `Setup "${s.name || s.id}": the bar held in the chuck.`;
  }
  const m3 = /^st(Rot|Off)([XYZ])$/.exec(id);
  if (m3 && st?.shape === "from_previous") {
    const key = m3[1] === "Rot" ? "rotate" : "offset";
    const a = st[key] ? [...st[key]] : [0, 0, 0];
    a["XYZ".indexOf(m3[2])] = r3(v ?? 0);
    if (a.some((x) => x)) st[key] = a;
    else delete st[key];
    return `Setup "${s.name || s.id}": the stock ${key === "rotate" ? "turned" : "moved"}.`;
  }
  if (id === "stTurn" && st?.shape === "from_previous") {
    if (val === "none") delete st.turn;
    else st.turn = val;
    return `Setup "${s.name || s.id}": the stock turned over.`;
  }
  return null;
}

/** A Part pane field (`ptTX` ..., `ptRZ`): the setup's model moved or turned. */
export function applyPartField(doc, s, id, val) {
  if (id === "ptModel") {
    if (val === "own") return null;
    if (val === "none") {
      s.part = [];
      const used = new Set(doc.setups.flatMap((x) => x.part || []));
      doc.models = doc.models.filter((x) => used.has(x.id));
      return `Setup "${s.name || s.id}": no model.`;
    }
    const o = doc.setups.find((x) => `setup:${x.id}` === val);
    const om = o && meshModelOf(doc, o);
    if (!om) return null;
    // its own copy of that model (placed as it is there)
    const id2 = `${s.id}_part`;
    doc.models = doc.models.filter((x) => x.id !== id2).concat({ ...structuredClone(om), id: id2 });
    s.part = [id2];
    return `Setup "${s.name || s.id}": the part as ${o.name || o.id}'s.`;
  }
  const m = meshModelOf(doc, s);
  const v = num(val) ?? 0;
  const k = /^pt([TR])([XYZ])$/.exec(id);
  if (!m || !k) return null;
  const key = k[1] === "T" ? "translate" : "rotate";
  const a = m[key] ? [...m[key]] : [0, 0, 0];
  a["XYZ".indexOf(k[2])] = r3(v);
  if (key === "rotate" && !a.some((x) => x)) delete m.rotate;
  else m[key] = a;
  return `Setup "${s.name || s.id}": the part ${key === "rotate" ? "turned" : "moved"}.`;
}

/** Fit to part: the smallest stock of the setup's type round its part (`tris`, mm), `margin`
 *  round it; the part placed in it. */
export function fitStock(doc, s, tris, margin) {
  const m = meshModelOf(doc, s);
  if (!m || !tris) return null;
  const kind = kindOf(s);
  const vise = kind === "vise";
  const sp = stockSpecOf(s.stock) || { shape: vise ? "block" : "round" };
  const axis = vise ? Object.entries(UP_AXIS).find(([, v]) => v === (m.axis ?? "x"))?.[0] ?? "z" : m.axis ?? "x";
  const spec = { shape: sp.shape, mode: "fit", margin, dims: { angle: sp.dims?.angle ?? 0 }, ...(vise && sp.shape === "block" ? { margins: { xl: margin, xh: margin, yl: margin, yh: margin } } : {}), ...(s.holding?.chuck ? { chuck: s.holding.chuck } : {}) };
  const place = placePart(tris, { kind, axis, up: axis, margin, stock: spec, rotate: m.rotate, along: s.holding?.along });
  s.stock = place.stock;
  m.axis = place.axis;
  m.translate = place.translate.map(r3);
  return `Setup "${s.name || s.id}": the stock fitted to the part.`;
}

export { fitBar, lyingBar, parseStl };
