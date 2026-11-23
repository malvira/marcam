// A headless Chrome/Edge/Chromium on the web app, over the DevTools protocol with Node's own
// WebSocket (no npm packages): serves the repository, opens the app, evaluates in the page,
// takes screenshots and collects page errors and network requests.
import { spawn } from "node:child_process";
import { existsSync, mkdirSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { createServer } from "node:net";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { get as nodeHttpGet } from "node:http";
// a GET over node:http, the body read to its end (Node's fetch has crashed on Windows CI in
// undici, `assert(!this.paused)`, on a response whose body was left unread)
function httpGet(u) {
  return new Promise((resolve, reject) => {
    const q = nodeHttpGet(u, (r) => {
      let body = "";
      r.setEncoding("utf8");
      r.on("data", (c) => (body += c));
      r.on("end", () => resolve({ ok: r.statusCode >= 200 && r.statusCode < 300, json: () => JSON.parse(body) }));
      r.on("error", reject);
    });
    q.on("error", reject);
    q.setTimeout(5000, () => q.destroy(new Error("timeout")));
  });
}
const httpOk = (u) => (u.startsWith("http:") ? httpGet(u) : fetch(u).then(async (r) => (await r.arrayBuffer(), r))).then((r) => r.ok);


export const root = join(dirname(fileURLToPath(import.meta.url)), "..", "..");
export const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
// (Windows, Node 22: Chrome gone with the DevTools socket open can trip an assertion inside
// undici's WebSocket; once closing, that one is not a test failure)
let closing = false;
process.on("uncaughtException", (e) => {
  if (closing && e?.code === "ERR_ASSERTION" && /undici/.test(e.stack || "")) return;
  console.error(e);
  process.exit(1);
});

export async function waitFor(fn, what, timeoutMs = 30000) {
  const t0 = Date.now();
  let last;
  while (Date.now() - t0 < timeoutMs) {
    try { last = await fn(); if (last) return last; } catch (e) { last = e; }
    await sleep(150);
  }
  throw new Error(`timed out waiting for ${what} (${last})`);
}

export function freePort() {
  return new Promise((resolve) => {
    const s = createServer();
    s.listen(0, "127.0.0.1", () => { const p = s.address().port; s.close(() => resolve(p)); });
  });
}

export function findBrowser() {
  const cands = process.platform === "win32"
    ? ["C:/Program Files/Google/Chrome/Application/chrome.exe", "C:/Program Files (x86)/Microsoft/Edge/Application/msedge.exe", "C:/Program Files/Microsoft/Edge/Application/msedge.exe"]
    : ["/usr/bin/google-chrome", "/usr/bin/chromium", "/usr/bin/chromium-browser", "/opt/pw-browsers/chromium", "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome"];
  const found = cands.find((p) => existsSync(p));
  if (!found) throw new Error("no Chrome/Edge/Chromium found");
  return found;
}

// what the browser logs on its own (a favicon), not the app
const EXPECTED_CONSOLE = [/favicon/i, /net::ERR_/, /Failed to load resource/];

/** Start the server and the browser, open the app; resolves when its engine is ready.
 *  `opts.root`: serve another checkout (default this one). */
export async function startApp(opts = {}) {
  const serveRoot = opts.root || root;
  const children = [];
  const cleanup = () => children.forEach((c) => { try { c.kill(); } catch {} });
  process.on("exit", cleanup);
  const port = await freePort();
  children.push(spawn(process.platform === "win32" ? "python" : "python3", ["-m", "http.server", String(port), "--bind", "127.0.0.1", "--directory", serveRoot], { stdio: "ignore" }));
  const url = `http://127.0.0.1:${port}/web/`;
  await waitFor(() => httpOk(url), "the web server", 15000);
  const profile = mkdtempSync(join(tmpdir(), "marcam-web-"));
  const dport = await freePort();
  children.push(spawn(findBrowser(), [
    "--headless=new", `--remote-debugging-port=${dport}`, `--user-data-dir=${profile}`, "--no-first-run", "--no-default-browser-check",
    "--disable-extensions", "--window-size=1400,900",
    ...(process.env.MARCAM_E2E_SOFTWARE_GL ? ["--use-angle=swiftshader", "--enable-unsafe-swiftshader"] : []), "about:blank",
  ], { stdio: "ignore" }));
  const version = await waitFor(() => httpGet(`http://127.0.0.1:${dport}/json/version`).then((r) => r.json()), "the browser", 20000);
  const ws = new WebSocket(version.webSocketDebuggerUrl);
  await new Promise((res, rej) => { ws.onopen = res; ws.onerror = () => rej(new Error("CDP connect failed")); });
  let id = 0;
  const waiters = new Map();
  const events = [];
  const send = (method, params = {}, sessionId) => {
    const n = ++id;
    ws.send(JSON.stringify({ id: n, method, params, sessionId }));
    return new Promise((resolve, reject) => waiters.set(n, { resolve, reject }));
  };
  ws.onmessage = (m) => {
    const msg = JSON.parse(m.data);
    // page dialogs (confirm, prompt): OK
    if (msg.method === "Page.javascriptDialogOpening") send("Page.handleJavaScriptDialog", { accept: true, promptText: msg.params.defaultPrompt ?? "" }, msg.sessionId);
    if (msg.id && waiters.has(msg.id)) {
      const { resolve, reject } = waiters.get(msg.id);
      waiters.delete(msg.id);
      msg.error ? reject(new Error(msg.error.message)) : resolve(msg.result);
    } else if (msg.method) events.push(msg);
  };
  const { targetId } = await send("Target.createTarget", { url: "about:blank" });
  const { sessionId } = await send("Target.attachToTarget", { targetId, flatten: true });
  const S = (m, p) => send(m, p, sessionId);
  await S("Page.enable");
  await S("Runtime.enable");
  await S("Network.enable");
  await S("Emulation.setDeviceMetricsOverride", { width: 1400, height: 900, deviceScaleFactor: 1, mobile: false });
  const js = async (expr) => {
    const r = await S("Runtime.evaluate", { expression: expr, awaitPromise: true, returnByValue: true, replMode: true });
    if (r.exceptionDetails) throw new Error(r.exceptionDetails.exception?.description || r.exceptionDetails.text);
    return r.result.value;
  };
  const shots = join(root, "web", "tests", "screenshots");
  mkdirSync(shots, { recursive: true });
  const app = {
    url, S, js, events,
    async shot(name) {
      const { data } = await S("Page.captureScreenshot", { format: "png" });
      writeFileSync(join(shots, `${name}.png`), Buffer.from(data, "base64"));
    },
    /** Page errors since event `from`: uncaught exceptions and console.error. */
    pageErrors(from = 0) {
      return events.slice(from).flatMap((e) => {
        if (e.method === "Runtime.exceptionThrown") return [e.params.exceptionDetails.exception?.description || e.params.exceptionDetails.text];
        if (e.method === "Runtime.consoleAPICalled" && e.params.type === "error") {
          const t = e.params.args.map((a) => a.value ?? a.description ?? "").join(" ");
          return EXPECTED_CONSOLE.some((r) => r.test(t)) ? [] : [t];
        }
        return [];
      });
    },
    /** URLs requested since event `from`. */
    requests(from = 0) {
      return events.slice(from).filter((e) => e.method === "Network.requestWillBeSent").map((e) => e.params.request.url);
    },
    async settle(ms = 250) {
      await waitFor(() => js("document.body.dataset.state !== 'busy' && !!window.__marcam.state.registry"), "the app to settle", 240000);
      await sleep(ms);
    },
    async open() {
      await S("Page.navigate", { url });
      // ready, or (live updating plans the startup example) done planning it
      try {
        await waitFor(() => js("['ready', 'done'].includes(document.body.dataset.state) && window.__marcam.planIdle?.() !== false"), "the engine", 120000);
      } catch (e) {
        const why = await js("JSON.stringify({ state: document.body.dataset.state, status: document.getElementById('status')?.textContent, error: document.getElementById('error')?.textContent, idle: window.__marcam?.planIdle?.() })").catch((x) => x.message);
        throw new Error(`${e.message}: ${why} ${app.pageErrors().join(" | ").slice(0, 600)}`);
      }
    },
    async close() {
      // (closed in order: the browser asked, our socket closed, then whatever is left killed)
      closing = true;
      try { await Promise.race([send("Browser.close"), sleep(2000)]); } catch {}
      try { ws.onmessage = null; ws.close(); } catch {}
      await sleep(300);
      cleanup();
      await sleep(300);
      try { rmSync(profile, { recursive: true, force: true }); } catch {}
    },
  };
  await app.open();
  return app;
}
