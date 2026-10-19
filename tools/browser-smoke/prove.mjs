// Proof that what was built works: screenshots and checks of every page, and a PROOF.md that ties
// each acceptance criterion to its tests and to the pages it touches. Run through
//
//   BASE=<the URL bin/dev printed> cargo offrails prove [--strict] [--commit-screenshots]
//   BASE=… node .offrails/tools/browser-smoke/prove.mjs        # the same, from the app root
//
// It starts nothing: the app must be running. For every GET route in docs/routes.md without a
// parameter (plus SMOKE_PATHS, minus SMOKE_SKIP) that answers an HTML page, as the visitor who can
// see it (signed out; signed in as the development demo account when the page needs it and the app
// has password sign-in, like smoke-boosted.mjs), it:
//   - takes full-page screenshots at desktop 1280 and mobile 390, in light and dark
//     (prefers-color-scheme emulated; the theme cookie is left alone, so "system" follows it);
//   - fails on a console error, an uncaught exception or a CSP violation in any of the four loads,
//     on axe-core violations (WCAG 2.2 AA, light and dark, as the a11y smoke), on a link to a page
//     of this site that answers 4xx/5xx or an #anchor with no target, and on horizontal overflow at
//     390px (the page scrolls sideways on a phone);
//   - compares each screenshot with the previous run's (a pixel diff, Node only): reported, and a
//     failure only with --strict (PROVE_STRICT=1).
// It writes docs/proof/PROOF.md (and proof.json), the screenshots in docs/proof/screens/ and the
// diff images in docs/proof/diff/. PNGs are git-ignored unless --commit-screenshots.
//
// Zero dependencies: Node >= 18 and an installed Chrome, Chromium or Edge over the DevTools
// Protocol on a pipe: cdp.mjs, the driver and browser discovery every smoke here shares. axe-core comes
// from the browser smokes' cache (run.sh installs it); when it isn't there, prove installs that one
// pinned package into the same cache with npm, or reports axe as skipped. No browser: "skip: no
// Chrome", exit 77.
//
// Environment: BASE (required), ROUTES (default docs/routes.md), SMOKE_PATHS, SMOKE_SKIP (default
// /dev/lab*), SMOKE_EMAIL, SMOKE_PASSWORD, SMOKE_LOGIN (default /session/new), SMOKE_SIGNIN_PATH
// (the POST route the sign-in form submits to, when it isn't one of the usual ones), SMOKE_IGNORE (a
// regular expression of console messages to allow), PROVE_OUT (default docs/proof),
// PROVE_DIFF_THRESHOLD (percent of pixels, default 0.5), PROVE_AXE=0 (skip axe),
// PROVE_MAX_HEIGHT (default 8000 px per screenshot), PROVE_LINK_SKIP (paths not to fetch),
// HEADED=1. Exit status: 0 proven, 1 a check failed, 2 usage, 77 no browser.
import { execFileSync } from "node:child_process";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";
import zlib from "node:zlib";

import { Tab, launch, requireChrome, signInPost, sleep } from "./cdp.mjs";

const AXE_VERSION = "4.13.0"; // the a11y smoke's pin (run.sh)
const BASE = (process.env.BASE ?? "").replace(/\/$/, "");
const ROUTES = process.env.ROUTES ?? path.join("docs", "routes.md");
const LOGIN = process.env.SMOKE_LOGIN ?? "/session/new";
const EMAIL = process.env.SMOKE_EMAIL ?? "demo@example.com";
const PASSWORD = process.env.SMOKE_PASSWORD ?? "not-a-real-password-42";
const IGNORE = process.env.SMOKE_IGNORE ? new RegExp(process.env.SMOKE_IGNORE) : null;
const OUT = process.env.PROVE_OUT ?? path.join("docs", "proof");
const STRICT = process.env.PROVE_STRICT === "1";
const COMMIT_SCREENSHOTS = process.env.PROVE_COMMIT_SCREENSHOTS === "1";
const THRESHOLD = Number(process.env.PROVE_DIFF_THRESHOLD ?? "0.5");
const MAX_HEIGHT = Number(process.env.PROVE_MAX_HEIGHT ?? "8000");
const VIEWPORTS = [
  { name: "desktop", width: 1280, height: 800, mobile: false },
  { name: "mobile", width: 390, height: 844, mobile: true },
];
const SCHEMES = ["light", "dark"];

const started = Date.now();
const patterns = (v) => (v ?? "").split(",").map((s) => s.trim()).filter(Boolean);
const matches = (list, p) => list.some((x) => (x.endsWith("*") ? p.startsWith(x.slice(0, -1)) : p === x));

// ---- the browser: cdp.mjs (findChrome, launch, Cdp, Tab), shared with the other smokes ----------

// ---- in the page ------------------------------------------------------------------------------

/** How a path answers a GET (redirects followed): an HTML page, a redirect, a fragment, … */
async function classify(p) {
  const ctl = new AbortController();
  const timer = setTimeout(() => ctl.abort(), 8000);
  try {
    const res = await fetch(p, { headers: { accept: "text/html" }, signal: ctl.signal });
    const type = (res.headers.get("content-type") || "").split(";")[0];
    const at = new URL(res.url).pathname + new URL(res.url).search;
    if (new URL(res.url).origin !== location.origin) return { kind: "elsewhere" };
    if (type !== "text/html") return { kind: "not-html", type, status: res.status };
    if (res.status !== 200) return { kind: "status", status: res.status };
    const html = await res.text();
    if (!/<body[\s>]/i.test(html)) return { kind: "fragment" };
    return at === p ? { kind: "page" } : { kind: "redirect", at };
  } catch (e) {
    return { kind: "error", error: String(e.name || e) };
  } finally {
    clearTimeout(timer);
    ctl.abort();
  }
}

/** Same-site links on the page (path + query), and #anchors with no target. */
function links() {
  const out = new Set();
  const missing = [];
  for (const a of document.querySelectorAll("a[href]")) {
    const raw = a.getAttribute("href");
    if (!raw || raw.startsWith("mailto:") || raw.startsWith("tel:") || raw.startsWith("javascript:") || a.hasAttribute("download")) continue;
    const u = new URL(raw, location.href);
    if (u.origin !== location.origin) continue;
    if (u.pathname === location.pathname && u.search === location.search && u.hash) {
      const id = decodeURIComponent(u.hash.slice(1));
      if (id && !document.getElementById(id)) missing.push(raw);
      continue;
    }
    out.add(u.pathname + u.search);
  }
  return { links: [...out], missing: [...new Set(missing)] };
}

/** Horizontal overflow: does the page scroll sideways, and which elements stick out. */
function overflow() {
  const root = document.documentElement;
  const width = root.clientWidth;
  if (root.scrollWidth <= width + 1) return null;
  const culprits = [];
  for (const el of document.body.querySelectorAll("*")) {
    const r = el.getBoundingClientRect();
    if (r.right > width + 1 && r.width > 0 && el.getClientRects().length) {
      const parent = el.parentElement?.getBoundingClientRect();
      if (parent && parent.right > width + 1) continue; // report the outermost
      const name = `${el.tagName.toLowerCase()}${el.id ? `#${el.id}` : ""}${el.classList.length ? `.${[...el.classList].slice(0, 3).join(".")}` : ""}`;
      const text = (el.textContent || "").trim().replace(/\s+/g, " ").slice(0, 24);
      culprits.push(`${name}${text ? ` "${text}"` : ""} (right edge ${Math.round(r.right)}px)`);
      if (culprits.length >= 3) break;
    }
  }
  return { scrollWidth: root.scrollWidth, width, culprits };
}

async function fetchStatus(p) {
  const ctl = new AbortController();
  const timer = setTimeout(() => ctl.abort(), 8000);
  try {
    const res = await fetch(p, { signal: ctl.signal, headers: { accept: "text/html,*/*" } });
    return res.status;
  } catch (e) {
    return String(e.name || e);
  } finally {
    clearTimeout(timer);
    ctl.abort();
  }
}

// ---- axe-core ---------------------------------------------------------------------------------

function axeSource() {
  if (process.env.PROVE_AXE === "0") return { skip: "PROVE_AXE=0" };
  const cache = path.join(process.env.XDG_CACHE_HOME || path.join(os.homedir(), ".cache"), "offrails", "browser-smoke");
  const file = path.join(cache, "node_modules", "axe-core", "axe.min.js");
  if (!fs.existsSync(file)) {
    try {
      fs.mkdirSync(cache, { recursive: true });
      console.log(`prove: installing axe-core@${AXE_VERSION} into ${cache} (once; the browser smokes share it)`);
      const npm = process.platform === "win32" ? "npm.cmd" : "npm";
      execFileSync(npm, ["install", "--prefix", cache, "--no-audit", "--no-fund", "--no-save", "--silent", `axe-core@${AXE_VERSION}`], {
        stdio: "inherit",
        shell: process.platform === "win32",
      });
    } catch (e) {
      return { skip: `axe-core is not installed and npm could not install it (${e.message.split("\n")[0]}); run sh .offrails/tools/browser-smoke/run.sh a11y once` };
    }
  }
  try {
    return { src: fs.readFileSync(file, "utf8") };
  } catch {
    return { skip: `no ${file}` };
  }
}

async function runAxe(tab, src) {
  await tab.send("Runtime.evaluate", { expression: src, returnByValue: true });
  return tab.eval(async () => {
    const r = await window.axe.run(document, {
      runOnly: { type: "tag", values: ["wcag2a", "wcag2aa", "wcag21a", "wcag21aa", "wcag22aa"] },
    });
    return r.violations.map((v) => ({
      id: v.id,
      impact: v.impact,
      count: v.nodes.length,
      nodes: v.nodes.slice(0, 3).map((n) => n.target.join(" ")),
    }));
  });
}

// ---- PNG: decode, diff, encode (node:zlib only) -----------------------------------------------

const CRC_TABLE = new Uint32Array(256).map((_, n) => {
  let c = n;
  for (let k = 0; k < 8; k += 1) c = c & 1 ? 0xedb88320 ^ (c >>> 1) : c >>> 1;
  return c >>> 0;
});
function crc32(buf) {
  let c = 0xffffffff;
  for (const b of buf) c = CRC_TABLE[(c ^ b) & 0xff] ^ (c >>> 8);
  return (c ^ 0xffffffff) >>> 0;
}

/** RGBA pixels of an 8-bit, non-interlaced PNG (gray, RGB, gray+alpha or RGBA). */
export function decodePng(buf) {
  if (buf.readUInt32BE(0) !== 0x89504e47) throw new Error("not a PNG");
  let off = 8;
  let width = 0;
  let height = 0;
  let type = 0;
  let depth = 0;
  let interlace = 0;
  const idat = [];
  while (off < buf.length) {
    const len = buf.readUInt32BE(off);
    const kind = buf.toString("latin1", off + 4, off + 8);
    const data = buf.subarray(off + 8, off + 8 + len);
    if (kind === "IHDR") {
      width = data.readUInt32BE(0);
      height = data.readUInt32BE(4);
      depth = data[8];
      type = data[9];
      interlace = data[12];
    } else if (kind === "IDAT") idat.push(data);
    else if (kind === "IEND") break;
    off += 12 + len;
  }
  const channels = { 0: 1, 2: 3, 4: 2, 6: 4 }[type];
  if (depth !== 8 || !channels || interlace) throw new Error(`unsupported PNG (type ${type}, depth ${depth}, interlace ${interlace})`);
  const raw = zlib.inflateSync(Buffer.concat(idat));
  const stride = width * channels;
  const out = Buffer.alloc(width * height * 4);
  let prev = Buffer.alloc(stride);
  for (let y = 0; y < height; y += 1) {
    const filter = raw[y * (stride + 1)];
    const line = Buffer.from(raw.subarray(y * (stride + 1) + 1, (y + 1) * (stride + 1)));
    for (let x = 0; x < stride; x += 1) {
      const a = x >= channels ? line[x - channels] : 0;
      const b = prev[x];
      const c = x >= channels ? prev[x - channels] : 0;
      let v = line[x];
      if (filter === 1) v += a;
      else if (filter === 2) v += b;
      else if (filter === 3) v += (a + b) >> 1;
      else if (filter === 4) {
        const p = a + b - c;
        const pa = Math.abs(p - a);
        const pb = Math.abs(p - b);
        const pc = Math.abs(p - c);
        v += pa <= pb && pa <= pc ? a : pb <= pc ? b : c;
      }
      line[x] = v & 0xff;
    }
    for (let x = 0; x < width; x += 1) {
      const o = (y * width + x) * 4;
      const i = x * channels;
      if (channels >= 3) {
        out[o] = line[i];
        out[o + 1] = line[i + 1];
        out[o + 2] = line[i + 2];
        out[o + 3] = channels === 4 ? line[i + 3] : 255;
      } else {
        out[o] = out[o + 1] = out[o + 2] = line[i];
        out[o + 3] = channels === 2 ? line[i + 1] : 255;
      }
    }
    prev = line;
  }
  return { width, height, data: out };
}

export function encodePng({ width, height, data }) {
  const rows = Buffer.alloc((width * 4 + 1) * height);
  for (let y = 0; y < height; y += 1) data.copy(rows, y * (width * 4 + 1) + 1, y * width * 4, (y + 1) * width * 4);
  const chunk = (kind, body) => {
    const head = Buffer.alloc(8);
    head.writeUInt32BE(body.length, 0);
    head.write(kind, 4, "latin1");
    const crc = Buffer.alloc(4);
    crc.writeUInt32BE(crc32(Buffer.concat([head.subarray(4), body])), 0);
    return Buffer.concat([head, body, crc]);
  };
  const ihdr = Buffer.alloc(13);
  ihdr.writeUInt32BE(width, 0);
  ihdr.writeUInt32BE(height, 4);
  ihdr[8] = 8;
  ihdr[9] = 6;
  return Buffer.concat([
    Buffer.from([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]),
    chunk("IHDR", ihdr),
    chunk("IDAT", zlib.deflateSync(rows)),
    chunk("IEND", Buffer.alloc(0)),
  ]);
}

/** Percent of pixels that differ (any channel by more than 24), and a diff image: changed pixels
 *  red over a faded copy of the new screenshot. A size change counts the uncovered area. */
export function diffPng(before, after) {
  const width = Math.max(before.width, after.width);
  const height = Math.max(before.height, after.height);
  const img = Buffer.alloc(width * height * 4);
  let changed = 0;
  const px = (im, x, y) => (x < im.width && y < im.height ? (y * im.width + x) * 4 : -1);
  for (let y = 0; y < height; y += 1) {
    for (let x = 0; x < width; x += 1) {
      const a = px(before, x, y);
      const b = px(after, x, y);
      const o = (y * width + x) * 4;
      let differs = a < 0 || b < 0;
      if (!differs) {
        for (let k = 0; k < 4; k += 1) {
          if (Math.abs(before.data[a + k] - after.data[b + k]) > 24) {
            differs = true;
            break;
          }
        }
      }
      if (differs) {
        changed += 1;
        img[o] = 230; img[o + 1] = 30; img[o + 2] = 60; img[o + 3] = 255;
      } else {
        const g = Math.round((after.data[b] + after.data[b + 1] + after.data[b + 2]) / 3);
        img[o] = img[o + 1] = img[o + 2] = 200 + Math.round(g / 5);
        img[o + 3] = 255;
      }
    }
  }
  return {
    percent: (changed / (width * height)) * 100,
    resized: before.width !== after.width || before.height !== after.height,
    image: { width, height, data: img },
  };
}

// ---- the app's own files ----------------------------------------------------------------------

function routes() {
  let text;
  try {
    text = fs.readFileSync(ROUTES, "utf8");
  } catch {
    console.error(`prove: no ${ROUTES} (run it from the app root, or set ROUTES)`);
    process.exit(2);
  }
  const rows = [];
  for (const line of text.split(/\r?\n/)) {
    const m = line.match(/^\|\s*([A-Z]+)\s*\|\s*`([^`]+)`\s*\|\s*`?([^`|]*)`?\s*\|\s*([a-z]+)?/);
    if (m) rows.push({ method: m[1], path: m[2], handler: m[3].trim(), access: (m[4] ?? "").trim() });
  }
  return rows;
}

function walk(dir, exts, out = []) {
  let entries;
  try {
    entries = fs.readdirSync(dir, { withFileTypes: true });
  } catch {
    return out;
  }
  for (const e of entries) {
    if (e.name === "target" || e.name === "node_modules" || e.name.startsWith(".")) continue;
    const p = path.join(dir, e.name);
    if (e.isDirectory()) walk(p, exts, out);
    else if (exts.some((x) => e.name.endsWith(x))) out.push(p);
  }
  return out;
}

/** Acceptance criteria from product/features/*.md: { id, text, ticked, struck, file, paths }. */
function criteria() {
  const out = [];
  for (const file of walk(path.join("product", "features"), [".md"]).sort()) {
    for (const line of fs.readFileSync(file, "utf8").split(/\r?\n/)) {
      const m = line.match(/^\s*[-*]\s+(?:\[([ xX])\]\s+)?(~~)?(AC-[A-Za-z0-9_-]+-\d+)(~~)?:?\s*(.*)$/);
      if (!m) continue;
      const text = m[5].replace(/~~/g, "").trim();
      const paths = [...text.matchAll(/`(\/[^`\s]*)`/g)].map((x) => x[1]);
      out.push({ id: m[3], text, ticked: (m[1] ?? " ").toLowerCase() === "x", struck: Boolean(m[2]), file: file.split(path.sep).join("/"), paths });
    }
  }
  return out;
}

/** `paths::name()` → "/literal" from src/routes.rs (the helpers that take no argument). */
function pathHelpers() {
  const map = new Map();
  let src = "";
  try {
    src = fs.readFileSync(path.join("src", "routes.rs"), "utf8");
  } catch {
    return map;
  }
  for (const m of src.matchAll(/pub fn ([a-z0-9_]+)\(\)\s*->\s*String\s*\{\s*"([^"]+)"\.into\(\)/g)) map.set(m[1], m[2]);
  return map;
}

/** A test attribute: #[test], #[tokio::test], #[tokio::test(flavor = …)], #[sqlx::test], #[rstest]… */
const TEST_ATTR = /^\s*#\[\s*(?:[A-Za-z_]\w*::)*(?:test|rstest|test_case)\b/;

/** In Rust, the `fn` at line `j` (cited at line `i`) is a test: a test attribute among the
 *  attributes and comments around the citation, and in src/ inside a #[cfg(test)] module. A
 *  handler or helper whose comment mentions an AC id is not a test. */
function isRustTest(lines, i, j, file) {
  let k = i;
  while (k > 0 && /^\s*(#\[|\/\/)/.test(lines[k - 1])) k -= 1;
  // Only attributes, comments and blank lines between the citation and the fn: a comment
  // inside a body does not cite the next test.
  if (!lines.slice(i + 1, j).every((l) => /^\s*(#\[|\/\/|$)/.test(l))) return false;
  const preamble = lines.slice(k, j + 1);
  if (!preamble.some((l) => TEST_ATTR.test(l))) return false;
  if (file.startsWith("src")) return lines.slice(0, i).some((l) => /^\s*#\[cfg\(test\)\]/.test(l));
  return true;
}

/** Every test that cites an AC id: { id, file, line, name, paths }. The test is the first `fn`
 *  after the citation (in Rust, only when it is a test function: isRustTest); its paths are the
 *  "/…" literals and paths::…() calls in its body. */
function citations(helpers) {
  const out = [];
  const files = [...walk("tests", [".rs", ".mjs", ".ts", ".js"]), ...walk("src", [".rs"]), ...walk(path.join("web", "src"), [".ts", ".tsx", ".js", ".svelte"])];
  for (const file of files) {
    const lines = fs.readFileSync(file, "utf8").split(/\r?\n/);
    lines.forEach((line, i) => {
      for (const m of line.matchAll(/AC-[A-Za-z0-9_-]+-\d+/g)) {
        let name = null;
        let start = i;
        for (let j = i; j < Math.min(lines.length, i + 15); j += 1) {
          const f = lines[j].match(/\bfn\s+([a-z0-9_]+)\s*[(<]/) ?? lines[j].match(/\btest\(\s*["'`]([^"'`]+)/);
          if (f) {
            name = f[1];
            start = j;
            break;
          }
        }
        if (!name) continue;
        if (file.endsWith(".rs") && !isRustTest(lines, i, start, file)) continue;
        // The body: up to the next line that starts a new top-level item.
        const body = [];
        for (let j = start; j < lines.length; j += 1) {
          if (j > start && /^(\S.*\bfn\s|#\[|\/\/\/|test\()/.test(lines[j])) break;
          body.push(lines[j]);
        }
        const text = body.join("\n");
        const ps = new Set([...text.matchAll(/"(\/[A-Za-z0-9_\-/.]*)[?"]/g)].map((x) => x[1]));
        for (const h of text.matchAll(/paths::([a-z0-9_]+)\(\)/g)) if (helpers.has(h[1])) ps.add(helpers.get(h[1]));
        out.push({ id: m[0], file: file.split(path.sep).join("/"), line: i + 1, name, paths: [...ps], unit: file.startsWith("src") });
      }
    });
  }
  return out;
}

/** The last `cargo offrails verify` run, from target/verify/: per suite, when, the summary, and
 *  the failed test names. */
function verifyRuns() {
  const runs = [];
  for (const [label, files] of [
    ["SQLite", ["tests-sqlite-coverage.log", "tests-sqlite.log"]],
    ["Postgres", ["tests-postgres.log"]],
  ]) {
    const found = files
      .map((f) => path.join("target", "verify", f))
      .filter((f) => fs.existsSync(f))
      .sort((a, b) => fs.statSync(b).mtimeMs - fs.statSync(a).mtimeMs)[0];
    if (!found) continue;
    const text = fs.readFileSync(found, "utf8");
    const summary = text.split(/\r?\n/).reverse().find((l) => l.trim().startsWith("Summary ["));
    const failed = [...text.matchAll(/^\s*(?:FAIL|TIMEOUT) \[[^\]]*\]\s+(?:\S+\s+)?(\S+)\s*$/gm)].map((m) => m[1]);
    runs.push({
      label,
      file: found.split(path.sep).join("/"),
      when: new Date(fs.statSync(found).mtimeMs),
      summary: summary ? summary.replace(/^.*\]\s*/, "").trim() : null,
      failed,
    });
  }
  return runs;
}

function testStatus(c, runs) {
  if (runs.length === 0) return "no verify run yet";
  const parts = [];
  for (const r of runs) {
    if (r.label === "Postgres" && c.unit) continue; // unit tests in src/ run on SQLite only
    if (!r.summary) parts.push(`${r.label}: incomplete run`);
    else if (r.failed.some((f) => f === c.name || f.endsWith(`::${c.name}`))) parts.push(`${r.label}: FAILED`);
    else parts.push(`${r.label}: passed`);
  }
  return parts.join(", ");
}

function appName() {
  try {
    const m = fs.readFileSync("brand.toml", "utf8").match(/^name\s*=\s*"([^"]*)"/m);
    if (m) return m[1];
  } catch {
    // no brand.toml: fall through
  }
  try {
    const m = fs.readFileSync("AGENTS.md", "utf8").match(/^APP:\s*(\S+)/m);
    if (m) return m[1];
  } catch {
    // not an app root
  }
  return path.basename(process.cwd());
}

function git(args) {
  try {
    return execFileSync("git", args, { encoding: "utf8", stdio: ["ignore", "pipe", "ignore"] }).trim();
  } catch {
    return "";
  }
}

const slug = (p) => (p === "/" ? "home" : p.replace(/^\//, "").replace(/[^A-Za-z0-9]+/g, "-").replace(/^-|-$/g, "")) || "page";

// ---- the run ----------------------------------------------------------------------------------

/** Signs in through the real form (a password field's form), as a person would. */
async function signIn(tab) {
  await tab.viewport(VIEWPORTS[0], "light");
  await tab.goto(`${BASE}${LOGIN}`);
  const filled = await tab.eval((email, password) => {
    const pw = document.querySelector("form input[type=password]");
    if (!pw) return "no password field";
    const em = pw.form.querySelector("input[type=email], input[name=email]");
    if (!em) return "no email field";
    em.value = email;
    pw.value = password;
    pw.form.requestSubmit();
    return "ok";
  }, EMAIL, PASSWORD);
  if (filled !== "ok") return `${LOGIN} has ${filled}`;
  for (let i = 0; i < 100; i += 1) {
    await sleep(100);
    const at = await tab.eval(() => (document.readyState === "complete" ? location.pathname : null)).catch(() => null);
    if (at && at !== LOGIN) break;
  }
  await tab.settle();
  const still = await tab.eval(() => !!document.querySelector("form input[type=password]") && location.pathname).catch(() => null);
  tab.errors.length = 0;
  return still ? `still at ${still} after submitting: seed the account with \`cargo run -- task auth_demo\`, or set SMOKE_EMAIL and SMOKE_PASSWORD` : null;
}

async function provePage(tab, page, axe, results) {
  const shots = {};
  const failures = [];
  // Kernel pages (/dev, /admin/jobs…: no hx-boost on <body>) come from the framework and its parts:
  // their sideways scroll is reported, and fails only with --strict (fix it upstream).
  const warnings = [];
  let links = [];
  for (const vp of VIEWPORTS) {
    for (const scheme of SCHEMES) {
      const key = `${vp.name}-${scheme}`;
      await tab.viewport(vp, scheme);
      const before = tab.errors.length;
      try {
        await tab.goto(`${BASE}${page.path}`);
      } catch (e) {
        failures.push(`${key}: does not load (${e.message.split("\n")[0]})`);
        continue;
      }
      const at = await tab.eval(() => location.pathname + location.search).catch(() => page.path);
      if (at !== page.path) failures.push(`${key}: moved to ${at} on load`);
      if (vp.mobile) {
        const o = await tab.eval(overflow).catch(() => null);
        if (o) {
          const kernel = !(await tab.eval(() => document.body?.getAttribute("hx-boost") === "true").catch(() => false));
          const msg = `${key}: scrolls sideways (${o.scrollWidth}px wide in a ${o.width}px viewport): ${o.culprits.join(", ") || "no single element found"}`;
          if (kernel && !STRICT) warnings.push(`${msg} [a kernel page: reported, fails with --strict]`);
          else failures.push(msg);
        }
      }
      const { png, cut } = await tab.screenshot(MAX_HEIGHT);
      const name = `${slug(page.path)}${page.who === "signed in" ? "--signed-in" : ""}--${key}.png`;
      shots[key] = { name, cut };
      const file = path.join(OUT, "screens", name);
      if (fs.existsSync(file)) {
        try {
          const d = diffPng(decodePng(fs.readFileSync(file)), decodePng(png));
          shots[key].diff = d.percent;
          shots[key].resized = d.resized;
          if (d.percent > THRESHOLD) {
            fs.mkdirSync(path.join(OUT, "diff"), { recursive: true });
            fs.writeFileSync(path.join(OUT, "diff", name), encodePng(d.image));
            shots[key].diffImage = `diff/${name}`;
          }
        } catch (e) {
          shots[key].diffError = e.message;
        }
      }
      fs.writeFileSync(file, png);
      if (!vp.mobile && scheme === "light") {
        const l = await tab.eval(links).catch(() => ({ links: [], missing: [] }));
        links = l.links;
        for (const m of l.missing) failures.push(`links to ${m}, and no element has that id`);
      }
      if (!vp.mobile && axe.src) {
        try {
          const v = await runAxe(tab, axe.src);
          for (const x of v) failures.push(`axe ${scheme}: ${x.id} [${x.impact}] × ${x.count}: ${x.nodes.join(" | ")}`);
        } catch (e) {
          failures.push(`axe ${scheme}: ${e.message.split("\n")[0]}`);
        }
      }
      for (const err of tab.errors.slice(before)) failures.push(`${key}: console: ${err}`);
    }
  }
  results.push({ ...page, shots, failures, warnings, links });
  const changed = Object.values(shots).filter((s) => (s.diff ?? 0) > THRESHOLD).length;
  console.log(`${failures.length ? "FAIL" : "ok  "} ${page.path} (${page.who})${failures.length ? `: ${failures.length} problem(s)` : ""}${changed ? `, ${changed} screenshot(s) changed` : ""}`);
  for (const f of failures) console.log(`       ${f}`);
  for (const w of warnings) console.log(`  warn ${page.path}: ${w}`);
}

async function main() {
  if (!BASE) {
    console.error("usage: BASE=<the URL bin/dev printed> cargo offrails prove (from the app root; it starts nothing)");
    process.exit(2);
  }
  const chrome = requireChrome();
  const all = routes();
  const skip = process.env.SMOKE_SKIP !== undefined ? patterns(process.env.SMOKE_SKIP) : ["/dev/lab*"];
  const linkSkip = [...skip, ...patterns(process.env.PROVE_LINK_SKIP)];
  const paths = [...new Set(all.filter((r) => r.method === "GET").map((r) => r.path))].filter((p) => !/[{}:*]/.test(p) && !matches(skip, p));
  for (const p of patterns(process.env.SMOKE_PATHS)) if (!paths.includes(p)) paths.push(p);
  const notCovered = all
    .filter((r) => r.method === "GET" && /[{}:*]/.test(r.path) && !patterns(process.env.SMOKE_PATHS).some((p) => p.split("/").length === r.path.split("/").length))
    .map((r) => ({ path: r.path, why: "has a parameter (add a concrete path to SMOKE_PATHS)" }));
  for (const p of all.filter((r) => r.method === "GET" && matches(skip, r.path))) notCovered.push({ path: p.path, why: "SMOKE_SKIP" });
  const passwordSignIn = all.some((r) => r.method === "GET" && r.path === LOGIN) && signInPost(all) !== null;

  fs.mkdirSync(path.join(OUT, "screens"), { recursive: true });
  fs.rmSync(path.join(OUT, "diff"), { recursive: true, force: true });
  fs.writeFileSync(path.join(OUT, ".gitignore"), COMMIT_SCREENSHOTS
    ? "# cargo offrails prove --commit-screenshots: screenshots are kept in git; diffs are not.\ndiff/\n"
    : "# cargo offrails prove: screenshots and diffs stay local (--commit-screenshots keeps screens/ in git).\nscreens/\ndiff/\n");

  const axe = axeSource();
  if (axe.skip) console.log(`prove: axe skipped: ${axe.skip}`);

  const results = [];
  const skipped = [];
  const broken = [];
  let browser = "";
  let signInProblem = null;
  let code = 0;
  let close = null;
  try {
    const launched = await launch(chrome, {
      prefix: "offrails-prove-",
      args: ["--hide-scrollbars", "--force-color-profile=srgb", "--font-render-hinting=none", "--window-size=1280,800"],
    });
    close = launched.close;
    const cdp = launched.cdp;
    browser = launched.version.product;
    console.log(`prove: ${browser} against ${BASE}, ${paths.length} GET routes without parameters`);

    // Signed out: every page that answers; the ones that send you to sign in wait for the next tab.
    const out = await Tab.open(cdp, "signed out", { ignore: IGNORE });
    await out.viewport(VIEWPORTS[0], "light");
    await out.goto(`${BASE}/`);
    const pages = [];
    const later = [];
    const consider = async (tab, who, p, list) => {
      const c = await tab.eval(classify, p);
      if (c.kind === "page") list.push({ path: p, who });
      else if (c.kind === "redirect" && c.at.split("?")[0] === LOGIN && who === "signed out") later.push(p);
      else if (c.kind === "redirect") {
        if (c.at.split("?")[0] === LOGIN) skipped.push({ path: p, why: `${who}: sends you to sign in` });
        else if (!paths.includes(c.at) && !list.some((x) => x.path === c.at)) list.push({ path: c.at, who, from: p });
        else skipped.push({ path: p, why: `redirects to ${c.at}, proven as its own page` });
      } else if (c.kind === "status" && c.status >= 500) list.push({ path: p, who, status: c.status });
      else if (c.kind === "status") skipped.push({ path: p, why: `answers ${c.status} ${who}` });
      else if (c.kind === "fragment") skipped.push({ path: p, why: "an htmx fragment, not a page" });
      else if (c.kind === "not-html") skipped.push({ path: p, why: `not a page (${c.type || "no content type"})` });
      else if (c.kind === "elsewhere") skipped.push({ path: p, why: "redirects off the site" });
      else skipped.push({ path: p, why: `no answer (${c.error})` });
    };
    for (const p of paths) await consider(out, "signed out", p, pages);
    for (const page of pages) {
      if (page.status) {
        results.push({ ...page, shots: {}, failures: [`answers ${page.status}`], warnings: [], links: [] });
        console.log(`FAIL ${page.path}: answers ${page.status}`);
      } else await provePage(out, page, axe, results);
    }
    if (later.length) {
      if (!passwordSignIn) {
        for (const p of later) skipped.push({ path: p, why: "needs sign-in, and the app has no password sign-in to use" });
      } else {
        const inn = await Tab.open(cdp, "signed in", { ignore: IGNORE });
        signInProblem = await signIn(inn);
        if (signInProblem) {
          console.log(`FAIL sign in as ${EMAIL}: ${signInProblem}`);
          for (const p of later) skipped.push({ path: p, why: "needs sign-in, and signing in failed" });
        } else {
          console.log(`ok   signed in as ${EMAIL} through ${LOGIN}`);
          const mine = [];
          for (const p of later) await consider(inn, "signed in", p, mine);
          for (const page of mine) await provePage(inn, page, axe, results);
          out.signedIn = inn;
        }
      }
    }

    // Links: every same-site link on the proven pages that isn't one of them, fetched once.
    const proven = new Set(results.map((r) => r.path));
    const linkers = new Map();
    for (const r of results) for (const l of r.links) {
      if (!linkers.has(l)) linkers.set(l, []);
      linkers.get(l).push(r.path);
    }
    const fetcher = out.signedIn ?? out;
    for (const [l, from] of linkers) {
      const bare = l.split("?")[0];
      if (proven.has(l) || matches(linkSkip, bare)) continue;
      const status = await fetcher.eval(fetchStatus, l);
      if (typeof status !== "number" || status >= 400) broken.push({ link: l, status, from: [...new Set(from)] });
    }
    for (const b of broken) console.log(`FAIL link ${b.link} answers ${b.status} (on ${b.from.join(", ")})`);
  } catch (e) {
    console.log(`FAIL ${e.message}`);
    code = 1;
  } finally {
    await close?.();
  }

  const failedPages = results.filter((r) => r.failures.length);
  const warnedCount = results.filter((r) => r.warnings.length).length;
  const changed = results.flatMap((r) => Object.values(r.shots).filter((s) => (s.diff ?? 0) > THRESHOLD));
  const failed = code !== 0 || failedPages.length > 0 || broken.length > 0 || Boolean(signInProblem) || (STRICT && changed.length > 0);
  writeProof({ results, skipped, notCovered, broken, browser, axe, signInProblem, failed, changed });
  const seconds = ((Date.now() - started) / 1000).toFixed(1);
  console.log(`\nprove: ${results.length} page(s), ${results.reduce((n, r) => n + Object.keys(r.shots).length, 0)} screenshots, ${failedPages.length} page(s) with problems, ${broken.length} broken link(s), ${changed.length} visual change(s), ${warnedCount} kernel page(s) with warnings in ${seconds} s`);
  console.log(`prove: ${failed ? "FAILED" : "proven"}: ${path.join(OUT, "PROOF.md")}`);
  process.exit(failed ? 1 : 0);
}

// ---- PROOF.md ---------------------------------------------------------------------------------

function writeProof({ results, skipped, notCovered, broken, browser, axe, signInProblem, failed, changed }) {
  const now = new Date();
  const commit = git(["rev-parse", "--short", "HEAD"]);
  const dirty = git(["status", "--porcelain"]) ? " (with uncommitted changes)" : "";
  const runs = verifyRuns();
  const acs = criteria();
  const cites = citations(pathHelpers());
  const byPath = new Map(results.map((r) => [r.path, r]));
  const shotLinks = (r) => {
    const l = (k) => (r.shots[k] ? `[${k.split("-")[1]}](screens/${r.shots[k].name})` : "—");
    return { desktop: `${l("desktop-light")} · ${l("desktop-dark")}`, mobile: `${l("mobile-light")} · ${l("mobile-dark")}` };
  };
  const cell = (s) => String(s).replace(/\|/g, "\\|").replace(/\n/g, " ");
  const lines = [];
  lines.push(`# Proof: ${appName()}`, "");
  lines.push(`> Generated by \`cargo offrails prove\` on ${now.toISOString().replace(/\.\d+Z$/, "Z")} against ${BASE}${commit ? `, commit ${commit}${dirty}` : ""}. ${browser}. Don't edit it: re-run prove.`, "");
  lines.push(`**Result: ${failed ? "FAILED" : "PROVEN"}.** ${results.length} page(s), ${results.reduce((n, r) => n + Object.keys(r.shots).length, 0)} screenshots (desktop 1280 and mobile 390, light and dark), ${results.filter((r) => r.failures.length).length} page(s) with problems, ${broken.length} broken link(s), ${changed.length} screenshot(s) changed more than ${THRESHOLD}% since the last run${STRICT ? " (--strict: a failure)" : " (reported, not a failure)"}.`, "");

  lines.push("## Checks", "");
  lines.push("| Check | Result |", "|-------|--------|");
  const count = (pred) => results.filter((r) => r.failures.some(pred)).length;
  lines.push(`| Console errors, uncaught exceptions, CSP violations | ${count((f) => f.includes("console:")) ? `FAIL on ${count((f) => f.includes("console:"))} page(s)` : "ok, every load"} |`);
  lines.push(`| axe-core (WCAG 2.2 AA), light and dark | ${axe.skip ? `skipped: ${cell(axe.skip)}` : count((f) => f.startsWith("axe")) ? `FAIL on ${count((f) => f.startsWith("axe"))} page(s)` : "ok, 0 violations"} |`);
  const warned = results.filter((r) => r.warnings.length).length;
  lines.push(`| No horizontal scroll at 390px | ${count((f) => f.includes("scrolls sideways")) ? `FAIL on ${count((f) => f.includes("scrolls sideways"))} page(s)` : "ok"}${warned ? `; ${warned} kernel page(s) warned (see Warnings)` : ""} |`);
  lines.push(`| Internal links and #anchors resolve | ${broken.length || count((f) => f.startsWith("links to")) ? `FAIL: ${broken.length} link(s), ${count((f) => f.startsWith("links to"))} page(s) with a missing anchor` : "ok"} |`);
  lines.push(`| Sign-in for pages that need it | ${signInProblem ? `FAIL: ${cell(signInProblem)}` : results.some((r) => r.who === "signed in") ? `ok, as ${EMAIL}` : "not needed"} |`);
  lines.push(`| Visual changes since the last run | ${changed.length ? `${changed.length} over ${THRESHOLD}% (see diff/)` : "none over the threshold"} |`);
  lines.push("");

  lines.push("## Pages", "");
  lines.push("| Page | As | Result | Desktop | Mobile | Changed |", "|------|----|--------|---------|--------|---------|");
  for (const r of results) {
    const s = shotLinks(r);
    const diffs = Object.entries(r.shots)
      .filter(([, x]) => x.diff !== undefined)
      .map(([k, x]) => (x.diff > THRESHOLD ? `[${k} ${x.diff.toFixed(1)}%](${x.diffImage})` : null))
      .filter(Boolean);
    const first = Object.values(r.shots).every((x) => x.diff === undefined);
    lines.push(`| \`${r.path}\`${r.from ? ` (from \`${r.from}\`)` : ""} | ${r.who} | ${r.failures.length ? `FAIL (${r.failures.length})` : "ok"} | ${s.desktop} | ${s.mobile} | ${first ? "first run" : diffs.length ? diffs.join(", ") : "no"} |`);
  }
  lines.push("");

  lines.push("## Acceptance criteria", "");
  if (acs.length === 0) {
    lines.push("No acceptance criteria yet: `product/features/*.md` lists none (`- [ ] AC-<feature>-<n>: …`, cited by a test with `/// AC-<feature>-<n>`).", "");
  } else {
    lines.push(`From \`product/features/*.md\`: each criterion, the tests that cite it, their result in the last \`cargo offrails verify\` (${runs.length ? runs.map((r) => `${r.label} ${r.when.toISOString().replace(/\.\d+Z$/, "Z")}${r.summary ? `: ${r.summary}` : ""}`).join("; ") : "none yet: run it, then prove again"}), and the pages they request with their screenshots.`, "");
    let file = "";
    for (const ac of acs) {
      if (ac.file !== file) {
        file = ac.file;
        lines.push(`### ${file}`, "", "| Criterion | Tests | Last verify | Pages |", "|-----------|-------|-------------|-------|");
      }
      const mine = cites.filter((c) => c.id === ac.id);
      const tests = mine.length ? mine.map((c) => `\`${c.name}\` (${c.file}:${c.line})`).join("<br>") : ac.struck ? "dropped" : "**none: not proven**";
      const status = mine.length ? [...new Set(mine.map((c) => testStatus(c, runs)))].join("<br>") : "—";
      const touched = [...new Set([...ac.paths, ...mine.flatMap((c) => c.paths)])];
      const pages = touched.map((p) => {
        const r = byPath.get(p);
        if (!r) return `\`${p}\` (not proven here)`;
        const s = shotLinks(r);
        return `\`${p}\` ${r.failures.length ? "FAIL" : "ok"}: ${s.desktop}, mobile ${s.mobile}`;
      });
      lines.push(`| ${ac.struck ? "~~" : ""}**${ac.id}**${ac.struck ? "~~" : ""} ${ac.ticked ? "[x]" : "[ ]"} ${cell(ac.text)} | ${tests} | ${status} | ${pages.length ? pages.join("<br>") : "no page (a model or job rule)"} |`);
    }
    lines.push("");
  }

  const problems = results.filter((r) => r.failures.length);
  if (problems.length || broken.length || signInProblem) {
    lines.push("## Problems", "");
    if (signInProblem) lines.push(`- Sign-in as ${EMAIL}: ${signInProblem}`);
    for (const r of problems) for (const f of r.failures) lines.push(`- \`${r.path}\` (${r.who}): ${f}`);
    for (const b of broken) lines.push(`- link \`${b.link}\` answers ${b.status}, on ${b.from.map((p) => `\`${p}\``).join(", ")}`);
    lines.push("");
  }

  const warnedPages = results.filter((r) => r.warnings.length);
  if (warnedPages.length) {
    lines.push("## Warnings", "", "Kernel pages (the framework's and its parts', not this app's templates): reported, failing only with `--strict`.", "");
    for (const r of warnedPages) for (const w of r.warnings) lines.push(`- \`${r.path}\` (${r.who}): ${w}`);
    lines.push("");
  }

  if (skipped.length || notCovered.length) {
    lines.push("## Not proven here", "");
    for (const s of [...notCovered, ...skipped]) lines.push(`- \`${s.path}\`: ${s.why}`);
    lines.push("");
  }

  lines.push("## Re-run", "");
  lines.push("```sh", `BASE=${BASE} cargo offrails prove            # --strict fails on visual changes; --commit-screenshots keeps the PNGs in git`, "```", "");
  lines.push(`Screenshots: \`${OUT.split(path.sep).join("/")}/screens/\`${COMMIT_SCREENSHOTS ? " (committed)" : " (git-ignored; --commit-screenshots keeps them)"}. Diffs against the previous run: \`diff/\`. Machine-readable: \`proof.json\`.`);
  fs.writeFileSync(path.join(OUT, "PROOF.md"), `${lines.join("\n")}\n`);
  fs.writeFileSync(path.join(OUT, "proof.json"), `${JSON.stringify({
    generated: now.toISOString(), base: BASE, commit, browser, failed, threshold: THRESHOLD,
    pages: results.map((r) => ({ path: r.path, who: r.who, from: r.from, failures: r.failures, warnings: r.warnings, shots: r.shots })),
    broken, skipped: [...notCovered, ...skipped],
    criteria: acs.map((ac) => ({ ...ac, tests: cites.filter((c) => c.id === ac.id).map((c) => ({ name: c.name, file: c.file, line: c.line, status: testStatus(c, runs), paths: c.paths })) })),
  }, null, 2)}\n`);
}

if (process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  await main();
}
