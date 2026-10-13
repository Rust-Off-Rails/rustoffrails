// Login smoke: a real browser with NO certificate overrides logs in through the real form and must
// still be logged in on the next page load. Catches the dev-TLS trap (a self-signed certificate:
// the server sets the cookie, Chrome drops it, the user bounces back to login) and any cookie
// attribute a browser refuses (Secure on http, a __Host- prefix with a Domain, SameSite=None…).
//
//   SCENARIO=tests/e2e/smoke.scenario.mjs BASE=<the URL bin/dev printed> bash .offrails/tools/browser-smoke/run.sh login
//
// The scenario exports login(page, base) (fill + submit the form) and loggedIn(page) → bool.
import { BASE, checker, launch, loginAndProve, preflight, scenario } from "./lib.mjs";

const { failures, check } = checker();
const sc = await scenario({ required: ["login", "loggedIn"] });
if (!sc.login) {
  console.error("set SCENARIO=<the app's scenario module exporting login and loggedIn>");
  process.exit(2);
}
const browser = await launch();
try {
  const page = await browser.newPage();
  if (await preflight(page, check)) await loginAndProve(page, sc, check);
} finally {
  await browser.close();
}
console.log(failures.length ? `\n${failures.length} failure(s) against ${BASE}` : `\nlogin smoke passed against ${BASE}`);
process.exit(failures.length ? 1 : 0);
