// Boosted-navigation smoke: every page in docs/routes.md must work the same after a boosted link
// as after a full load. The app layout boosts every link (`<body hx-boost="true">`): a boosted
// click swaps <body> and runs nothing in the next page's <head>, and DOMContentLoaded never fires
// again. So a page script loaded in a page's head block, or one that binds elements on
// DOMContentLoaded, works on a fresh load (and in request tests, and in screenshots) and is dead
// after a click. The real case: the sign-in page's password "Show" button.
//
//   BASE=<the URL bin/dev printed> node .offrails/tools/browser-smoke/smoke-boosted.mjs   # from the app root
//   cargo offrails verify --full                                   # runs it when BASE is set
//
// For each GET route in docs/routes.md without a parameter that answers 200 with an HTML page
// whose <body> is boosted (kernel pages are not, so they are skipped; a route that redirects to
// such a page, like /live to /live/lobby, smokes the page it lands on):
//   1. full load: record its live elements (below) and click each one: did the DOM or an
//      attribute change, or did it print, copy or open a dialog?
//   2. boosted: load /, follow a boosted link to the page (the page's own link when / has one,
//      else one added to the page and processed by htmx), and click the same elements: each one
//      that reacted on the full load must react again.
//   It fails on an element that is dead after the boosted link, on a script or stylesheet the page
//   loads in its own <head> (a boosted link never loads it), and on any console error, uncaught
//   exception or CSP violation on either path.
// With a password sign-in (GET /session/new, and a POST /session/password, /session, /login… route
// in docs/routes.md, or SMOKE_SIGNIN_PATH; the form found on the sign-in page is submitted) it runs
// every page twice: signed out, then signed in as the development demo account (`cargo run -- task
// auth_demo`; SMOKE_EMAIL and SMOKE_PASSWORD override it).
//
// Live elements (the convention, guides/frontend-htmx.md#boosted-navigation): every element page JS
// reacts to on click is a `<button type="button">`, or carries `data-js` (or `data-toggle`, or
// role="button"), and the JS binds by delegation on `document`. Elements that send a request
// themselves (hx-get, hx-post…), links, submit buttons and `data-confirm` are not clicked.
// `data-smoke-skip` on an element or a container leaves it out.
//
// Zero dependencies: Node >= 18 and an installed Chrome, Chromium or Edge, driven over the Chrome
// DevTools Protocol on a pipe (cdp.mjs, shared with prove.mjs). It never downloads a browser: CHROME_PATH (or CHROME) names one,
// else it looks in the usual places on macOS, Linux and Windows. No browser: it prints
// "skip: no Chrome" and exits 77, which `verify` and the CI scripts report as skipped, never passed.
// Like the other smokes it launches Chrome with a throwaway profile and NO certificate overrides.
//
// Environment: BASE (required), ROUTES (default docs/routes.md), SMOKE_SKIP (comma-separated
// paths; a trailing * matches a prefix; default /dev/lab*: the load lab is slow and fails on
// purpose), SMOKE_ONLY (the same syntax: only these), SMOKE_PATHS (comma-separated pages to add,
// for routes with parameters: /posts/1,/posts/1/edit), SMOKE_EMAIL, SMOKE_PASSWORD, SMOKE_LOGIN
// (default /session/new), SMOKE_SIGNIN_PATH (the sign-in form's POST route when it isn't
// /session/password, /session, /sessions, /login, /sign_in or /users/sign_in), SMOKE_IGNORE (a regular expression of console messages to allow),
// HEADED=1 (a visible window). Exit status: 0 green, 1 a failure, 2 usage, 77 no browser.
import fs from "node:fs";
import path from "node:path";

import { Tab, launch, requireChrome, signInPost, sleep } from "./cdp.mjs";

const BASE = (process.env.BASE ?? "").replace(/\/$/, "");
const ROUTES = process.env.ROUTES ?? path.join("docs", "routes.md");
const LOGIN = process.env.SMOKE_LOGIN ?? "/session/new";
const EMAIL = process.env.SMOKE_EMAIL ?? "demo@example.com";
const PASSWORD = process.env.SMOKE_PASSWORD ?? "not-a-real-password-42";
const IGNORE = process.env.SMOKE_IGNORE ? new RegExp(process.env.SMOKE_IGNORE) : null;
const QUIET_MS = 150; // the window before and after each click

const started = Date.now();
let failures = 0;
const check = (ok, what) => {
  if (!ok) failures += 1;
  console.log(`${ok ? "ok  " : "FAIL"} ${what}`);
  return ok;
};
const note = (what) => console.log(`skip ${what}`);

// ---- the browser: cdp.mjs (findChrome, launch, Cdp, Tab), shared with prove.mjs ----------------

/** A tab at the smoke's window size, its console errors prefixed with what it was doing. */
async function openTab(cdp, label) {
  const tab = await Tab.open(cdp, label, { ignore: IGNORE, viewport: { width: 1280, height: 900 } });
  tab.phase = "start";
  return tab;
}

// ---- in the page ------------------------------------------------------------------------------

/** Counts DOM mutations and side effects (print, open, dialogs, clipboard) from here on. */
function instrument() {
  if (window.__smoke) return;
  const s = { mutations: 0, effects: 0, els: [] };
  window.__smoke = s;
  new MutationObserver((list) => { s.mutations += list.length; }).observe(document.documentElement, {
    subtree: true, childList: true, attributes: true, characterData: true,
  });
  const effect = (value) => () => { s.effects += 1; return value; };
  window.print = effect(undefined);
  window.open = effect(null);
  window.alert = effect(undefined);
  window.confirm = effect(false);
  window.prompt = effect(null);
  try {
    if (navigator.clipboard) {
      for (const m of ["writeText", "write"]) {
        Object.defineProperty(navigator.clipboard, m, { configurable: true, value: async () => { s.effects += 1; } });
      }
    }
  } catch {
    // no clipboard to stub
  }
}

/** The page's live elements (the convention in the header), remembered in order for click(). */
function discover() {
  const s = window.__smoke;
  const seen = new Map();
  s.els = [];
  const keys = [];
  const selector = 'button[type="button"], [data-js], [data-toggle], [role="button"]';
  for (const el of document.body.querySelectorAll(selector)) {
    if (el.closest("[data-smoke-skip]")) continue;
    if (el.matches("[hx-get], [hx-post], [hx-put], [hx-patch], [hx-delete], [data-confirm], [disabled]")) continue;
    if (el.matches("a[href]") && !el.getAttribute("href").startsWith("#")) continue;
    if (el.getClientRects().length === 0) continue; // not rendered
    const data = [...el.attributes]
      .filter((a) => a.name.startsWith("data-") || a.name === "role")
      .map((a) => (a.value ? `[${a.name}="${a.value}"]` : `[${a.name}]`))
      .join("");
    const label = (el.getAttribute("aria-label") || el.textContent || "").trim().replace(/\s+/g, " ").slice(0, 30);
    const base = `${el.tagName.toLowerCase()}${el.id ? `#${el.id}` : ""}${data} "${label}"`;
    const n = seen.get(base) ?? 0;
    seen.set(base, n + 1);
    s.els.push(el);
    keys.push(n ? `${base} (${n + 1})` : base);
  }
  return keys;
}

/** Clicks live element `i`: quiet before (else the page is busy on its own), reacted after. */
async function clickOne(i, quiet) {
  const s = window.__smoke;
  const el = s.els[i];
  if (!el || !el.isConnected) return { gone: true };
  const where = location.href;
  const m0 = s.mutations;
  await new Promise((r) => setTimeout(r, quiet));
  const busy = s.mutations !== m0;
  const m1 = s.mutations;
  const e1 = s.effects;
  el.click();
  await new Promise((r) => setTimeout(r, quiet));
  return { busy, reacted: s.mutations > m1 || s.effects > e1, moved: location.href !== where };
}

/** Script and stylesheet paths in <head>. */
function headAssets() {
  const out = [];
  for (const el of document.head.querySelectorAll("script, link[rel~=stylesheet]")) {
    const url = el.src || el.href;
    out.push(url ? new URL(url).pathname : `inline <script> "${el.textContent.trim().slice(0, 40)}"`);
  }
  return out;
}

/** How the page answers a GET (redirects followed, no stream read): is it a boosted HTML page? */
async function classify(path) {
  const ctl = new AbortController();
  const timer = setTimeout(() => ctl.abort(), 8000);
  try {
    const res = await fetch(path, { headers: { accept: "text/html" }, signal: ctl.signal });
    const type = (res.headers.get("content-type") || "").split(";")[0];
    const at = new URL(res.url).pathname;
    if (type !== "text/html") return { kind: "not-html", type };
    if (res.status !== 200) return { kind: "status", status: res.status };
    if (new URL(res.url).origin !== location.origin) return { kind: "elsewhere" };
    const html = await res.text();
    if (!/<body[^>]*\shx-boost=["']?true/i.test(html)) return { kind: "not-boosted" };
    return at === path ? { kind: "page" } : { kind: "redirect", at };
  } catch (e) {
    return { kind: "error", error: String(e.name || e) };
  } finally {
    clearTimeout(timer);
    ctl.abort();
  }
}

/** Follows a boosted link to `path` from the current page; true when it arrived by a swap. */
async function boost(path) {
  window.__smokeMark = true;
  const boosted = (a) => a.closest("[hx-boost]")?.getAttribute("hx-boost") === "true" && !a.target && !a.hasAttribute("download");
  let a = [...document.querySelectorAll("a[href]")].find((x) => {
    const u = new URL(x.href, location.href);
    return u.origin === location.origin && u.pathname === path && !u.search && !u.hash && boosted(x) && x.getClientRects().length;
  });
  const injected = !a;
  if (injected) {
    a = document.createElement("a");
    a.href = path;
    a.textContent = "smoke";
    (document.querySelector("main") || document.body).append(a);
    window.htmx?.process(a);
  }
  const settled = new Promise((r) => {
    document.addEventListener("htmx:afterSettle", () => r(true), { once: true });
    setTimeout(() => r(false), 10000);
  });
  a.click();
  await settled;
  for (let i = 0; i < 100 && location.pathname !== path; i += 1) await new Promise((r) => setTimeout(r, 50));
  return { injected, swapped: window.__smokeMark === true, at: location.pathname };
}

// ---- the run ----------------------------------------------------------------------------------

function routes() {
  let text;
  try {
    text = fs.readFileSync(ROUTES, "utf8");
  } catch {
    console.error(`smoke-boosted: no ${ROUTES} (run it from the app root, or set ROUTES)`);
    process.exit(2);
  }
  const rows = [];
  for (const line of text.split(/\r?\n/)) {
    const m = line.match(/^\|\s*([A-Z]+)\s*\|\s*`([^`]+)`\s*\|/);
    if (m) rows.push({ method: m[1], path: m[2] });
  }
  return rows;
}

const patterns = (v) => (v ?? "").split(",").map((s) => s.trim()).filter(Boolean);
const matches = (list, p) => list.some((x) => (x.endsWith("*") ? p.startsWith(x.slice(0, -1)) : p === x));

/** Signs in through the real form (a password field's form), as a person would. */
async function signIn(tab) {
  tab.phase = `sign in (${LOGIN})`;
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
  if (filled !== "ok") return check(false, `sign in: ${LOGIN} has ${filled}`);
  for (let i = 0; i < 100; i += 1) {
    await sleep(100);
    const at = await tab.eval(() => (document.readyState === "complete" ? location.pathname : null)).catch(() => null);
    if (at && at !== LOGIN) break;
  }
  await tab.settle();
  const still = await tab.eval(() => !!document.querySelector("form input[type=password]") && location.pathname).catch(() => null);
  return check(!still, still
    ? `sign in as ${EMAIL} failed (still at ${still}): seed it with \`cargo run -- task auth_demo\` (development), or set SMOKE_EMAIL and SMOKE_PASSWORD`
    : `signed in as ${EMAIL} through ${LOGIN}`);
}

/** Clicks every live element in order, re-arriving (with `arrive`) after one that navigates. */
async function exercise(tab, arrive) {
  const results = new Map();
  let keys = await tab.eval(discover);
  for (let i = 0; i < keys.length; i += 1) {
    let r;
    try {
      r = await tab.eval(clickOne, i, QUIET_MS);
    } catch {
      r = { moved: true }; // the click replaced the document
    }
    results.set(keys[i], r);
    if (r.moved) {
      await arrive();
      keys = await tab.eval(discover);
    }
  }
  return results;
}

async function smokePage(tab, p, who, layoutHead) {
  const label = `${p} (${who})`;
  // 1. Full load.
  tab.phase = `${label}, full load`;
  const errorsBefore = tab.errors.length;
  const full = async () => {
    await tab.goto(`${BASE}${p}`);
    await tab.eval(instrument);
  };
  await full();
  const fullAt = await tab.eval(() => location.pathname);
  if (fullAt !== p) return note(`${label}: the page moved to ${fullAt} on load`);
  const pageHead = await tab.eval(headAssets);
  const fullResults = await exercise(tab, full);

  // 2. The same page through a boosted link from /.
  tab.phase = `${label}, boosted link from /`;
  let how = null;
  const boosted = async () => {
    await tab.goto(`${BASE}/`);
    await tab.eval(instrument);
    how = await tab.eval(boost, p);
    await tab.settle();
  };
  await boosted();
  if (how.at !== p) return check(false, `${label}: a boosted link to it ended at ${how.at}`);
  if (!how.swapped) return note(`${label}: the boosted link arrived by a full page load (hx-boost="false", or HX-Refresh): nothing to compare`);
  const boostResults = await exercise(tab, boosted);

  // Scripts and stylesheets the page loads in its own <head> never load after a boosted link.
  const dropped = pageHead.filter((a) => !layoutHead.includes(a));
  check(dropped.length === 0, dropped.length
    ? `${label}: loads ${dropped.join(", ")} in its <head>, which a boosted link never runs: load it from the layout (samples: app head)`
    : `${label}: nothing page-specific in <head>`);

  let live = 0;
  for (const [key, f] of fullResults) {
    if (!f.reacted || f.busy || f.gone) continue;
    live += 1;
    const b = boostResults.get(key);
    if (!b || b.gone) check(false, `${label}: ${key} is not on the page after a boosted link`);
    else if (b.busy && !b.reacted) note(`${label}: ${key}: the page kept changing on its own after the boosted link; not compared`);
    else check(b.reacted, `${label}: ${key} ${b.reacted ? "works after a boosted link" : "reacts on a full load and is DEAD after a boosted link (a script in the page's <head>, or bound on DOMContentLoaded?)"}`);
  }
  if (live === 0) console.log(`ok   ${label}: no live elements; loads and boosts cleanly`);
  const errs = tab.errors.slice(errorsBefore);
  check(errs.length === 0, errs.length ? `${label}: console errors:\n       ${errs.join("\n       ")}` : `${label}: no console errors on either path`);
}

async function pass(cdp, who, paths, { login = false } = {}) {
  const tab = await openTab(cdp, who);
  if (login && !(await signIn(tab))) return;
  tab.phase = `${who}: /`;
  try {
    await tab.goto(`${BASE}/`);
  } catch (e) {
    check(false, `${BASE}/ does not load: ${e.message}`);
    return;
  }
  const layoutHead = await tab.eval(headAssets);
  // The pages to smoke: each route that is a boosted page, and the page a route redirects to
  // (/live → /live/lobby) once. Kernel pages (not boosted), JSON, streams and errors are not.
  const pages = [];
  const skipped = new Map();
  const skip = (why, p) => skipped.set(why, [...(skipped.get(why) ?? []), p]);
  for (const p of paths) {
    const c = await tab.eval(classify, p);
    if (c.kind === "page") {
      if (!pages.includes(p)) pages.push(p);
    } else if (c.kind === "redirect") {
      if (pages.includes(c.at) || paths.includes(c.at) || c.at === LOGIN) skip("redirect to a page smoked anyway", `${p} → ${c.at}`);
      else {
        pages.push(c.at);
        console.log(`     ${p} (${who}) redirects to ${c.at}: smoking that`);
      }
    } else if (c.kind === "status" && c.status >= 500) check(false, `${p} (${who}): answers ${c.status}`);
    else if (c.kind === "not-boosted") skip("not boosted (no hx-boost on <body>: kernel pages)", p);
    else if (c.kind === "status") skip("not 200", `${p} (${c.status})`);
    else if (c.kind === "error") skip("no answer", `${p} (${c.error})`);
    else if (c.kind === "elsewhere") skip("redirects off the site", p);
    // not-html (robots.txt, JSON, streams): not a page at all
  }
  for (const [why, list] of skipped) note(`${who}, ${list.length} ${why}: ${list.join(", ")}`);
  for (const p of pages) {
    try {
      await smokePage(tab, p, who, layoutHead);
    } catch (e) {
      check(false, `${p} (${who}): ${e.message.split("\n")[0]}`);
    }
  }
  return pages.length;
}

async function main() {
  if (!BASE) {
    console.error("usage: BASE=<the URL bin/dev printed> node .offrails/tools/browser-smoke/smoke-boosted.mjs (from the app root)");
    process.exit(2);
  }
  const chrome = requireChrome();
  const all = routes();
  const skip = process.env.SMOKE_SKIP !== undefined ? patterns(process.env.SMOKE_SKIP) : ["/dev/lab*"];
  const only = patterns(process.env.SMOKE_ONLY);
  const paths = [...new Set(all.filter((r) => r.method === "GET").map((r) => r.path))].filter(
    (p) => !/[{}:*]/.test(p) && !matches(skip, p) && (only.length === 0 || matches(only, p)),
  );
  for (const p of patterns(process.env.SMOKE_PATHS)) if (!paths.includes(p)) paths.push(p);
  const passwordSignIn = all.some((r) => r.method === "GET" && r.path === LOGIN) && signInPost(all) !== null;

  let code = 0;
  let close = null;
  try {
    const launched = await launch(chrome, { prefix: "offrails-boosted-", args: ["--window-size=1280,900"] });
    close = launched.close;
    const { cdp } = launched;
    console.log(`smoke-boosted: ${launched.version.product} against ${BASE}, ${paths.length} GET routes without parameters`);
    let pages = await pass(cdp, "signed out", paths);
    if (passwordSignIn) pages += (await pass(cdp, "signed in", paths, { login: true })) ?? 0;
    else if (all.some((r) => r.path === LOGIN)) note(`signed-in pages: no password sign-in (a POST /session/password, /session… route, or SMOKE_SIGNIN_PATH) to sign in with`);
    console.log(`\nsmoke-boosted: ${pages} page visits, ${failures} failure(s) in ${((Date.now() - started) / 1000).toFixed(1)} s`);
    code = failures ? 1 : 0;
  } catch (e) {
    console.log(`FAIL ${e.message}`);
    code = 1;
  } finally {
    await close?.();
  }
  process.exit(code);
}

await main();
