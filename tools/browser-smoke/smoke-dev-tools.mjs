// Browser smoke for the kernel UIs. Arrives from the app the way a person does (app nav → /dev),
// then mailbox → open message → Clear all → Back/Forward, and jobs → delete → Back. HTTP tests
// can't see boosted navigation, <head> swaps, the back/forward cache or CSP violations; this can.
// Run through run.sh. Seeds via the dev-only buttons (/dev/mailbox/test, /dev/jobs/test), so it
// needs no app-specific routes. Spec: .offrails/guides/dev-tools.md#browser-smoke
import { BASE, checker, launch, preflight } from "./lib.mjs";

const { failures, check } = checker();
const browser = await launch();
try {
  const page = await browser.newPage();
  if (!(await preflight(page, check))) throw new Error("preflight failed: see above");
  const cspErrors = [];
  page.on("console", (m) => /Content Security Policy/i.test(m.text()) && cspErrors.push(m.text()));
  const go = (action) => Promise.all([page.waitForNavigation({ waitUntil: "load" }), action]);
  const click = (selector) => go(page.click(selector));
  const state = () =>
    page.evaluate(() => ({
      path: location.pathname,
      styled: [...document.styleSheets].some((s) => (s.href || "").includes("/kernel/admin.css")),
      rows: document.querySelectorAll("#mail-list li:not(.new-mail)").length,
      text: document.body.innerText,
    }));

  // 1. From an app page into /dev (the app layout boosts links; kernel links must not be boosted).
  await page.goto(`${BASE}/`, { waitUntil: "load" });
  await click('a[href="/dev"]');
  let s = await state();
  check(s.path === "/dev" && s.styled, "app nav → /dev is styled without a hard refresh");

  // 2. Seed three emails with the dev-only button, then open the mailbox.
  for (let i = 0; i < 3; i++) await click('form[action="/dev/mailbox/test"] button');
  s = await state();
  check(s.path === "/dev/mailbox" && s.styled && s.rows >= 3, `mailbox lists the seeded mail (${s.rows} rows)`);

  await click("#mail-list li:not(.new-mail) a");
  s = await state();
  check(/^\/dev\/mailbox\/\d+$/.test(s.path) && s.styled, "opening a message stays styled");
  await page.waitForSelector("iframe[sandbox]");
  check(true, "the HTML part renders in a sandboxed iframe");

  await click('form[action="/dev/mailbox/clear"] button');
  s = await state();
  check(s.path === "/dev/mailbox" && s.rows === 0, "Clear all empties the list");

  await go(page.goBack());
  s = await state();
  check(s.rows === 0 && s.styled, "Back after Clear all does not resurrect deleted mail");
  await go(page.goForward()).catch(() => {});
  s = await state();
  check(s.rows === 0 && s.styled, "Forward shows an empty, styled mailbox");

  // 3. Jobs: enqueue, open, delete, then Back.
  await page.goto(`${BASE}/dev`, { waitUntil: "load" });
  await click('form[action="/dev/jobs/test"] button');
  s = await state();
  const id = (s.text.match(/Enqueued test job (\d+)/) || [])[1];
  check(s.path === "/admin/jobs/list" && s.styled && id, `test job enqueued (${id})`);
  await page.goto(`${BASE}/admin/jobs/${id}`, { waitUntil: "load" });
  await click(`form[action="/admin/jobs/${id}/delete"] button`);
  s = await state();
  check(s.path === "/admin/jobs/list" && /Deleted 1 job/.test(s.text), "deleting a job lands on the list with a flash");
  await go(page.goBack());
  s = await state();
  check(s.path === "/admin/jobs/list" && s.styled && /no longer exists/.test(s.text), "Back to the deleted job redirects to the list");

  check(cspErrors.length === 0, `no CSP violations (${cspErrors.length})`);
  cspErrors.forEach((e) => console.log(`     ${e}`));
} finally {
  await browser.close();
}
if (failures.length) {
  console.error(`\n${failures.length} check(s) failed`);
  process.exit(1);
}
console.log("\nall checks passed");
