// Multi-tab live-update smoke: open N tabs on one live page in ONE browser profile (one shared
// connection pool, like a real user), change something from tab 1, and assert every tab updates.
// This is the test that catches "realtime breaks at ~6 tabs" (HTTP/1.1 allows ~6 connections per
// origin and each open stream pins one; HTTP/2 multiplexes). Request tests can't see it.
//
//   SCENARIO=tests/e2e/live-tabs.scenario.mjs BASE=https://localhost:3000 TABS=12 \
//     bash .offrails/tools/browser-smoke/run.sh live-tabs
//
// The scenario module (the app's own file) exports:
//   export const path = "/books/1";                      // the live page to open in every tab
//   export async function login(page, base) {}           // optional: sign in tab 1 (cookies are shared)
//   export async function mutate(page, base) { return token }  // change data from tab 1
//   export async function seen(page, token) { return bool }    // does this tab show the change?
// Exits 0 when all tabs update within TIMEOUT_MS, 1 otherwise. Self-signed dev certs are accepted.
import puppeteer from "puppeteer-core";
import path from "node:path";
import { pathToFileURL } from "node:url";

const base = (process.env.BASE ?? "http://localhost:3000").replace(/\/$/, "");
const tabsN = Number(process.env.TABS ?? 12);
const timeout = Number(process.env.TIMEOUT_MS ?? 10000);
const chrome = process.env.CHROME ?? "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome";
if (!process.env.SCENARIO) { console.error("set SCENARIO=<path to the app's scenario module>"); process.exit(2); }
const sc = await import(pathToFileURL(path.resolve(process.env.SCENARIO)).href);
const t0 = Date.now();
const log = (...a) => console.log(`[${((Date.now() - t0) / 1000).toFixed(1)}s]`, ...a);

const browser = await puppeteer.launch({
  executablePath: chrome,
  headless: "new",
  args: ["--ignore-certificate-errors", "--no-first-run", "--no-default-browser-check"],
});
let failed = false;
try {
  const first = await browser.newPage();
  await first.goto(base + "/", { waitUntil: "load" });
  if (sc.login) await sc.login(first, base);
  const tabs = [first];
  for (let i = 1; i < tabsN; i++) tabs.push(await browser.newPage());
  const opened = await Promise.all(
    tabs.map(async (p, i) => {
      const t = Date.now();
      try {
        await p.goto(base + sc.path, { waitUntil: "load", timeout: 15000 });
      } catch (e) {
        return { tab: i + 1, status: "STALLED", ms: Date.now() - t };
      }
      const proto = await p.evaluate(() => performance.getEntriesByType("navigation")[0]?.nextHopProtocol).catch(() => "?");
      return { tab: i + 1, status: "ok", ms: Date.now() - t, proto };
    }),
  );
  await new Promise((r) => setTimeout(r, 1500)); // let every stream connect
  const token = await sc.mutate(first, base);
  const start = Date.now();
  const seenAt = new Array(tabs.length).fill(null);
  while (Date.now() - start < timeout && seenAt.some((s) => s === null)) {
    await Promise.all(tabs.map(async (p, i) => {
      if (seenAt[i] === null && (await sc.seen(p, token).catch(() => false))) seenAt[i] = Date.now() - start;
    }));
    await new Promise((r) => setTimeout(r, 150));
  }
  const got = seenAt.filter((s) => s !== null).length;
  console.table(opened.map((o, i) => ({ ...o, updatedAfterMs: seenAt[i] ?? "never" })));
  log(`live update reached ${got}/${tabs.length} tabs (protocol: ${[...new Set(opened.map((o) => o.proto))].join(",")})`);
  if (got < tabs.length) {
    failed = true;
    const h1 = opened.some((o) => o.proto === "http/1.1");
    log(h1
      ? "FAIL: on HTTP/1.1 the browser allows ~6 connections per origin; serve HTTPS so it negotiates h2 (bin/dev-cert + APP_TLS_*, guides/realtime.md \"Many tabs stall\")."
      : "FAIL: some tabs never updated on a multiplexed connection: capture evidence (document.scripts, the EventSource state, requests) before changing code.");
  }
} finally {
  await browser.close();
}
process.exit(failed ? 1 : 0);
