// The setup dialog: one place to choose what a setup is. Used for New project, Import STL,
// Edit setup and Add setup. Name, machine, the setup type (three big choices), the part (an
// STL, its units and its axis or up direction, the origin, a tailstock) with a placement
// preview, and the stock (from the part with a margin, or given).

const DEFAULT_MARGIN = { rotary: 0.5, indexed: 0.5, vise: 2 };
const DEFAULT_ORIGIN = { rotary: 0, indexed: 0, vise: 0.5 };

// common stock sizes, in mm (imperial ones given in inches)
const IN = 25.4;
const COMMON = {
  block: [3, 5, 6, 8, 10, 12, 15, 20, 25, 30].map((z) => ({ label: `plate ${z} mm thick`, z }))
    .concat([[1 / 8, "1/8"], [1 / 4, "1/4"], [3 / 8, "3/8"], [1 / 2, "1/2"], [3 / 4, "3/4"], [1, "1"]].map(([z, l]) => ({ label: `plate ${l}" thick`, z: z * IN }))),
  round: [6, 8, 10, 12, 16, 20, 22, 25, 30, 32, 35, 40, 45, 50].map((d) => ({ label: `Ø${d} mm`, d }))
    .concat([[1 / 4, "1/4"], [3 / 8, "3/8"], [1 / 2, "1/2"], [5 / 8, "5/8"], [3 / 4, "3/4"], [7 / 8, "7/8"], [1, "1"], [1.25, "1-1/4"], [1.5, "1-1/2"], [2, "2"]].map(([d, l]) => ({ label: `Ø${l}"`, d: d * IN }))),
  square: [8, 10, 12, 15, 16, 20, 25, 30, 40, 50].map((d) => ({ label: `${d} mm square`, d }))
    .concat([[1 / 4, "1/4"], [3 / 8, "3/8"], [1 / 2, "1/2"], [3 / 4, "3/4"], [1, "1"], [1.25, "1-1/4"], [1.5, "1-1/2"], [2, "2"]].map(([d, l]) => ({ label: `${l}" square`, d: d * IN }))),
  rect: [[20, 5], [20, 10], [25, 10], [30, 10], [30, 15], [40, 10], [40, 20], [50, 10], [50, 25]].map(([w, h]) => ({ label: `${w} x ${h} mm`, w, h }))
    .concat([[1, 1 / 4, '1 x 1/4"'], [1, 1 / 2, '1 x 1/2"'], [1.5, 1 / 2, '1-1/2 x 1/2"'], [2, 1 / 2, '2 x 1/2"'], [2, 1, '2 x 1"']].map(([w, h, l]) => ({ label: l, w: w * IN, h: h * IN }))),
  hex: [8, 10, 13, 17, 19, 22, 24, 27, 30, 32, 36, 41].map((d) => ({ label: `${d} mm across flats`, d }))
    .concat([[1 / 4, "1/4"], [5 / 16, "5/16"], [3 / 8, "3/8"], [1 / 2, "1/2"], [5 / 8, "5/8"], [3 / 4, "3/4"], [7 / 8, "7/8"], [1, "1"], [1.25, "1-1/4"]].map(([d, l]) => ({ label: `${l}" across flats`, d: d * IN }))),
};
const BAR_SHAPES = ["round", "square", "rect", "hex"];

export function setupDialog({ $, state, parseStl, readPart, partSummary, placePart, KINDS, stockSpecOf, lyingBar, flatFaces, layFlat, UP_AXIS, rotation }) {
  const dlg = $("setupDialog");
  let cur = null; // { mode, preset, parsed, resolve }

  const kind = () => dlg.querySelector('input[name="sdKind"]:checked')?.value ?? "rotary";
  const stockMode = () => dlg.querySelector('input[name="sdStock"]:checked')?.value ?? "auto";
  const num = (id) => ($(id).value === "" ? undefined : +$(id).value);
  const bar = (k) => k !== "vise";

  /** The part's triangles in mm, if there is a part (the chosen file, else the project's). */
  function tris() {
    // (another part chosen for this setup: that one)
    if (cur?.modelPick?.kind === "none") return null;
    if (cur?.modelPick?.tris) return cur.modelPick.tris;
    const t = cur?.parsed?.tris ?? cur?.preset?.tris ?? cur?.preset?.previewTris ?? null;
    if (!t) return null;
    return cur.parsed && $("sdUnits").value === "inch" ? t.map((v) => v * 25.4) : t;
  }

  // ---- the stock: its shape, fitted to the part or given, in mm or inches
  const shape = () => dlg.querySelector('input[name="sdShape"]:checked')?.value ?? (bar(kind()) ? "round" : "block");
  const unit = () => ($("sdStockUnits").value === "inch" ? IN : 1);
  const STOCK_FIELDS = ["sdJaws", "sdMargin", "sdMxl", "sdMxh", "sdMyl", "sdMyh", "sdMzb", "sdChuckFit", "sdDia", "sdSide", "sdW", "sdH", "sdAf", "sdAc", "sdLen", "sdChuck", "sdBx", "sdBy", "sdBz", "sdPx", "sdPy"];
  const mm = (id) => {
    const v = num(id);
    return v === undefined ? undefined : v * unit();
  };
  const setMm = (id, v) => ($(id).value = v == null || !Number.isFinite(v) ? "" : +(v / unit()).toFixed(4));

  /** What the form says of the stock (see placePart's `stock`), in mm. */
  function stockSpec() {
    const sh = shape();
    const angle = num("sdAngle") || 0;
    if (stockMode() !== "explicit") {
      return sh === "block"
        ? { shape: sh, mode: "fit", margins: { xl: mm("sdMxl"), xh: mm("sdMxh"), yl: mm("sdMyl"), yh: mm("sdMyh"), bottom: mm("sdMzb") } }
        : { shape: sh, mode: "fit", margin: mm("sdMargin"), chuck: mm("sdChuckFit"), dims: { angle } };
    }
    const dims = {
      block: { x: mm("sdBx"), y: mm("sdBy"), z: mm("sdBz") },
      round: { radius: mm("sdDia") / 2 },
      square: { side: mm("sdSide"), angle },
      rect: { width: mm("sdW"), height: mm("sdH"), angle },
      hex: { across_flats: mm("sdAf"), angle },
    }[sh];
    const spec = { shape: sh, mode: "given", dims };
    if (sh === "block") spec.at = [mm("sdPx"), mm("sdPy")];
    else {
      spec.length = mm("sdLen");
      spec.chuck = mm("sdChuck");
    }
    return spec;
  }
  /** The sizes a given stock needs, missing or not positive. */
  function missing(spec) {
    if (spec.mode !== "given") return [];
    const out = Object.entries(spec.dims).filter(([k, v]) => k !== "angle" && !(v > 0)).map(([k]) => k);
    if (spec.shape !== "block" && !(spec.length > 0)) out.push("length");
    return out;
  }

  function options() {
    return { kind: kind(), axis: $("sdAxis").value, up: $("sdAxis").value, margin: bar(kind()) ? mm("sdMargin") : undefined, stock: stockSpec(), origin: num("sdOrigin"), rotate: rotate() };
  }
  /** The part's orientation: degrees about X, Y and Z. */
  const rotate = () => ["sdRotX", "sdRotY", "sdRotZ"].map((id) => +$(id).value || 0);
  const ROT_IDS = ["sdRotX", "sdRotY", "sdRotZ"];
  // (in (-180, 180])
  const normDeg = (d) => {
    let v = ((d % 360) + 360) % 360;
    if (v > 180) v -= 360;
    return +v.toFixed(4);
  };
  const setRotate = (r) => ROT_IDS.forEach((id, i) => ($(id).value = String(normDeg(r[i]))));
  // a quarter turn about one axis, upside down, or laid on its next largest flat face
  for (const b of dlg.querySelectorAll("#sdOrient [data-rot]")) {
    b.addEventListener("click", () => {
      const r = rotate();
      r[+b.dataset.rot] += 90;
      setRotate(r);
      preview();
    });
  }
  $("sdFlip").addEventListener("click", () => {
    const r = rotate();
    r[0] += 180;
    setRotate(r);
    preview();
  });
  $("sdLayFlat").addEventListener("click", () => {
    const t = tris();
    if (!t) return;
    const ax = kind() === "vise" ? UP_AXIS[$("sdAxis").value] : $("sdAxis").value;
    const faces = flatFaces(t, ax);
    if (!faces.length) return;
    cur.flat = ((cur.flat ?? -1) + 1) % faces.length;
    setRotate(layFlat(faces[cur.flat]));
    preview();
  });

  const ORIENT_TITLE = $("sdOrient").title;

  /** A vise holds the part as a mesh, the stock a setup before left (Mar), or a block given
   *  for no model. */
  function viseGate() {
    if (!cur) return;
    const off = !!cur.noMesh && stockMode() !== "previous" && cur.modelPick?.kind !== "none";
    for (const el of dlg.querySelectorAll(".sd-kind")) {
      if (el.dataset.kind !== "vise") continue;
      el.classList.toggle("disabled", off);
      el.querySelector("input").disabled = off;
    }
  }

  /** The shapes the setup type takes (a vise: a block; the rotary axis: bars), the fields of
   *  the one chosen, the common sizes. */
  function stockLayout() {
    const k = kind();
    // (a vise holds a block, or a bar lying along X)
    const allowed = bar(k) ? BAR_SHAPES : ["block", ...BAR_SHAPES];
    for (const el of dlg.querySelectorAll(".sd-shape")) el.classList.toggle("hidden", !allowed.includes(el.dataset.shape));
    if (!allowed.includes(shape())) dlg.querySelector(`input[name="sdShape"][value="${allowed[0]}"]`).checked = true;
    const sh = shape();
    const given = stockMode() === "explicit";
    // from the previous setup: its stock turned, nothing to size
    const carried = stockMode() === "previous";
    $("sdCarry").classList.toggle("hidden", !carried);
    for (const id of ["sdMargins", "sdDims"]) $(id).classList.toggle("hidden", carried);
    for (const el of dlg.querySelectorAll(".sd-shape")) if (carried) el.classList.add("hidden");
    viseGate();
    // (Mar) turned any way: carried, the stock (and its part) turns, its cut shape as a mesh
    $("sdOrient").title = carried ? "How the stock from the setup before (and its part) is turned: about X, Y and Z, in that order; any angle (its cut shape goes on as a mesh)" : ORIENT_TITLE;
    for (const l of $("sdMargins").querySelectorAll("label")) {
      const f = l.dataset.for;
      // (a bar in a vise: no chuck)
      const chuck = l.querySelector("#sdChuckFit");
      l.classList.toggle("hidden", given || (f === "bar" ? sh === "block" || (chuck && !bar(k)) : sh !== "block"));
    }
    for (const l of $("sdDims").querySelectorAll("label")) {
      const fs = l.dataset.for.split(" ");
      const angle = l.querySelector("#sdAngle");
      const chuck = l.querySelector("#sdChuck");
      // (the angle turns a fitted bar too; a bar in a vise has no chuck)
      l.classList.toggle("hidden", !(fs.includes(sh) && (given || angle)) || (chuck && !bar(k)));
    }
    const list = COMMON[sh] || [];
    $("sdCommon").innerHTML = `<option value="">choose...</option>` + list.map((c, i) => `<option value="${i}">${c.label}</option>`).join("");
    for (const u of dlg.querySelectorAll("#sdStockSet .u")) u.textContent = $("sdStockUnits").value === "inch" ? "in" : "mm";
    // (the vise's jaws: how much they grip)
    $("sdJawsRow").classList.toggle("hidden", bar(k));
  }

  /** A common size chosen: its sizes given (a plate: its thickness; X and Y fitted). */
  function useCommon(i) {
    const c = COMMON[shape()]?.[i];
    if (!c) return;
    const sh = shape();
    if (sh === "block") {
      fitTo();
      setMm("sdBz", c.z);
    } else {
      if (!$("sdLen").value) fitTo();
      if (sh === "round") setMm("sdDia", c.d);
      if (sh === "square") setMm("sdSide", c.d);
      if (sh === "rect") {
        setMm("sdW", c.w);
        setMm("sdH", c.h);
      }
      if (sh === "hex") {
        setMm("sdAf", c.d);
        setMm("sdAc", (c.d * 2) / Math.sqrt(3));
      }
    }
    dlg.querySelector('input[name="sdStock"][value="explicit"]').checked = true;
    stockLayout();
    preview();
  }

  /** Fit to model: the sizes of the smallest stock of the shape round the part, given. */
  function fitTo() {
    const t = tris();
    if (!t) return;
    const o = options();
    const place = placePart(t, { ...o, stock: { ...stockSpec(), mode: "fit" } });
    const st = place.stock;
    if (st.shape === "box") {
      setMm("sdBx", st.max[0] - st.min[0]);
      setMm("sdBy", st.max[1] - st.min[1]);
      setMm("sdBz", st.max[2] - st.min[2]);
      // (where the fit put the part: its margins on the low sides)
      const mg = stockSpec().margins || {};
      setMm("sdPx", mg.xl ?? 2);
      setMm("sdPy", mg.yl ?? 2);
    } else {
      if (st.shape === "cylinder") setMm("sdDia", 2 * st.radius);
      if (st.shape === "square") setMm("sdSide", st.side);
      if (st.shape === "rect") {
        setMm("sdW", st.width);
        setMm("sdH", st.height);
      }
      if (st.shape === "hex") {
        setMm("sdAf", st.across_flats);
        setMm("sdAc", (st.across_flats * 2) / Math.sqrt(3));
      }
      setMm("sdLen", st.length);
      if (!$("sdChuck").value && place.chuck != null) setMm("sdChuck", place.chuck);
    }
    dlg.querySelector('input[name="sdStock"][value="explicit"]').checked = true;
    stockLayout();
    preview();
  }

  /** The stock's section and the part's outline, looking along X (the bar's axis, or the
   *  block from its front). */
  function endView(place, t) {
    const cv = $("sdEnd");
    if (!cv) return;
    const ctx = cv.getContext("2d");
    ctx.clearRect(0, 0, cv.width, cv.height);
    if (!place) return;
    const st = place.stock;
    const sh = stockSpecOf(st);
    const a = place.axis;
    const turned = rotation(rotate());
    const map = (p) => {
      let q = a === "y" ? [p[1], p[2], p[0]] : a === "z" ? [p[2], p[0], p[1]] : p;
      if (turned) q = turned(q);
      return [q[0] + place.translate[0], q[1] + place.translate[1], q[2] + place.translate[2]];
    };
    // the view box (Y across, Z up); a bar lying in a vise: at its axis, its top at Z0
    const off = st.shape !== "box" && st.min ? lyingBar(st).axis : [0, 0];
    let box;
    if (st.shape === "box" || st.min) box = [st.min[1], st.min[2], st.max[1], st.max[2]];
    else {
      const r = sh.shape === "round" ? st.radius : Math.max(...Array.from({ length: 72 }, (_, i) => sectionR(sh, (i * Math.PI) / 36)));
      box = [-r, -r, r, r];
    }
    const pad = 10;
    const s = Math.min((cv.width - 2 * pad) / (box[2] - box[0]), (cv.height - 2 * pad) / (box[3] - box[1]));
    const cx = (box[0] + box[2]) / 2, cz = (box[1] + box[3]) / 2;
    const X = (y) => cv.width / 2 + (y - cx) * s, Y = (z) => cv.height / 2 - (z - cz) * s;
    // the part's outline (its triangles, thinned)
    if (t) {
      ctx.strokeStyle = "rgba(147, 197, 253, 0.35)";
      ctx.beginPath();
      const step = Math.max(1, Math.floor(t.length / 9 / 4000));
      for (let i = 0; i < t.length; i += 9 * step) {
        const p = [0, 3, 6].map((o) => map([t[i + o], t[i + o + 1], t[i + o + 2]]));
        ctx.moveTo(X(p[0][1]), Y(p[0][2]));
        ctx.lineTo(X(p[1][1]), Y(p[1][2]));
        ctx.lineTo(X(p[2][1]), Y(p[2][2]));
        ctx.closePath();
      }
      ctx.stroke();
    }
    // the stock
    ctx.strokeStyle = "rgba(245, 158, 11, 0.95)";
    ctx.lineWidth = 1.5;
    ctx.beginPath();
    if (st.shape === "box") ctx.rect(X(st.min[1]), Y(st.max[2]), (st.max[1] - st.min[1]) * s, (st.max[2] - st.min[2]) * s);
    else {
      for (let i = 0; i <= 360; i++) {
        const phi = (i * Math.PI) / 180;
        const r = sectionR(sh, phi);
        const y = r * Math.cos(phi) + off[0], z = r * Math.sin(phi) + off[1];
        if (i) ctx.lineTo(X(y), Y(z));
        else ctx.moveTo(X(y), Y(z));
      }
    }
    ctx.stroke();
    ctx.lineWidth = 1;
  }
  /** A bar's radius at phi (as the engine's Section). */
  function sectionR(spec, phi) {
    const d = spec.dims;
    const a = ((d.angle || 0) * Math.PI) / 180;
    if (spec.shape === "round") return d.radius;
    if (spec.shape === "rect") {
      const p = phi - a;
      const c = Math.abs(Math.cos(p)), s = Math.abs(Math.sin(p));
      return Math.min(c > 1e-12 ? d.width / 2 / c : Infinity, s > 1e-12 ? d.height / 2 / s : Infinity);
    }
    const n = spec.shape === "square" ? 4 : 6;
    const af = spec.shape === "square" ? d.side : d.across_flats;
    const step = (2 * Math.PI) / n;
    let x = (((phi - Math.PI / 2 - a) % step) + step) % step;
    if (x > step / 2) x -= step;
    return af / 2 / Math.cos(x);
  }
  /** The bar's corner radius (what it needs to turn clear). */
  const cornerR = (spec) => Math.max(...Array.from({ length: 720 }, (_, i) => sectionR(spec, (i * Math.PI) / 360)));
  /** The stock in words. */
  function stockWords(st, chuck) {
    const sp = stockSpecOf(st);
    const f = (v) => +(+v).toFixed(3);
    const len = st.length != null ? ` x ${f(st.length)} mm long${chuck ? `, ${f(chuck)} held in the chuck: ${f(st.length - chuck)} sticks out` : ""}` : "";
    switch (sp.shape) {
      case "block": return `block ${f(sp.dims.x)} x ${f(sp.dims.y)} x ${f(sp.dims.z)} mm, Z0 at its top`;
      case "square": return `square bar ${f(sp.dims.side)} mm${sp.dims.angle ? ` turned ${f(sp.dims.angle)}°` : ""}${len}`;
      case "rect": return `rectangular bar ${f(sp.dims.width)} (Y) x ${f(sp.dims.height)} (Z) mm${sp.dims.angle ? ` turned ${f(sp.dims.angle)}°` : ""}${len}`;
      case "hex": return `hex bar ${f(sp.dims.across_flats)} mm across flats (${f((sp.dims.across_flats * 2) / Math.sqrt(3))} across corners)${sp.dims.angle ? ` turned ${f(sp.dims.angle)}°` : ""}${len}`;
      default: return `round bar Ø${f(2 * st.radius)} mm${len}`;
    }
  }

  /** Labels and fields that depend on the setup type. */
  function layout(fromKindChange) {
    // (a rotary setup's work zero: the bar's front on its axis)
    const vise = kind() === "vise";
    $("sdZeroRow").classList.toggle("hidden", !vise);
    $("sdZeroXYZ").classList.toggle("hidden", !vise);
    $("sdZeroNote").classList.toggle("hidden", vise);
    const k = kind();
    $("sdAxisLabel").textContent = bar(k) ? "Part axis along X" : "Up (towards the tool)";
    $("sdOriginLabel").textContent = bar(k) ? "Front at X0, then shift" : "Top below Z0 by";
    $("sdTailRow").classList.toggle("hidden", !bar(k));
    stockLayout();
    if (fromKindChange) {
      // the type's own defaults, unless the user typed something
      if (!$("sdMargin").dataset.edited) $("sdMargin").value = DEFAULT_MARGIN[k];
      if (!$("sdOrigin").dataset.edited) $("sdOrigin").value = DEFAULT_ORIGIN[k];
      if (!$("sdAxis").dataset.edited) $("sdAxis").value = bar(k) ? "x" : "z";
    }
    preview();
  }

  /** The part and stock as placed, seen from the side (X across; up: radius or Z). */
  function preview() {
    const cv = $("sdPreview");
    const ctx = cv.getContext("2d");
    ctx.clearRect(0, 0, cv.width, cv.height);
    const t = tris();
    const k = kind();
    $("sdWarn").classList.add("hidden");
    if (!t) {
      $("sdInfo").textContent = cur?.mode === "new" ? "No part yet: choose an STL now, or import one after." : "";
      return;
    }
    if (stockMode() === "previous") {
      $("sdInfo").textContent = "The stock the setup before leaves (simulated), and the part with it, turned and moved as set here.";
      return;
    }
    let place;
    try {
      place = placePart(t, options());
    } catch (e) {
      $("sdInfo").textContent = e.message;
      return;
    }
    const turned = rotation(rotate());
    const map = (p) => {
      const a = place.axis;
      let q = a === "y" ? [p[1], p[2], p[0]] : a === "z" ? [p[2], p[0], p[1]] : p;
      if (turned) q = turned(q);
      return [q[0] + place.translate[0], q[1] + place.translate[1], q[2] + place.translate[2]];
    };
    const st = place.stock;
    const R = bar(k) ? (st.radius ?? cornerR(stockSpecOf(st))) : 0;
    // (a bar lying in a vise: seen as the block round it)
    const lying = !bar(k) && st.shape !== "box" ? lyingBar(st).box : null;
    if (lying) Object.assign(st, { min: lying.min, max: lying.max });
    endView(place, t);
    // the view box: stock and part
    const box = bar(k) ? [-st.length, -R, Math.max(0, place.translate[0] + 1), R] : [st.min[0], st.min[2], st.max[0], st.max[2] + 1];
    const pad = 12;
    const sx = (cv.width - 2 * pad) / (box[2] - box[0]), sy = (cv.height - 2 * pad) / (box[3] - box[1]);
    const s = Math.min(sx, sy);
    const X = (x) => pad + (x - box[0]) * s, Y = (y) => cv.height - pad - (y - box[1]) * s;
    ctx.strokeStyle = "rgba(156, 163, 175, 0.9)";
    ctx.setLineDash([4, 3]);
    if (bar(k)) {
      ctx.strokeRect(X(-st.length), Y(R), st.length * s, 2 * R * s);
      // the length held in the chuck
      const ch = stockSpec().chuck ?? place.chuck;
      if (ch > 0) {
        ctx.fillStyle = "rgba(156, 163, 175, 0.25)";
        ctx.fillRect(X(-st.length), Y(R), Math.min(ch, st.length) * s, 2 * R * s);
      }
    }
    else ctx.strokeRect(X(st.min[0]), Y(st.max[2]), (st.max[0] - st.min[0]) * s, (st.max[2] - st.min[2]) * s);
    ctx.setLineDash([]);
    // the part's edges, side on (X across, Z up)
    ctx.strokeStyle = "rgba(147, 197, 253, 0.35)";
    ctx.beginPath();
    const step = Math.max(1, Math.floor(t.length / 9 / 6000));
    for (let i = 0; i < t.length; i += 9 * step) {
      const a = map([t[i], t[i + 1], t[i + 2]]), b = map([t[i + 3], t[i + 4], t[i + 5]]), c = map([t[i + 6], t[i + 7], t[i + 8]]);
      ctx.moveTo(X(a[0]), Y(a[2]));
      ctx.lineTo(X(b[0]), Y(b[2]));
      ctx.lineTo(X(c[0]), Y(c[2]));
      ctx.closePath();
    }
    ctx.stroke();
    // the origin
    ctx.strokeStyle = "#f59e0b";
    ctx.beginPath();
    if (bar(k)) {
      ctx.moveTo(X(box[0]), Y(0));
      ctx.lineTo(X(box[2]), Y(0));
      ctx.moveTo(X(0), Y(box[1]));
      ctx.lineTo(X(0), Y(box[3]));
    } else {
      ctx.moveTo(X(box[0]), Y(0));
      ctx.lineTo(X(box[2]), Y(0));
    }
    ctx.stroke();
    ctx.fillStyle = "#f59e0b";
    ctx.font = "11px system-ui";
    ctx.fillText(bar(k) ? "X0 (front), rotary axis" : "Z0 (top of the block)", X(bar(k) ? 0 : box[0]) + 4, Y(0) - 4);
    const size = place.size.map((v) => v.toFixed(2)).join(" x ");
    const words = stockWords(st, stockSpec().chuck ?? place.chuck);
    $("sdInfo").textContent = bar(k)
      ? `Part ${size} mm; ${words}, the part's front at X0${k === "indexed" ? "; cut from fixed A angles" : ""}.`
      : `Part ${size} mm; ${words}.`;
    if (bar(k) && stockMode() === "explicit" && st.shape === "cylinder" && st.radius < Math.max(...[place.size[1], place.size[2]]) / 2) {
      $("sdWarn").textContent = "The bar is thinner than the part.";
      $("sdWarn").classList.remove("hidden");
    }
  }

  async function chooseFile(file) {
    if (!file) return;
    try {
      $("sdFileName").textContent = `${file.name}: reading...`;
      cur.parsed = { ...(await readPart(new Uint8Array(await file.arrayBuffer()), file.name)), file };
      $("sdFileName").textContent = `${file.name}: ${partSummary(cur.parsed)}`;
    } catch (e) {
      cur.parsed = null;
      $("sdFileName").textContent = "";
      $("sdWarn").textContent = `${file.name}: ${e.message}`;
      $("sdWarn").classList.remove("hidden");
    }
    preview();
  }

  /**
   * Open the dialog. `mode`: "new" (project name, machine, type, an optional STL), "import"
   * (a parsed STL given), "edit" (a setup: type, machine, stock, origin, holding), "add" (a new
   * setup for the same part). Resolves to the configuration, or null when cancelled.
   */
  function open(mode, preset = {}, opts = {}) {
    cur?.resolve(null);
    // (Mar) a setup's settings in the details below the tree: the same form, in `opts.host`,
    // not modal; anywhere else (New, Import, Add setup, Edit setup...) a modal dialog
    const host = opts.host || null;
    if (host) {
      if (dlg.parentElement !== host) host.appendChild(dlg);
      dlg.classList.add("embedded");
    } else if (dlg.classList.contains("embedded")) {
      if (dlg.open) dlg.close();
      document.body.appendChild(dlg);
      dlg.classList.remove("embedded");
    }
    return new Promise((resolve) => {
      cur = { mode, preset, parsed: preset.parsed ? { ...preset.parsed, file: preset.file } : null, resolve, embedded: !!host };
      const titles = { new: "New project", import: `Import ${preset.file?.name ?? "STL"}`, edit: `Setup: ${preset.setupName ?? ""}`, add: "Add a setup" };
      $("sdTitle").textContent = titles[mode];
      $("sdOk").textContent = { new: "Create", import: "Import", edit: "Apply", add: "Add setup" }[mode];
      $("sdNameRow").firstChild.textContent = mode === "new" ? "Project name " : "Setup name ";
      $("sdName").value = mode === "new" ? preset.name ?? "untitled" : preset.setupName ?? "";
      $("sdMachine").innerHTML = (state.registry?.machines ?? []).map((m) => `<option value="${m.id}">${m.name}</option>`).join("");
      if (preset.machine) $("sdMachine").value = preset.machine;
      const k = preset.kind ?? "rotary";
      for (const r of dlg.querySelectorAll('input[name="sdKind"]')) r.checked = r.value === k;
      dlg.dataset.kind = k;
      // a vise needs the part as a mesh
      cur.noMesh = (mode === "edit" || mode === "add") && !preset.hasMesh;
      $("sdFileRow").classList.toggle("hidden", mode !== "new");
      // (Mar) this setup's part: its own, another setup's, or another file
      const choices = preset.modelChoices || [];
      $("sdModelRow").classList.toggle("hidden", !choices.length);
      $("sdModel").innerHTML = choices.map((c) => `<option value="${c.value}">${c.label}</option>`).join("");
      // (its own by default; no model when there is none)
      const first = preset.modelDefault ?? (choices.some((c) => c.value === "own") ? "own" : choices[0]?.value);
      $("sdModel").value = first ?? "";
      cur.modelPick = first === "none" ? { kind: "none" } : null;
      $("sdFile").value = "";
      $("sdFileName").textContent = cur.parsed ? `${preset.file.name}: ${partSummary(cur.parsed)}` : "";
      $("sdUnits").value = preset.units ?? "mm";
      $("sdUnits").disabled = !(mode === "new" || mode === "import");
      for (const id of ["sdMargin", "sdOrigin", "sdAxis"]) delete $(id).dataset.edited;
      $("sdMargin").value = preset.margin ?? DEFAULT_MARGIN[k];
      $("sdOrigin").value = preset.origin ?? DEFAULT_ORIGIN[k];
      // (the setup's own axis; a change of type resets it to that type's, unless set here)
      $("sdAxis").value = preset.axis ?? (bar(k) ? "x" : "z");
      $("sdTail").checked = !!preset.tailstock;
      ["sdRotX", "sdRotY", "sdRotZ"].forEach((id, i) => ($(id).value = String(preset.rotate?.[i] ?? 0)));
      cur.presetRotate = JSON.stringify(preset.rotate ?? [0, 0, 0]);
      $("sdMaterials").innerHTML = (state.registry?.materials ?? []).map((m) => `<option value="${m}">`).join("");
      $("sdMaterial").value = preset.material ?? "";
      const st = preset.stock;
      const ex = preset.explicit;
      $("sdStockUnits").value = "mm";
      for (const id of STOCK_FIELDS) {
        $(id).value = "";
        delete $(id).dataset.edited;
      }
      $("sdMargin").value = preset.margin ?? 0.5;
      $("sdAngle").value = 0;
      // the setup's stock: its shape and sizes (given), or (the older explicit sizes)
      const sp = preset.stockSpec ?? (st ? stockSpecOf(st) : ex ? (ex.bx ? { shape: "block", dims: { x: ex.bx, y: ex.by, z: ex.bz } } : { shape: "round", dims: { radius: ex.dia / 2 }, length: ex.len }) : null);
      const sh = sp?.shape ?? (bar(k) ? "round" : "block");
      for (const r of dlg.querySelectorAll('input[name="sdShape"]')) r.checked = r.value === sh;
      cur.presetShape = sh;
      dlg.querySelector(`input[name="sdStock"][value="${ex || preset.stockSpec?.mode === "given" ? "explicit" : "auto"}"]`).checked = true;
      // a setup after another: its stock may be what that one left
      $("sdCarryOpt").classList.toggle("hidden", !preset.canCarry);
      // (any setup before this one: the one just before by default)
      const sources = preset.carrySources ?? [];
      $("sdCarryFrom").innerHTML = sources.map((x) => `<option value="${x.id}">${x.name}</option>`).join("");
      const from = st?.shape === "from_previous" && st.from ? st.from : sources[sources.length - 1]?.id;
      if (from) $("sdCarryFrom").value = from;
      $("sdTurn").value = st?.shape === "from_previous" ? st.turn ?? "none" : "none";
      const off = st?.shape === "from_previous" ? st.offset ?? [0, 0, 0] : [0, 0, 0];
      ["sdOffX", "sdOffY", "sdOffZ"].forEach((id, i) => ($(id).value = off[i]));
      // (a setup added after others: it carries on from the last by default, Mar)
      if (preset.canCarry && (st?.shape === "from_previous" || (mode === "add" && sources.length))) dlg.querySelector('input[name="sdStock"][value="previous"]').checked = true;
      cur.presetCarry = st?.shape === "from_previous" ? JSON.stringify([st.turn ?? "none", off, from ?? null, (st.rotate ?? [0, 0, 0]).map((v) => +v || 0)]) : null;
      if (sp?.dims) {
        const d = sp.dims;
        setMm("sdDia", d.radius != null ? 2 * d.radius : null);
        setMm("sdSide", d.side);
        setMm("sdW", d.width);
        setMm("sdH", d.height);
        setMm("sdAf", d.across_flats);
        setMm("sdAc", d.across_flats != null ? (d.across_flats * 2) / Math.sqrt(3) : null);
        $("sdAngle").value = d.angle ?? 0;
        setMm("sdBx", d.x);
        setMm("sdBy", d.y);
        setMm("sdBz", d.z);
        setMm("sdLen", sp.length);
      }
      // the setup's G-code options
      const po = preset.post || {};
      $("sdWork").value = po.work_offset ?? "";
      // the work zero: a vise setup's, from its stock or part (their boxes from the caller)
      const z0 = po.origin ?? [0, 0, 0];
      ["sdZeroX", "sdZeroY", "sdZeroZ"].forEach((id, i) => ($(id).value = String(z0[i])));
      cur.boxes = { stock: preset.stockBox ?? null, part: preset.partBox ?? null };
      $("sdZeroAt").value = zeroPreset(z0) ?? "custom";
      $("sdLineNums").checked = !!po.line_numbers;
      $("sdNoComments").checked = !!po.no_comments;
      $("sdHeader").value = (po.header || []).join("\n");
      $("sdFooter").value = (po.footer || []).join("\n");
      $("sdPostBox").open = !!(po.work_offset || po.line_numbers || po.no_comments || po.header?.length || po.footer?.length);
      cur.presetPost = JSON.stringify(po);
      setMm("sdChuck", preset.chuck);
      setMm("sdJaws", preset.jaws);
      setMm("sdChuckFit", preset.chuck);
      $("sdWarn").classList.add("hidden");
      layout(false);
      // (Mar: no modal) `opts.auto`: the form's own defaults (and `opts.auto()`'s changes)
      // taken at once, never shown; what it refuses comes back as { error }
      if (opts.auto) {
        if (typeof opts.auto === "function") opts.auto();
        finish(true);
        if (cur) {
          const c = cur;
          cur = null;
          c.resolve({ error: $("sdWarn").textContent || "the setup could not be made" });
        }
        return;
      }
      if (!dlg.open) {
        if (host) dlg.show();
        else dlg.showModal();
      }
    });
  }

  /** No setup's details shown: the form, if it was there, closed (its edit dropped) and put
   *  back in the page (never left out of it: its fields are looked up by id). */
  function park() {
    if (!dlg.classList.contains("embedded") && dlg.isConnected) return;
    if (cur?.embedded) finish(false);
    if (dlg.open) dlg.close();
    dlg.classList.remove("embedded");
    document.body.appendChild(dlg);
  }

  /** Whether the form is open in the details for setup `id` (being edited there). */
  function embeddedFor(id) {
    return !!(cur?.embedded && cur.preset.setupId === id && dlg.open);
  }
  /** Put the open form back in `host` (the details drawn again): as it was, values and all. */
  function reattach(host, focus) {
    if (dlg.parentElement !== host) host.appendChild(dlg);
    if (focus) {
      const x = dlg.querySelector(`#${CSS.escape(focus.id)}`);
      if (x && document.activeElement !== x) {
        x.focus({ preventScroll: true });
        try {
          if (focus.s != null) x.setSelectionRange(focus.s, focus.e);
        } catch {}
      }
    }
  }

  function finish(ok) {
    if (!cur) return;
    const c = cur;
    if (ok) {
      const k = kind();
      const spec = stockSpec();
      const carry = stockMode() === "previous" ? { turn: $("sdTurn").value, offset: ["sdOffX", "sdOffY", "sdOffZ"].map((id) => num(id) || 0), from: $("sdCarryFrom").value || null, rotate: rotate() } : null;
      const noModel = c.modelPick?.kind === "none";
      // (Mar: a setup with no part made another type in its details: the usual plate, or bar,
      // of that type, its sizes then in its Stock)
      const usual = noModel && spec.mode !== "given" && c.embedded ? (k === "vise" ? { shape: "block", mode: "given", dims: { x: 60, y: 40, z: 20 } } : { shape: "round", mode: "given", dims: { radius: 12.5 }, length: 60 }) : null;
      // (a new project: no setup yet, no stock to give)
      const need = carry || usual || c.mode === "new" ? [] : noModel && spec.mode !== "given" ? ["no model to fit it to"] : missing(spec);
      if (need.length) {
        $("sdWarn").textContent = noModel && spec.mode !== "given" ? "With no model, give the stock's sizes, or take the stock from a setup before." : `Give every stock size (${need.join(", ")}), or fit the stock to the part.`;
        $("sdWarn").classList.remove("hidden");
        return;
      }
      let ex = spec.mode === "given" ? spec : usual || undefined;
      // fitted round a part that is not placed again (a revolved profile): the sizes fitted here
      if (!ex && !c.parsed?.tris && !c.preset.tris && c.preset.previewTris && bar(k)) {
        const place = placePart(c.preset.previewTris, options());
        ex = { ...stockSpecOf(place.stock), mode: "given", chuck: spec.chuck };
      }
      if (k === "vise" && !c.parsed && !c.preset.tris && !c.modelPick?.tris && !carry && !noModel && c.mode !== "new") {
        $("sdWarn").textContent = "A vise setup needs the part as a mesh: import an STL.";
        $("sdWarn").classList.remove("hidden");
        return;
      }
      // G-code options: only what is set
      const post = {};
      if ($("sdWork").value) post.work_offset = $("sdWork").value;
      const zero = ["sdZeroX", "sdZeroY", "sdZeroZ"].map((id) => +$(id).value || 0);
      if (k === "vise" && zero.some((v) => v)) post.origin = zero;
      const linesOf = (id) => $(id).value.split("\n").map((l) => l.trim()).filter(Boolean);
      if (linesOf("sdHeader").length) post.header = linesOf("sdHeader");
      if (linesOf("sdFooter").length) post.footer = linesOf("sdFooter");
      if ($("sdLineNums").checked) post.line_numbers = true;
      if ($("sdNoComments").checked) post.no_comments = true;
      const cfg = {
        post,
        postChanged: JSON.stringify(post) !== c.presetPost,
        mode: c.mode,
        name: c.mode === "new" ? $("sdName").value.trim() || "untitled" : undefined,
        setupName: c.mode === "new" ? undefined : $("sdName").value.trim() || undefined,
        machine: $("sdMachine").value || undefined,
        kind: k,
        units: $("sdUnits").value,
        axis: $("sdAxis").value,
        origin: num("sdOrigin"),
        margin: num("sdMargin"),
        explicit: carry ? undefined : ex,
        stock: carry ? undefined : spec,
        carry,
        jaws: k === "vise" ? mm("sdJaws") : undefined,
        // (the stock changed: given sizes, another shape, a margin typed)
        stockChanged: carry ? JSON.stringify([carry.turn, carry.offset, carry.from, carry.rotate]) !== c.presetCarry : !!ex || shape() !== c.presetShape || c.presetCarry !== null || STOCK_FIELDS.some((id) => id !== "sdJaws" && $(id).dataset.edited),
        tailstock: bar(k) && $("sdTail").checked,
        material: $("sdMaterial").value.trim() || null,
        // the placement fields typed in (others are the type's defaults)
        edited: ["sdMargin", "sdOrigin", "sdAxis"].filter((id) => $(id).dataset.edited).concat(JSON.stringify(rotate()) !== c.presetRotate ? ["sdRotate"] : [], c.modelPick ? ["sdModel"] : []),
        model: c.modelPick ? { kind: c.modelPick.kind, id: c.modelPick.id, file: c.modelPick.file, parsed: c.modelPick.parsed } : null,
        rotate: rotate(),
        file: c.parsed?.file ?? null,
        tris: c.parsed?.tris ?? null,
        ascii: !!c.parsed?.ascii,
        step: c.parsed?.step ?? null,
      };
      cur = null;
      dlg.close();
      c.resolve(cfg);
    } else {
      cur = null;
      dlg.close();
      c.resolve(null);
    }
  }

  // ------------------------------------------------------------ wiring
  dlg.addEventListener("change", (ev) => {
    const t = ev.target;
    if (t.name === "sdKind") {
      // to or from a vise: its usual stock (a block; a bar on the rotary axis), unless the
      // sizes were given
      const was = dlg.dataset.kind;
      if (was && bar(was) !== bar(t.value) && stockMode() !== "explicit") {
        const sh = bar(t.value) ? "round" : "block";
        for (const r of dlg.querySelectorAll('input[name="sdShape"]')) r.checked = r.value === sh;
      }
      dlg.dataset.kind = t.value;
      return layout(true);
    }
    if (t.name === "sdShape" || t.name === "sdStock") {
      stockLayout();
      return preview();
    }
    if (t.id === "sdCommon") return useCommon(+t.value);
    if (t.id === "sdStockUnits") {
      // the sizes typed stay the same lengths, in the other unit
      const f = t.value === "inch" ? 1 / IN : IN;
      for (const id of STOCK_FIELDS) if ($(id).value !== "") $(id).value = +(+$(id).value * f).toFixed(4);
      stockLayout();
      return preview();
    }
    if (STOCK_FIELDS.includes(t.id)) t.dataset.edited = "1";
    if (t.id === "sdFile") return chooseFile(t.files[0]);
    if (["sdMargin", "sdOrigin", "sdAxis"].includes(t.id)) t.dataset.edited = "1";
    preview();
  });
  dlg.addEventListener("input", (ev) => {
    const t = ev.target;
    // a hex bar's two sizes go together
    if (t.id === "sdAf" && t.value !== "") $("sdAc").value = +((+t.value * 2) / Math.sqrt(3)).toFixed(4);
    if (t.id === "sdAc" && t.value !== "") $("sdAf").value = +((+t.value * Math.sqrt(3)) / 2).toFixed(4);
    if (t.type === "number") preview();
  });
  $("sdFit").addEventListener("click", () => fitTo());
  /** The work zero a preset puts (in the setup's coordinates), or null (no box for it). */
  function zeroOf(p) {
    const b = p.startsWith("part") ? cur?.boxes?.part : cur?.boxes?.stock;
    if (!b) return null;
    const [lo, hi] = [b.min, b.max];
    const z = p.startsWith("bottom") ? lo[2] : hi[2];
    const at = p.split("-")[1];
    const x = at === "c" ? (lo[0] + hi[0]) / 2 : at === "lr" || at === "ur" ? hi[0] : lo[0];
    const y = at === "c" ? (lo[1] + hi[1]) / 2 : at === "ul" || at === "ur" ? hi[1] : lo[1];
    return [x, y, z].map((v) => Math.round(v * 1000) / 1000);
  }
  /** The preset a zero is, if one. */
  function zeroPreset(z) {
    for (const o of $("sdZeroAt").options) {
      if (o.value === "custom") continue;
      const q = zeroOf(o.value);
      if (q && q.every((v, i) => Math.abs(v - z[i]) < 1e-6)) return o.value;
    }
    // (none given: the usual)
    return z.every((v) => !v) ? "top-ll" : null;
  }
  $("sdZeroAt").addEventListener("change", () => {
    const q = zeroOf($("sdZeroAt").value);
    if (q) ["sdZeroX", "sdZeroY", "sdZeroZ"].forEach((id, i) => ($(id).value = String(q[i])));
  });
  for (const id of ["sdZeroX", "sdZeroY", "sdZeroZ"]) $(id).addEventListener("input", () => ($("sdZeroAt").value = zeroPreset(["sdZeroX", "sdZeroY", "sdZeroZ"].map((x) => +$(x).value || 0)) ?? "custom"));
  // this setup's part: another setup's (its triangles from the caller), or a file
  $("sdModel").addEventListener("change", async () => {
    const v = $("sdModel").value;
    if (!cur) return;
    if (v === "file") return $("sdModelFile").click();
    if (v.startsWith("setup:")) {
      const id = v.slice(6);
      cur.modelPick = { kind: "setup", id, tris: cur.preset.trisOf?.(id) ?? null };
    } else if (v === "none") cur.modelPick = { kind: "none" };
    else cur.modelPick = null;
    stockLayout();
    preview();
  });
  $("sdModelFile").addEventListener("change", async () => {
    const f = $("sdModelFile").files[0];
    $("sdModelFile").value = "";
    if (!f || !cur) return;
    try {
      const parsed = await readPart(new Uint8Array(await f.arrayBuffer()), f.name);
      cur.modelPick = { kind: "file", file: f, parsed, tris: parsed.tris };
      const o = document.createElement("option");
      o.value = "picked";
      o.textContent = f.name;
      $("sdModel").querySelector('option[value="picked"]')?.remove();
      $("sdModel").append(o);
      $("sdModel").value = "picked";
      preview();
    } catch (e) {
      $("sdWarn").textContent = `${f.name}: ${e.message}`;
      $("sdWarn").classList.remove("hidden");
      $("sdModel").value = cur.preset.modelChoices?.some((c) => c.value === "own") ? "own" : "none";
      cur.modelPick = $("sdModel").value === "none" ? { kind: "none" } : null;
    }
  });
  // (Mar) in the setup's details the form applies as it is changed, a moment after the last
  // change (its OK and Cancel not shown)
  let liveTimer = null;
  dlg.addEventListener("change", (ev) => {
    if (!cur?.embedded) return;
    if (ev.target?.id === "sdModel" || ev.target?.id === "sdModelFile" || ev.target?.id === "sdFile") return;
    clearTimeout(liveTimer);
    liveTimer = setTimeout(() => cur?.embedded && finish(true), 250);
  });
  $("sdForm").addEventListener("submit", (ev) => {
    ev.preventDefault();
    finish(true);
  });
  $("sdCancel").addEventListener("click", () => finish(false));
  dlg.addEventListener("cancel", (ev) => {
    ev.preventDefault();
    finish(false);
  });

  return { open, close: () => finish(false), preview, embeddedFor, reattach, park, get mode() { return cur?.mode ?? null; }, get modal() { return !!cur && !cur.embedded; } };
}
