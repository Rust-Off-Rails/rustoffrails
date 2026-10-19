// Shared by the browser smokes. The rule this file enforces: Chrome runs with NO certificate
// overrides (no --ignore-certificate-errors, no acceptInsecureCerts). On a self-signed certificate
// Chrome drops Set-Cookie even after "Proceed to localhost (unsafe)", so login "works" on the
// server and bounces in the browser. A smoke that ignores certificate errors can't see that.
import puppeteer from "puppeteer-core";
import fs from "node:fs";
import { createRequire } from "node:module";
import os from "node:os";
import path from "node:path";
import { pathToFileURL } from "node:url";
import { findChrome, requireChrome } from "./cdp.mjs";

export const BASE = (process.env.BASE ?? "http://localhost:3000").replace(/\/$/, "");
// The same discovery as the CDP smokes (cdp.mjs): CHROME_PATH or CHROME, else the usual places on
// macOS, Linux and Windows. It used to default to the macOS path, so on Linux CI every smoke run
// through run.sh failed to launch unless CHROME was set.
export const CHROME = findChrome();

export function checker() {
  const failures = [];
  const check = (ok, what) => {
    console.log(`${ok ? "ok  " : "FAIL"} ${what}`);
    if (!ok) failures.push(what);
    return ok;
  };
  return { failures, check };
}

export async function launch() {
  const profile = fs.mkdtempSync(path.join(os.tmpdir(), "offrails-smoke-"));
  return puppeteer.launch({
    executablePath: requireChrome(),
    headless: true,
    userDataDir: profile,
    args: ["--no-first-run", "--no-default-browser-check"],
  });
}

export async function scenario({ required = [] } = {}) {
  if (!process.env.SCENARIO) return {};
  const sc = await import(pathToFileURL(path.resolve(process.env.SCENARIO)).href);
  const missing = required.filter((k) => typeof sc[k] !== "function");
  if (missing.length) throw new Error(`${process.env.SCENARIO} must export ${missing.join(", ")}`);
  return sc;
}

/** Opens BASE the way a person does. Fails loudly on a certificate the browser doesn't trust. */
export async function preflight(page, check) {
  try {
    await page.goto(`${BASE}/`, { waitUntil: "load" });
  } catch (e) {
    if (/ERR_CERT|ERR_SSL|SSL_PROTOCOL/.test(String(e.message))) {
      check(false, `${BASE} serves HTTPS with a certificate this browser does not trust (${e.message.split(" at ")[0]})`);
      console.log("     Chrome drops Set-Cookie on untrusted origins even after \"Proceed (unsafe)\": login would bounce.");
      console.log("     Use a locally trusted certificate (mkcert + 'mkcert -install', then bin/dev-cert) or plain http");
      console.log("     (bin/dev's fallback). Never smoke with --ignore-certificate-errors. (guides/dev-tools.md#https-in-development)");
      return false;
    }
    if (/ERR_INVALID_HTTP_RESPONSE|ERR_EMPTY_RESPONSE/.test(String(e.message)) && BASE.startsWith("http://")) {
      check(false, `${BASE}: the server speaks TLS on this port; use https:// (bin/dev prints the exact URL)`);
      return false;
    }
    throw e;
  }
  const proto = await page.evaluate(() => performance.getEntriesByType("navigation")[0]?.nextHopProtocol);
  check(true, `${BASE} loads with no certificate override (${proto})`);
  return true;
}

/**
 * Logs in through the scenario's own `login(page, base)` (fill and submit the real form), then
 * proves the browser kept the session: a fresh navigation must still be logged in.
 */
export async function loginAndProve(page, sc, check) {
  await sc.login(page, BASE);
  const names = (await page.cookies()).map((c) => c.name);
  check(names.length > 0, `the browser stored cookies after login (${names.join(", ") || "none"})`);
  if (typeof sc.loggedIn === "function") {
    await page.goto(`${BASE}/`, { waitUntil: "load" });
    check(await sc.loggedIn(page), "a fresh page load after login is still logged in (session cookie kept)");
  }
}

/** axe-core (WCAG 2.2 AA) on the current page. Injected through CDP, so the strict CSP stays on. */
export async function axe(page) {
  // Resolved the way the puppeteer-core import is: up from this file to the nearest node_modules.
  // run.sh runs a copy from its own $cache/run.XXXXXX, whose parent (the cache) holds it.
  const src = fs.readFileSync(createRequire(import.meta.url).resolve("axe-core/axe.min.js"), "utf8");
  await page.evaluate(src);
  return page.evaluate(async () => {
    const r = await window.axe.run(document, {
      runOnly: { type: "tag", values: ["wcag2a", "wcag2aa", "wcag21a", "wcag21aa", "wcag22aa"] },
    });
    return r.violations.map((v) => ({
      id: v.id,
      impact: v.impact,
      nodes: v.nodes.slice(0, 5).map((n) => `${n.target.join(" ")}: ${(n.failureSummary || "").split("\n").slice(1).join(" ").trim()}`),
      count: v.nodes.length,
    }));
  });
}
