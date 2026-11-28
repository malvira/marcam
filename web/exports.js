// NC exports (Mar: "as Fusion"): a named set of a setup's operations, picked from any of its
// files, written to a .nc file of its own. The engine posts each export with every plan (the
// operations' planned toolpaths, in planning order: what was simulated and checked) with a
// fingerprint of the G-code; the project keeps the fingerprint of what was last written, so the
// tree says, as the toolpaths update in the background, whether the file is still in sync.
//
// Where the browser lets a page write files (Chrome, Edge), the first write asks where to save
// it and later writes go to that same file without asking; elsewhere each write downloads
// <name>.nc.

const DB = "marcam-exports";
const STORE = "handles";

function idb() {
  return new Promise((res, rej) => {
    const r = indexedDB.open(DB, 1);
    r.onupgradeneeded = () => r.result.createObjectStore(STORE);
    r.onsuccess = () => res(r.result);
    r.onerror = () => rej(r.error);
  });
}
async function dbDo(mode, fn) {
  const db = await idb();
  try {
    return await new Promise((res, rej) => {
      const tx = db.transaction(STORE, mode);
      const r = fn(tx.objectStore(STORE));
      tx.oncomplete = () => res(r?.result);
      tx.onerror = () => rej(tx.error);
    });
  } finally {
    db.close();
  }
}
const getHandle = (k) => dbDo("readonly", (s) => s.get(k)).catch(() => null);
const putHandle = (k, h) => dbDo("readwrite", (s) => s.put(h, k)).catch(() => {});
const delHandle = (k) => dbDo("readwrite", (s) => s.delete(k)).catch(() => {});

/** Whether writes go to a file the person chose (and stay there). */
export const canPickFile = () => typeof window.showSaveFilePicker === "function";

/** Where an export's chosen file is remembered: the project, the setup, the export. */
export const exportKey = (project, setupId, name) => `${project}|${setupId}|${name}`;

/** The file an export writes to, if one was chosen: its name. */
export async function chosenFile(key) {
  if (!canPickFile()) return null;
  return (await getHandle(key))?.name ?? null;
}

/** An export renamed or deleted: its chosen file follows it, or is forgotten. */
export async function moveChosen(from, to) {
  const h = await getHandle(from);
  await delHandle(from);
  if (h && to) await putHandle(to, h);
}

function when(iso) {
  if (!iso) return "";
  const d = new Date(iso);
  if (Number.isNaN(+d)) return "";
  const today = new Date().toDateString() === d.toDateString();
  return today ? `at ${d.toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" })}` : `on ${d.toLocaleDateString([], { month: "short", day: "numeric" })} ${d.toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" })}`;
}

/** An export's state against the toolpaths planned for its setup (`res`: the plan's export
 *  entry, or null before the setup is planned): kind (ok, stale, new, empty, unknown), a
 *  short text for the tree and a sentence for its details. */
export function exportStatus(ex, res) {
  const file = `${ex.name}.nc`;
  const at = when(ex.posted_at);
  const missing = res?.missing?.length ? ` ${res.missing.length} of its operations ${res.missing.length === 1 ? "is" : "are"} not planned (disabled, in error or deleted) and left out.` : "";
  if (!res) return ex.posted ? { kind: "unknown", text: "written", long: `Written ${at}.` } : { kind: "new", text: "not written", long: `Not written yet.` };
  if (!res.fingerprint) return { kind: "empty", text: "nothing to write", long: `Nothing to write: none of its operations is planned (tick some below, or enable them).` };
  if (!ex.posted) return { kind: "new", text: "not written", long: `Not written yet: ${res.lines.toLocaleString("en")} lines ready.${missing}` };
  if (ex.posted === res.fingerprint) return { kind: "ok", text: "✓ in sync", long: `In sync: ${file} was written ${at} and its operations' toolpaths have not changed since.${missing}` };
  return { kind: "stale", text: "⚠ out of sync", long: `Out of sync: the toolpaths changed since ${file} was written ${at}. Write it again to update the file.${missing}` };
}

/** Write an export: the file is chosen (or its permission asked) at once, while the click
 *  still counts, then its G-code is taken once the toolpaths are current. `current()` gives
 *  the plan's entry for it then. Resolves {fingerprint, file, lines}, or null when the person
 *  cancelled the file chooser. */
export async function writeExport({ name, key, pick = false, current, whenSettled }) {
  let handle = null;
  if (canPickFile()) {
    handle = pick ? null : await getHandle(key);
    if (handle) {
      let p = await handle.queryPermission?.({ mode: "readwrite" });
      if (p !== "granted") p = await handle.requestPermission?.({ mode: "readwrite" });
      if (p !== "granted") handle = null;
    }
    if (!handle) {
      try {
        handle = await window.showSaveFilePicker({ suggestedName: `${name}.nc`, types: [{ description: "G-code", accept: { "text/plain": [".nc", ".gcode", ".tap", ".ngc"] } }] });
      } catch (e) {
        if (e?.name === "AbortError") return null;
        throw e;
      }
    }
  }
  await whenSettled();
  const r = current();
  if (!r?.fingerprint) throw new Error(`Nothing to write in ${name}.nc: none of its operations is planned.`);
  if (handle) {
    const w = await handle.createWritable();
    await w.write(r.gcode);
    await w.close();
    await putHandle(key, handle);
  } else {
    const url = URL.createObjectURL(new Blob([r.gcode], { type: "text/plain" }));
    const a = document.createElement("a");
    a.href = url;
    a.download = `${name}.nc`;
    document.body.appendChild(a);
    a.click();
    a.remove();
    setTimeout(() => URL.revokeObjectURL(url), 1000);
  }
  return { fingerprint: r.fingerprint, file: handle?.name || `${name}.nc`, lines: r.lines };
}
