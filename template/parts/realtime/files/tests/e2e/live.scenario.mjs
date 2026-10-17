// The live room for the framework's browser smokes (.offrails/tools/browser-smoke):
//
//   SCENARIO=tests/e2e/live.scenario.mjs BASE=<the URL bin/dev printed> TABS=40 \
//     bash .offrails/tools/browser-smoke/run.sh live-tabs     # N tabs, one message, every tab sees it
//   SCENARIO=tests/e2e/live.scenario.mjs BASE=… bash .offrails/tools/browser-smoke/run.sh a11y
//
// Development only: it signs in as a guest (/dev/guests/sign-in). With accounts, sign in through
// the app's own form in login() instead.
export const path = "/live/lobby";

export const a11yPaths = ["/dev/guests/sign-in"];
export const a11yAuthPaths = ["/live/lobby", "/live/design", "/dev/sockets"];

export async function login(page, base) {
  await page.goto(`${base}/dev/guests/sign-in`, { waitUntil: "load" });
  await page.type("#name", "Smoke Test");
  await Promise.all([page.waitForNavigation({ waitUntil: "load" }), page.click("form button")]);
}

export async function loggedIn(page) {
  // Signed out, the room redirects to the sign-in page.
  await page.goto(`${new URL(page.url()).origin}/live/lobby`, { waitUntil: "load" });
  return (await page.$("[data-live-room]")) !== null;
}

export async function mutate(page) {
  const token = `smoke ${Date.now()}`;
  await page.waitForFunction(() => document.querySelector("[data-live-room]")?.dataset.status === "open");
  await page.type("[data-composer] textarea", token);
  await page.keyboard.press("Enter");
  return token;
}

export async function seen(page, token) {
  return page.evaluate(
    (t) => Array.from(document.querySelectorAll("[data-messages] [data-f=body]")).some((el) => el.textContent === t),
    token,
  );
}
