// Browser smoke for the kernel UIs. Opens /dev directly (an app's home need not link to it),
// then mailbox → open message → Clear all → Back/Forward, and (with a test-job button on /dev)
// jobs → delete → Back. HTTP tests can't see boosted navigation, <head> swaps, the back/forward
// cache or CSP violations; this can.
// Run through run.sh. Seeds via the dev-only buttons (/dev/mailbox/test, /dev/jobs/test), so it
// needs no app-specific routes. Spec: .offrails/guides/dev-tools.md#browser-smoke
import { BASE, checker, launch, preflight } from "./lib.mjs";

const { failures, check } = checker();
const browser = await launch();
try {
  const page = await browser.newPage();
  // A desktop window: under 820 px the mailbox shows the list or the message, not both.
  await page.setViewport({ width: 1280, height: 900 });
  if (!(await preflight(page, check))) throw new Error("preflight failed: see above");
  const cspErrors = [];
  page.on("console", (m) => /Content Security Policy/i.test(m.text()) && cspErrors.push(m.text()));
  const go = (action) => Promise.all([page.waitForNavigation({ waitUntil: "load" }), action]);
  const click = (selector) => go(page.click(selector));
  const state = () =>
    page.evaluate(() => ({
      path: location.pathname,
      styled: [...document.styleSheets].some((s) => (s.href || "").includes("/kernel/admin.css")),
      rows: document.querySelectorAll("#mbx-list li.mbx-item").length,
      ids: [...document.querySelectorAll("#mbx-list li.mbx-item a")].map((a) => a.getAttribute("href")),
      flash: document.querySelector(".flash")?.textContent.trim() ?? "",
      text: document.body.innerText,
    }));

  // 1. /dev, opened directly: kernel pages are not boosted, so a full load is how a link gets there.
  await page.goto(`${BASE}/dev`, { waitUntil: "load" });
  let s = await state();
  check(s.path === "/dev" && s.styled, "/dev is styled");

  // 2. Seed three emails with the dev-only button (on /dev, and on the mailbox it lands in): each
  // lands on the message it sent, /dev/mailbox/<id>, with the flash, beside the list.
  // The page may carry more than one (a narrow message view hides the list's): click a visible one.
  const seed = 'form[action="/dev/mailbox/test"] button';
  const visibleSeed = async () => {
    for (const h of await page.$$(seed)) if (await h.isVisible()) return h;
    return null;
  };
  for (let i = 0; i < 3; i++) {
    let button = await visibleSeed();
    if (!button) {
      await page.goto(`${BASE}/dev/mailbox`, { waitUntil: "load" });
      button = await visibleSeed();
    }
    await go(button.click());
    s = await state();
    check(/^\/dev\/mailbox\/\d+$/.test(s.path) && s.styled && s.flash === "Sent a test email to your mailbox.",
      `the test-email button lands on its message with the flash (${s.path}: "${s.flash}")`);
  }
  check(s.rows >= 3, `the mailbox lists the seeded mail (${s.rows} rows)`);

  await page.goto(`${BASE}/dev/mailbox`, { waitUntil: "load" });
  await click("#mbx-list li.mbx-item a");
  s = await state();
  check(/^\/dev\/mailbox\/\d+$/.test(s.path) && s.styled, "opening a message stays styled");
  await page.waitForSelector("iframe[sandbox]");
  check(true, "the HTML part renders in a sandboxed iframe");

  // Judged by the messages that were cleared, not by an empty list: other smokes `verify --full`
  // runs at the same time sign in, and an auth part's "new sign-in" email may land meanwhile.
  const cleared = (await state()).ids;
  const back = (ids) => ids.filter((id) => cleared.includes(id));
  await click('form[action="/dev/mailbox/clear"] button');
  s = await state();
  check(s.path === "/dev/mailbox" && cleared.length >= 3 && back(s.ids).length === 0,
    `Clear all empties the list (${cleared.length} cleared${s.rows ? `; ${s.rows} newer` : ""})`);

  await go(page.goBack());
  s = await state();
  check(back(s.ids).length === 0 && s.styled, `Back after Clear all does not resurrect deleted mail (${back(s.ids).join(" ") || "none back"})`);
  await go(page.goForward()).catch(() => {});
  s = await state();
  check(back(s.ids).length === 0 && s.styled, "Forward shows a styled mailbox without the cleared mail");

  // 3. Jobs, when /dev has a test-job button: enqueue, open, delete, then Back.
  await page.goto(`${BASE}/dev`, { waitUntil: "load" });
  if (await page.$('form[action="/dev/jobs/test"] button')) {
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
  } else {
    console.log("skip no test-job button on /dev (POST /dev/jobs/test): the jobs flow is not exercised");
  }

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
