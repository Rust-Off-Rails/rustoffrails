// The live room in a real browser: two people in two browser contexts see each other's messages,
// presence, typing and waves; a blocked WebSocket falls back to SSE, then to long polling; signing
// out ends the page's connection. Fails on any console error (a CSP violation is one).
//
//   BASE=http://localhost:3000 node tests/e2e/live_room.e2e.mjs     # against bin/dev (development)
//
// Uses puppeteer-core from the framework's browser-smoke cache (run
// `bash .offrails/tools/browser-smoke/run.sh` once to install it); nothing is added to the app.
// Node >= 22.12. CHROME=<path> on Linux.
import { createRequire } from "node:module";
import { homedir } from "node:os";
import { join } from "node:path";

const cache = join(process.env.XDG_CACHE_HOME ?? join(homedir(), ".cache"), "offrails", "browser-smoke");
const puppeteer = createRequire(join(cache, "package.json"))("puppeteer-core");
const BASE = (process.env.BASE ?? "http://localhost:3000").replace(/\/$/, "");
const CHROME = process.env.CHROME ?? "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome";
const WAIT = 10000;

let failures = 0;
const check = (ok, what) => {
  console.log(`${ok ? "ok  " : "FAIL"} ${what}`);
  if (!ok) failures += 1;
};

const browser = await puppeteer.launch({ executablePath: CHROME, headless: true, args: ["--no-first-run"] });
const errors = [];

async function person(name, { block = [] } = {}) {
  const context = await browser.createBrowserContext();
  const page = await context.newPage();
  page.on("console", (m) => {
    if (m.type() !== "error") return;
    // The requests this test blocks on purpose fail loudly; anything else is a real error.
    if (block.includes("sse") && (m.location()?.url ?? "").includes("/socket/sse")) return;
    errors.push(`${name}: ${m.text()}`);
  });
  page.on("pageerror", (e) => errors.push(`${name}: ${e.message}`));
  if (block.includes("websocket")) {
    // A proxy that kills WebSockets: the socket closes before it opens.
    await page.evaluateOnNewDocument(() => {
      window.WebSocket = class {
        static OPEN = 1;
        constructor() { setTimeout(() => this.onclose?.({ code: 1006 }), 50); }
        send() {}
        close() {}
      };
    });
  }
  if (block.includes("sse")) {
    await page.setRequestInterception(true);
    page.on("request", (r) => (r.url().includes("/socket/sse") ? r.abort() : r.continue()));
  }
  await page.goto(`${BASE}/dev/guests/sign-in`, { waitUntil: "load" });
  await page.type("#name", name);
  await Promise.all([page.waitForNavigation({ waitUntil: "load" }), page.click("form button")]);
  await page.waitForFunction(() => document.querySelector("[data-live-room]")?.dataset.status === "open", { timeout: WAIT });
  return page;
}

const transport = (page) => page.evaluate(() => document.querySelector("[data-live-room]").dataset.transport);
const names = (page) => page.$$eval("[data-presence] [data-f=name]", (els) => els.map((e) => e.textContent));
const until = (page, fn, arg) => page.waitForFunction(fn, { timeout: WAIT }, arg).then(() => true, () => false);

try {
  const avery = await person("Avery Example");
  const blake = await person("Blake Example");
  check((await transport(avery)) === "websocket", "Avery is on the WebSocket");

  check(await until(avery, () => document.querySelectorAll("[data-presence] li").length === 2), "Avery sees two people here");
  check((await names(blake)).includes("Avery Example"), "Blake sees Avery");

  await blake.type("[data-composer] textarea", "Hello");
  check(await until(avery, () => document.querySelector("[data-typing]").textContent.includes("Blake Example is typing")), "Avery sees Blake typing");
  check(await until(blake, () => document.querySelector("[data-typing]").textContent === ""), "Blake does not see their own typing");

  await blake.keyboard.type(" <b>Avery</b>");
  await blake.keyboard.press("Enter");
  const sent = "Hello <b>Avery</b>";
  check(
    await until(avery, (t) => Array.from(document.querySelectorAll("[data-messages] [data-f=body]")).some((e) => e.textContent === t), sent),
    "Avery sees Blake's message, as text",
  );
  check((await avery.$$("[data-messages] b")).length === 0, "no markup was parsed out of the message");

  await avery.click("[data-presence] li:not(:first-child) [data-wave]");
  check(await until(blake, () => document.querySelector("[data-toasts]").textContent.includes("Avery Example waved at you")), "Blake gets Avery's wave");

  // Fallbacks: a third person whose WebSocket is blocked lands on SSE; a fourth, with SSE blocked
  // too, on long polling. Both still talk to the others.
  const casey = await person("Casey Example", { block: ["websocket"] });
  check((await transport(casey)) === "sse", "a blocked WebSocket falls back to SSE");
  const devon = await person("Devon Example", { block: ["websocket", "sse"] });
  check((await transport(devon)) === "longpoll", "a blocked SSE falls back to long polling");
  check(await until(devon, () => document.querySelectorAll("[data-presence] li").length === 4), "long polling sees everyone");
  await devon.type("[data-composer] textarea", "over long polling");
  await devon.keyboard.press("Enter");
  check(
    await until(avery, () => Array.from(document.querySelectorAll("[data-messages] [data-f=body]")).some((e) => e.textContent === "over long polling")),
    "Avery sees the long-polling message",
  );
  await casey.type("[data-composer] textarea", "over sse");
  await casey.keyboard.press("Enter");
  check(
    await until(devon, () => Array.from(document.querySelectorAll("[data-messages] [data-f=body]")).some((e) => e.textContent === "over sse")),
    "long polling sees the SSE message",
  );

  // Signing out closes the page's connection; the others see them leave.
  await Promise.all([blake.waitForNavigation({ waitUntil: "load" }), blake.click("form[action='/dev/guests/sign-out'] button")]);
  check(blake.url().endsWith("/dev/guests/sign-in"), "Blake is signed out");
  check(await until(avery, () => !Array.from(document.querySelectorAll("[data-presence] [data-f=name]")).some((e) => e.textContent.startsWith("Blake"))), "Avery sees Blake leave");
} catch (e) {
  check(false, `the run threw: ${e.message}`);
} finally {
  await browser.close();
}
check(errors.length === 0, `no console errors${errors.length ? `: ${errors.join(" | ")}` : ""}`);
console.log(failures ? `\n${failures} failure(s) against ${BASE}` : `\nlive room e2e passed against ${BASE}`);
process.exit(failures ? 1 : 0);
