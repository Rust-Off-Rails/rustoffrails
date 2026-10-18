// Proves smoke-boosted.mjs catches the dead-after-a-boosted-link bug, against a tiny htmx site
// served here (no app, no build): a sign-in page whose password "Show" button is bound by
//   fixed   a delegated listener in a script the layout loads on every page   → the smoke passes
//   head    a script in the page's own <head> that binds on DOMContentLoaded  → the smoke fails
//   dcl     the layout's script, binding the button directly on DOMContentLoaded → the smoke fails
// and a console error on the home page fails it too.
//
//   node tools/browser-smoke/selftest-boosted.mjs        # framework CI; exit 77 (skip) without Chrome
import { spawn } from "node:child_process";
import fs from "node:fs";
import http from "node:http";
import os from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";

const here = path.dirname(fileURLToPath(import.meta.url));
const htmxFile = ["../../template/app/static/vendor", "../template/app/static/vendor"]
  .map((d) => path.join(here, d))
  .filter((d) => fs.existsSync(d))
  .flatMap((d) => fs.readdirSync(d).filter((f) => /^htmx-.*\.min\.js$/.test(f)).map((f) => path.join(d, f)))[0];
if (!htmxFile) {
  console.error("selftest-boosted: no template/app/static/vendor/htmx-*.min.js");
  process.exit(2);
}

const delegated = `document.addEventListener("click", (e) => {
  const b = e.target.closest("[data-reveal]");
  if (!b) return;
  const input = document.getElementById(b.dataset.reveal);
  input.type = input.type === "password" ? "text" : "password";
  b.setAttribute("aria-pressed", String(input.type === "text"));
});`;
const onLoad = `document.addEventListener("DOMContentLoaded", () => {
  document.querySelectorAll("[data-reveal]").forEach((b) => b.addEventListener("click", () => {
    const input = document.getElementById(b.dataset.reveal);
    input.type = input.type === "password" ? "text" : "password";
  }));
});`;

let mode = "fixed";
let homeError = false;
const page = (headExtra, body) => `<!doctype html>
<html lang="en"><head><meta charset="utf-8"><title>selftest</title>
<script src="/static/htmx.min.js" defer></script>
${mode === "fixed" ? '<script src="/static/delegated.js" defer></script>' : ""}
${mode === "dcl" ? '<script src="/static/on-load.js" defer></script>' : ""}
${headExtra}
</head><body hx-boost="true">
<nav><a href="/">Home</a> <a href="/sign-in">Sign in</a></nav>
<main>${body}</main>
</body></html>`;

const server = http.createServer((req, res) => {
  const send = (status, type, body) => {
    res.writeHead(status, { "content-type": type });
    res.end(body);
  };
  const url = new URL(req.url, "http://x");
  if (url.pathname === "/static/htmx.min.js") return send(200, "text/javascript", fs.readFileSync(htmxFile));
  if (url.pathname === "/static/delegated.js") return send(200, "text/javascript", delegated);
  if (url.pathname === "/static/on-load.js") return send(200, "text/javascript", onLoad);
  if (url.pathname === "/static/broken.js") return send(200, "text/javascript", 'console.error("selftest: broken on purpose");');
  if (url.pathname === "/robots.txt") return send(200, "text/plain", "User-agent: *\n");
  if (url.pathname === "/") {
    return send(200, "text/html", page(homeError ? '<script src="/static/broken.js" defer></script>' : "", "<h1>Home</h1>"));
  }
  if (url.pathname === "/sign-in") {
    const head = mode === "head" ? '<script src="/static/on-load.js" defer></script>' : "";
    return send(200, "text/html", page(head, `<h1>Sign in</h1>
<form method="post" action="/session"><label>Password <input id="pw" type="password" name="password"></label>
<button type="button" data-reveal="pw" aria-pressed="false">Show</button> <button>Sign in</button></form>`));
  }
  send(404, "text/plain", "not found");
});
await new Promise((r) => server.listen(0, "127.0.0.1", r));
const base = `http://127.0.0.1:${server.address().port}`;
const dir = fs.mkdtempSync(path.join(os.tmpdir(), "offrails-selftest-"));
const routes = path.join(dir, "routes.md");
fs.writeFileSync(routes, "| Method | Path | Handler | Access |\n|---|---|---|---|\n| GET | `/` | `home` | public |\n| GET | `/sign-in` | `sign_in` | public |\n| GET | `/robots.txt` | `robots` | public |\n| POST | `/session` | `session` | public |\n");

const smoke = () => new Promise((resolve) => {
  const child = spawn(process.execPath, [path.join(here, "smoke-boosted.mjs")], {
    env: { ...process.env, BASE: base, ROUTES: routes, SMOKE_SKIP: "" },
    stdio: ["ignore", "pipe", "pipe"],
  });
  let out = "";
  child.stdout.on("data", (d) => { out += d; });
  child.stderr.on("data", (d) => { out += d; });
  child.on("exit", (code) => resolve({ code, out }));
});

let failed = 0;
const expect = (ok, what, out) => {
  console.log(`${ok ? "ok  " : "FAIL"} ${what}`);
  if (!ok) {
    failed += 1;
    console.log(out.replace(/^/gm, "     | "));
  }
};
const cases = [
  ["fixed", false, 0, /\/sign-in \(signed out\): button\[data-reveal="pw"\] "Show" works after a boosted link/, "a delegated listener in the layout's script: passes"],
  ["head", false, 1, /\/static\/on-load\.js in its <head>[\s\S]*DEAD after a boosted link/, "a head-block script binding on DOMContentLoaded: fails, naming the script and the dead button"],
  ["dcl", false, 1, /DEAD after a boosted link/, "the layout's script binding on DOMContentLoaded: fails on the dead button"],
  ["fixed", true, 1, /console errors:[\s\S]*broken on purpose/, "a console error: fails"],
];
try {
  for (const [m, err, want, pattern, what] of cases) {
    mode = m;
    homeError = err;
    const r = await smoke();
    if (r.code === 77) {
      console.log(r.out.trim());
      process.exitCode = 77;
      break;
    }
    expect(r.code === want && pattern.test(r.out), `${what} (exit ${r.code})`, r.out);
    if (process.env.VERBOSE) console.log(r.out.replace(/^/gm, "     | "));
  }
} finally {
  server.closeAllConnections?.();
  server.close();
  fs.rmSync(dir, { recursive: true, force: true });
}
if (process.exitCode !== 77) {
  console.log(`\nselftest-boosted: ${cases.length - failed}/${cases.length} cases as expected`);
  process.exitCode = failed ? 1 : 0;
}
