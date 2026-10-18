// The browser driver the zero-dependency smokes share (smoke-boosted.mjs, smoke-components.mjs,
// prove.mjs): find the installed Chrome, Chromium or Edge, launch it with a throwaway profile, and
// talk the Chrome DevTools Protocol to it over --remote-debugging-pipe. Node >= 18, no npm install,
// never a browser download. CHROME_PATH (or CHROME) names the browser; else the usual places on
// macOS, Linux and Windows are tried. No certificate overrides.
import { spawn } from "node:child_process";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";

/** The exit status a smoke uses when there is no browser (verify and CI report it as skipped). */
export const SKIP_EXIT = 77;

export const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

/** The browser's executable, or null. */
export function findChrome() {
  const given = process.env.CHROME_PATH || process.env.CHROME;
  if (given) return fs.existsSync(given) ? given : null;
  const candidates = [];
  if (process.platform === "darwin") {
    for (const root of ["/Applications", path.join(os.homedir(), "Applications")]) {
      candidates.push(
        `${root}/Google Chrome.app/Contents/MacOS/Google Chrome`,
        `${root}/Chromium.app/Contents/MacOS/Chromium`,
        `${root}/Google Chrome Canary.app/Contents/MacOS/Google Chrome Canary`,
        `${root}/Microsoft Edge.app/Contents/MacOS/Microsoft Edge`,
      );
    }
  } else if (process.platform === "win32") {
    const roots = [process.env.PROGRAMFILES, process.env["PROGRAMFILES(X86)"], process.env.LOCALAPPDATA].filter(Boolean);
    for (const root of roots) {
      candidates.push(
        path.join(root, "Google", "Chrome", "Application", "chrome.exe"),
        path.join(root, "Chromium", "Application", "chrome.exe"),
        path.join(root, "Microsoft", "Edge", "Application", "msedge.exe"),
      );
    }
  } else {
    const names = ["google-chrome-stable", "google-chrome", "chromium", "chromium-browser", "chrome", "microsoft-edge"];
    for (const dir of (process.env.PATH ?? "").split(path.delimiter).filter(Boolean)) {
      for (const n of names) candidates.push(path.join(dir, n));
    }
    candidates.push("/opt/google/chrome/chrome", "/snap/bin/chromium");
  }
  return candidates.find((c) => {
    try {
      return fs.statSync(c).isFile();
    } catch {
      return false;
    }
  }) ?? null;
}

/** The browser, or "skip: no Chrome" and exit 77. */
export function requireChrome() {
  const chrome = findChrome();
  if (chrome) return chrome;
  const given = process.env.CHROME_PATH || process.env.CHROME;
  console.log(given
    ? `skip: no Chrome (CHROME_PATH=${given} is not a file)`
    : "skip: no Chrome (install Google Chrome or Chromium, or set CHROME_PATH=<the browser executable>)");
  process.exit(SKIP_EXIT);
}

/** A CDP connection over --remote-debugging-pipe: fd 3 to Chrome, fd 4 from it, NUL-framed JSON. */
export class Cdp {
  constructor(proc) {
    this.writer = proc.stdio[3];
    this.nextId = 0;
    this.pending = new Map();
    this.listeners = new Set();
    let buffered = Buffer.alloc(0);
    proc.stdio[4].on("data", (chunk) => {
      buffered = Buffer.concat([buffered, chunk]);
      let end;
      while ((end = buffered.indexOf(0)) !== -1) {
        const msg = JSON.parse(buffered.subarray(0, end).toString("utf8"));
        buffered = buffered.subarray(end + 1);
        if (msg.id !== undefined) {
          const p = this.pending.get(msg.id);
          this.pending.delete(msg.id);
          if (!p) continue;
          if (msg.error) p.reject(new Error(`${p.method}: ${msg.error.message}`));
          else p.resolve(msg.result);
        } else {
          for (const l of this.listeners) l(msg);
        }
      }
    });
    proc.on("exit", () => {
      for (const p of this.pending.values()) p.reject(new Error("Chrome exited"));
      this.pending.clear();
    });
  }
  send(method, params = {}, sessionId = undefined, timeout = 60000) {
    const id = ++this.nextId;
    const msg = sessionId ? { id, method, params, sessionId } : { id, method, params };
    return new Promise((resolve, reject) => {
      const timer = setTimeout(() => {
        this.pending.delete(id);
        reject(new Error(`${method}: no answer in ${timeout} ms`));
      }, timeout);
      this.pending.set(id, {
        method,
        resolve: (v) => { clearTimeout(timer); resolve(v); },
        reject: (e) => { clearTimeout(timer); reject(e); },
      });
      this.writer.write(`${JSON.stringify(msg)}\0`);
    });
  }
  on(fn) {
    this.listeners.add(fn);
    return () => this.listeners.delete(fn);
  }
}

/**
 * Launches the browser headless (HEADED=1: a window) with a throwaway profile and connects.
 * `prefix`: the profile directory's name; `args`: extra flags. Returns { cdp, version, close }:
 * `close()` closes the browser and deletes the profile.
 */
export async function launch(chrome, { prefix = "offrails-smoke-", args: extra = [] } = {}) {
  const profile = fs.mkdtempSync(path.join(os.tmpdir(), prefix));
  const args = [
    "--remote-debugging-pipe",
    `--user-data-dir=${profile}`,
    "--no-first-run",
    "--no-default-browser-check",
    "--disable-extensions",
    "--disable-background-networking",
    "--disable-component-update",
    "--disable-sync",
    "--mute-audio",
    ...extra,
  ];
  if (process.env.HEADED !== "1") args.push("--headless=new");
  // GitHub's Ubuntu runners restrict the user namespaces Chrome's sandbox needs.
  if (process.platform === "linux" && (process.env.CI || process.getuid?.() === 0)) args.push("--no-sandbox");
  args.push("about:blank");
  const proc = spawn(chrome, args, { stdio: ["ignore", "ignore", "pipe", "pipe", "pipe"] });
  let stderr = "";
  proc.stderr.on("data", (d) => { stderr = (stderr + d).slice(-4000); });
  const cdp = new Cdp(proc);
  const close = async () => {
    await cdp.send("Browser.close", {}, undefined, 5000).catch(() => {});
    if (proc.exitCode === null) {
      await Promise.race([new Promise((r) => proc.once("exit", r)), sleep(3000)]);
      if (proc.exitCode === null) proc.kill();
    }
    try {
      fs.rmSync(profile, { recursive: true, force: true, maxRetries: 5, retryDelay: 200 });
    } catch {
      // Windows can hold the profile a moment longer; it is in the temp dir
    }
  };
  try {
    const version = await cdp.send("Browser.getVersion", {}, undefined, 30000);
    return { cdp, version, close };
  } catch (e) {
    await close();
    throw new Error(`${chrome} did not start (${e.message})\n${stderr}`);
  }
}

/**
 * One tab in its own browser context (its own cookies). Collects console errors, uncaught
 * exceptions and CSP reports in `errors` (prefixed with `phase` when one is set); `ignore` is a
 * regular expression of messages to allow. Dialogs are dismissed.
 */
export class Tab {
  static async open(cdp, label, { ignore = null, viewport = null } = {}) {
    const { browserContextId } = await cdp.send("Target.createBrowserContext", { disposeOnDetach: true });
    const { targetId } = await cdp.send("Target.createTarget", { url: "about:blank", browserContextId });
    const { sessionId } = await cdp.send("Target.attachToTarget", { targetId, flatten: true });
    const tab = new Tab(cdp, sessionId, label, ignore);
    await Promise.all([tab.send("Page.enable"), tab.send("Runtime.enable"), tab.send("Log.enable")]);
    if (viewport) await tab.viewport(viewport);
    return tab;
  }
  constructor(cdp, sessionId, label, ignore = null) {
    this.cdp = cdp;
    this.sessionId = sessionId;
    this.label = label;
    this.phase = null;
    this.errors = [];
    cdp.on((m) => {
      if (m.sessionId !== sessionId) return;
      const p = m.params;
      let text = null;
      if (m.method === "Runtime.consoleAPICalled" && (p.type === "error" || p.type === "assert")) {
        text = p.args.map((a) => a.value ?? a.description ?? a.type).join(" ");
      } else if (m.method === "Runtime.exceptionThrown") {
        const d = p.exceptionDetails;
        text = `uncaught: ${d.exception?.description ?? d.text}`;
      } else if (m.method === "Log.entryAdded" && p.entry.level === "error") {
        text = `${p.entry.text}${p.entry.url ? ` (${p.entry.url})` : ""}`;
      } else if (m.method === "Page.javascriptDialogOpening") {
        this.send("Page.handleJavaScriptDialog", { accept: false }).catch(() => {});
      }
      if (text && !(ignore && ignore.test(text))) {
        const line = text.split("\n")[0];
        this.errors.push(this.phase ? `${this.phase}: ${line}` : line);
      }
    });
  }
  send(method, params = {}, timeout = undefined) {
    return this.cdp.send(method, params, this.sessionId, timeout);
  }
  /** Runs `fn(...args)` in the page and returns its (awaited, JSON) value. */
  async eval(fn, ...args) {
    const r = await this.send("Runtime.evaluate", {
      expression: `(${fn})(...${JSON.stringify(args)})`,
      awaitPromise: true,
      returnByValue: true,
    });
    if (r.exceptionDetails) throw new Error(r.exceptionDetails.exception?.description ?? r.exceptionDetails.text);
    return r.result.value;
  }
  /** A full page load: navigates and waits for `load`, then `settle()`. */
  async goto(url, quietMs = 150) {
    const loaded = new Promise((resolve) => {
      const off = this.cdp.on((m) => {
        if (m.sessionId === this.sessionId && m.method === "Page.loadEventFired") {
          off();
          resolve();
        }
      });
      setTimeout(() => { off(); resolve(); }, 20000);
    });
    const nav = await this.send("Page.navigate", { url });
    if (nav.errorText) throw new Error(`${url}: ${nav.errorText}`);
    await loaded;
    await this.settle(quietMs);
  }
  /** Waits until the document is complete, htmx is idle, fonts are in, then `quietMs` more. */
  async settle(quietMs = 150) {
    for (let i = 0; i < 100; i += 1) {
      const idle = await this.eval(() =>
        document.readyState === "complete" && !document.querySelector(".htmx-request, .htmx-swapping, .htmx-settling"),
      ).catch(() => false);
      if (idle) break;
      await sleep(50);
    }
    await this.eval(() => (document.fonts ? document.fonts.ready.then(() => true) : true)).catch(() => {});
    await sleep(quietMs);
  }
  /** The window size (`{ width, height, mobile }`) and, when given, the color scheme emulated. */
  async viewport(vp, scheme = null) {
    await this.send("Emulation.setDeviceMetricsOverride", {
      width: vp.width, height: vp.height, deviceScaleFactor: 1, mobile: !!vp.mobile,
    });
    if (scheme) {
      await this.send("Emulation.setEmulatedMedia", { features: [{ name: "prefers-color-scheme", value: scheme }] });
    }
  }
  /** A full-page PNG, at most `maxHeight` CSS pixels tall: { png, cut }. */
  async screenshot(maxHeight = 8000) {
    const m = await this.send("Page.getLayoutMetrics");
    const size = m.cssContentSize ?? m.contentSize;
    const vw = m.cssLayoutViewport?.clientWidth ?? size.width;
    const height = Math.min(Math.ceil(size.height), maxHeight);
    const shot = await this.send("Page.captureScreenshot", {
      format: "png",
      captureBeyondViewport: true,
      clip: { x: 0, y: 0, width: vw, height, scale: 1 },
    });
    return { png: Buffer.from(shot.data, "base64"), cut: size.height > maxHeight };
  }
}

/**
 * The POST route a password sign-in form submits to, from docs/routes.md's rows ({ method, path }):
 * SMOKE_SIGNIN_PATH when set, else the first of the usual ones the app has (/session/password from
 * the auth-password part, /session, /sessions, /login, /sign_in, /users/sign_in), else null. The
 * smokes then sign in through the form they find on the sign-in page (SMOKE_LOGIN), whatever its
 * action, so this only decides whether to try.
 */
export function signInPost(routes) {
  if (process.env.SMOKE_SIGNIN_PATH) return process.env.SMOKE_SIGNIN_PATH;
  const usual = ["/session/password", "/session", "/sessions", "/login", "/sign_in", "/users/sign_in"];
  return usual.find((p) => routes.some((r) => r.method === "POST" && r.path === p)) ?? null;
}
