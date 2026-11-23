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
      <div class="row small"><button type="button" id="ptReplace" title="${m ? "A new STL or STEP for the part: its placement, the setups and the operations stay; picked features are found again on it" : "An STL or STEP for this setup's part: placed on it, its stock fitted round it"}">${m ? "Replace model..." : "Import model..."}</button>
      ${m ? '<button type="button" id="ptInch" title="The file was drawn in inches: the part made 25.4 times bigger, its stock fitted again (Undo takes it back)">File in inches (x25.4)</button>' : ""}</div>`;
  }
  // (Mar: "a way in 4th to center up a part"): on the rotary axis, placed in one click
  // (Mar: preset buttons) in the vise: centred or flush in the block, and quarter turns
  const presets = kindOf(s) === "vise" ? `<div class="row small part-presets" role="group" aria-label="Place the part in the block">
      <span class="muted">Position</span>
      <button type="button" data-preset="cx" title="The part's middle at the block's middle in X">Center X</button>
      <button type="button" data-preset="cy" title="The part's middle at the block's middle in Y">Center Y</button>
      <button type="button" data-preset="cxy" title="The part's middle at the block's middle in X and Y">Center XY</button>
      <button type="button" data-preset="top" title="The part's top at the block's top (Z0)">Top flush</button>
      <button type="button" data-preset="bottom" title="The part's bottom at the block's bottom">Bottom flush</button>
      <button type="button" data-preset="cz" title="The part's middle at the block's middle in Z">Center Z</button></div>` + `<div class="row small part-presets" role="group" aria-label="Turn the part a quarter">
      <span class="muted">Rotation</span>
      ${["X", "Y", "Z"].map((a) => `<button type="button" data-preset="turn" data-axis="${a}" data-deg="-90" title="A quarter turn back about ${a}">${a} −90°</button><button type="button" data-preset="turn" data-axis="${a}" data-deg="90" title="A quarter turn about ${a}">${a} +90°</button>`).join("")}
      <button type="button" data-preset="unturn" title="Not turned at all">0°</button></div>` : kindOf(s) !== "vise" ? `<div class="row small part-presets" role="group" aria-label="Place the part on the rotary axis">
      <button type="button" data-preset="axis" title="The part's middle (across X) onto the rotary axis">Center on axis</button>
      <button type="button" data-preset="along" title="The part halfway along the bar">Center along stock</button>
      <button type="button" data-preset="front" title="The part's front at the bar's front face (X0)">Flush to front</button>
      <button type="button" data-preset="back" title="The part's back at the bar's back end">Flush to back</button></div>` : "";
  // (Mar: "missing the quick buttons for part rotation") on the 4th axis too: quarter turns,
  // the part kept on the axis with its front where it was
  const turns = `<div class="row small part-presets" role="group" aria-label="Turn the part a quarter">
      <span class="muted">Rotation</span>
      ${["X", "Y", "Z"].map((a) => `<button type="button" data-preset="turn" data-axis="${a}" data-deg="-90" title="A quarter turn back about ${a}">${a} −90°</button><button type="button" data-preset="turn" data-axis="${a}" data-deg="90" title="A quarter turn about ${a}">${a} +90°</button>`).join("")}
      <button type="button" data-preset="unturn" title="Not turned at all">0°</button></div>`;
  // (carried in the vise: the block is the piece itself; turned a quarter, its stock with it)
  const carriedPresets = kindOf(s) === "vise" ? `<div class="row small part-presets" role="group" aria-label="Turn the part a quarter">
      <span class="muted">Rotation</span>
      ${["X", "Y", "Z"].map((a) => `<button type="button" data-preset="turn" data-axis="${a}" data-deg="-90" title="A quarter turn back about ${a}">${a} −90°</button><button type="button" data-preset="turn" data-axis="${a}" data-deg="90" title="A quarter turn about ${a}">${a} +90°</button>`).join("")}
      <button type="button" data-preset="unturn" title="Not turned at all">0°</button></div>` : presets;
  if (from) return html + `<div class="muted small">It goes with the stock ${esc(from.name || from.id)} left: move and turn them together with the Stock's handles.</div>${carriedPresets}</div>`;
  if (!m) return html + `<div class="muted small">${s.part?.length ? "A revolved profile stays on the rotary axis." : "This setup has no model: its operations work on the stock."}</div></div>`;
  const t = m.translate || [0, 0, 0], r = m.rotate || [0, 0, 0];
  const rotary = kindOf(s) !== "vise";
  html += `<div class="muted small">Move it in the stock with the handles in the 3D view (its rings: 5° on their ticks, free inside).${rotary ? " On the rotary axis: turned about X; moved along X, or across it off the axis." : ""}</div>${presets}${rotary ? turns : ""}`;
  html += `<details class="op-more" id="ptMore" ${opts.moreOpen ? "open" : ""}><summary>More</summary>
    <div class="small muted">Moved (mm):</div>${["X", "Y", "Z"].map((a, i) => `<label>${a} <input id="ptT${a}" type="text" inputmode="decimal" value="${r3(t[i])}"></label>`).join("")}
    <div class="small muted">Turned (degrees about X, then Y, then Z):</div>${["X", "Y", "Z"].map((a, i) => `<label>${a} <input id="ptR${a}" type="text" inputmode="decimal" value="${r3(r[i])}"></label>`).join("")}</details>`;
  return html + `</div>`;
}

// ------------------------------------------------------------ the fixture (Mar)
/** What holds setup `s`: "vise", "chuck" or "chuck_tailstock". */
export function fixtureOf(s) {
  if (kindOf(s) === "vise") return "vise";
  return s.holding?.tailstock ? "chuck_tailstock" : "chuck";
}
export const FIXTURES = { vise: "Vise", chuck: "Chuck", chuck_tailstock: "Chuck + tailstock" };
/** The chuck's diameter when none is given (mm): the 4th axis's own. */
export const CHUCK_D = 65;

/** The Fixture's pane: its preset (those that fit the setup's type) and its sizes, each
 *  applied as it is typed. */
export function fixturePane(doc, s, esc) {
  const which = fixtureOf(s);
  const h = s.holding || {};
  const opts = (kindOf(s) === "vise" ? ["vise"] : ["chuck", "chuck_tailstock"]).map((k) => `<option value="${k}" ${k === which ? "selected" : ""}>${FIXTURES[k]}</option>`).join("");
  const f = (id, label, v, ph, title) => `<label title="${esc(title)}">${label} <input id="${id}" type="text" inputmode="decimal" value="${v ?? ""}" placeholder="${ph}"> <span class="u">mm</span></label>`;
  let html = `<div id="fixturePane" class="stock-pane" data-setup="${esc(s.id)}"><div class="tree-outname">Fixture <span class="muted">${esc(FIXTURES[which])}</span></div>
    <label>Fixture <select id="fxPreset" aria-label="What holds the stock">${opts}</select></label>`;
  if (which === "vise") {
    html += f("fxJawW", "Jaw width", h.jaw_width, "block + 20", "The jaws' length along X");
    html += f("fxJawH", "Jaw height", h.jaw_height, "to the grip", "The jaws' own height (drawn)");
    html += f("fxOpen", "Opening", h.opening, "the block's width", "Between the jaws (Y); empty: gripping the block");
    html += f("fxGrip", "Grip depth", h.jaws, "default", "How deep the jaws grip the stock, up from its bottom");
  } else {
    html += f("fxChuckD", "Chuck Ø", h.chuck_diameter, String(CHUCK_D), "The chuck's diameter: the tool is checked against it");
    html += f("fxChuckL", "Chuck length", h.chuck_length, "30", "The chuck's body behind the bar");
    html += f("fxGripLen", "Jaw grip", h.chuck, "0", "The bar's length held in the chuck's jaws, from its back end");
    if (which === "chuck_tailstock") {
      html += f("fxTailX", "Tailstock at X", h.tailstock_x, "0 (the bar's front)", "Where the tailstock's centre meets the bar");
      html += f("fxDepth", "Centre depth", h.centre_depth, "3", "How deep the tailstock's centre goes into the bar");
    }
  }
  return html + `<div class="muted small">${which === "vise" ? "Drawn in the view, and the tool" : "The tool"} checked against it.</div></div>`;
}

/** A Fixture pane field (`id`) changed: setup `s`'s holding to match; a label for Undo. */
export function applyFixtureField(doc, s, id, val) {
  s.holding ||= { kind: kindOf(s) === "vise" ? "vise" : "rotary_chuck", tailstock: false };
  const h = s.holding;
  const name = `Setup "${s.name || s.id}"`;
  if (id === "fxPreset") {
    if (val === "vise") Object.assign(h, { kind: "vise", tailstock: false });
    else {
      Object.assign(h, { kind: "rotary_chuck", tailstock: val === "chuck_tailstock" });
      // (picked: its chuck checked, at the usual size unless one is given)
      h.chuck_diameter ??= CHUCK_D;
    }
    return `${name}: held by ${FIXTURES[val] || val}.`;
  }
  const key = { fxJawW: "jaw_width", fxJawH: "jaw_height", fxOpen: "opening", fxGrip: "jaws", fxChuckD: "chuck_diameter", fxChuckL: "chuck_length", fxGripLen: "chuck", fxTailX: "tailstock_x", fxDepth: "centre_depth" }[id];
  if (!key) return null;
  const v = num(val);
  if (val === "" || v == null) delete h[key];
  else if (key !== "tailstock_x" && v < 0) return null;
  else h[key] = r3(v);
  return `${name}: the fixture's ${key.replace(/_/g, " ")}.`;
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
      // off the 4th axis when carried there) the block grows or shrinks about its middle: the
      // part stays where it is in it, and on the screen
      const grow = v - (st.max[k] - st.min[k]);
      st.min[k] = r3(st.min[k] - grow / 2);
      st.max[k] = r3(st.min[k] + v);
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

/** (the impeller) On the rotary axis the operations' X ranges go along with the part, moved
 *  `dx` along X (by the handles, a typed X, a placement): they stay on it. */
export function rangesFollow(s, dx) {
  if (!dx || kindOf(s) === "vise") return;
  for (const o of s.operations || []) if (Array.isArray(o.x_range) && o.x_range.length === 2) o.x_range = o.x_range.map((v) => r3(v + dx));
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
  if (key === "translate" && k[2] === "X") rangesFollow(s, r3(v) - a[0]);
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
