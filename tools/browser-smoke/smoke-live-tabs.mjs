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
//   export async function loggedIn(page) { return bool } // optional: proves the session cookie stuck
// Exits 0 when all tabs update within TIMEOUT_MS, 1 otherwise. No certificate overrides: an
// untrusted certificate fails the run (Chrome drops cookies on it; see lib.mjs).
import { BASE as base, checker, launch, loginAndProve, preflight, scenario } from "./lib.mjs";

const tabsN = Number(process.env.TABS ?? 12);
const timeout = Number(process.env.TIMEOUT_MS ?? 10000);
if (!process.env.SCENARIO) { console.error("set SCENARIO=<path to the app's scenario module>"); process.exit(2); }
const sc = await scenario({ required: ["mutate", "seen"] });
const t0 = Date.now();
const log = (...a) => console.log(`[${((Date.now() - t0) / 1000).toFixed(1)}s]`, ...a);

const browser = await launch();
const { failures, check } = checker();
let failed = false;
try {
  const first = await browser.newPage();
  if (!(await preflight(first, check))) throw new Error("preflight failed");
  if (sc.login) await loginAndProve(first, sc, check);
  if (failures.length) throw new Error("login did not stick");
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
      ? "FAIL: on HTTP/1.1 the browser allows ~6 connections per origin; serve HTTPS so it negotiates h2 (a trusted certificate: mkcert -install + bin/dev-cert, guides/realtime.md \"Many tabs stall\")."
      : "FAIL: some tabs never updated on a multiplexed connection: capture evidence (document.scripts, the EventSource state, requests) before changing code.");
  }
} catch (e) {
  failed = true;
  log(`FAIL: ${e.message}`);
} finally {
  await browser.close();
}
process.exit(failed ? 1 : 0);
