// The component kit's overlays in a real browser, on /dev/components (development): what the
// boosted smoke leaves out because it never clicks hx-get or data-confirm elements.
//   - modal: opens, keeps focus inside on Tab, closes on Esc, gives focus back to its opener;
//   - htmx-loaded modal: opens itself, leaves the page when closed;
//   - confirm dialog (data-confirm): Cancel keeps the page, Confirm submits;
//   - the demo form's 422 keeps what was typed and marks the fields;
//   - and all of it again after a boosted link from /.
//
//   BASE=<the URL bin/dev printed> node .offrails/tools/browser-smoke/smoke-components.mjs
//
// The driver every smoke here shares, cdp.mjs (Node >= 18, the installed Chrome, no npm install).
// No browser: exit 77.
import { Tab, launch, requireChrome, sleep } from "./cdp.mjs";

const BASE = (process.env.BASE ?? "").replace(/\/$/, "");
const PAGE = process.env.COMPONENTS_PATH ?? "/dev/components";
let failures = 0;
const check = (ok, what) => {
  if (!ok) failures += 1;
  console.log(`${ok ? "ok  " : "FAIL"} ${what}`);
};

async function until(tab, fn, ms = 3000) {
  for (let i = 0; i < ms / 50; i += 1) {
    if (await tab.eval(fn).catch(() => false)) return true;
    await sleep(50);
  }
  return false;
}

async function key(tab, k, shift = false) {
  const code = { Tab: 9, Escape: 27 }[k];
  for (const type of ["keyDown", "keyUp"]) {
    await tab.send("Input.dispatchKeyEvent", { type, key: k, code: k, windowsVirtualKeyCode: code, modifiers: shift ? 8 : 0 });
  }
}

async function exercise(tab, how) {
  const at = (what) => `${PAGE} (${how}): ${what}`;
  await tab.eval(() => document.querySelector("[data-modal-open]").click());
  const id = await tab.eval(() => document.querySelector("[data-modal-open]").dataset.modalOpen);
  check(await until(tab, new Function(`return document.getElementById(${JSON.stringify(id)})?.open`)), at("the modal opens"));
  for (let i = 0; i < 8; i += 1) await key(tab, "Tab", i % 3 === 2);
  check(await tab.eval((m) => document.getElementById(m).contains(document.activeElement), id), at("Tab and Shift+Tab keep focus inside the modal"));
  await key(tab, "Escape");
  check(await until(tab, new Function(`return !document.getElementById(${JSON.stringify(id)}).open`)), at("Esc closes it"));
  check(await tab.eval(() => document.activeElement?.hasAttribute("data-modal-open")), at("focus goes back to the button that opened it"));

  await tab.eval(() => document.querySelector('[hx-target="#modal-root"]').click());
  check(await until(tab, () => !!document.querySelector("#modal-root dialog[open]")), at("an htmx-loaded modal opens itself"));
  await tab.eval(() => document.querySelector("#modal-root dialog [data-modal-close]").click());
  check(await until(tab, () => !document.querySelector("#modal-root dialog")), at("and leaves the page when closed"));

  await tab.eval(() => document.querySelector("form[data-confirm] button").click());
  check(await until(tab, () => document.getElementById("confirm-dialog").open), at("data-confirm asks in the confirm dialog"));
  await tab.eval(() => document.querySelector('#confirm-dialog button[value="cancel"]').click());
  await sleep(400);
  check(await tab.eval((p) => location.pathname === p && !document.getElementById("confirm-dialog").open, PAGE), at("Cancel keeps the page"));
  await tab.eval(() => document.querySelector("form[data-confirm] button").click());
  await until(tab, () => document.getElementById("confirm-dialog").open);
  await tab.eval(() => document.querySelector('#confirm-dialog button[value="ok"]').click());
  check(await until(tab, () => !!document.querySelector('[role="status"], [role="alert"]') && document.body.textContent.includes("Confirmed")), at("Confirm submits the form"));
}

async function main() {
  if (!BASE) {
    console.error("usage: BASE=<the URL bin/dev printed> node .offrails/tools/browser-smoke/smoke-components.mjs");
    process.exit(2);
  }
  const chrome = requireChrome();
  let code = 0;
  let close = null;
  try {
    const launched = await launch(chrome, { prefix: "offrails-components-" });
    close = launched.close;
    const { cdp } = launched;
    const tab = await Tab.open(cdp, "components");
    await tab.viewport({ width: 1280, height: 800, mobile: false }, "light");
    await tab.goto(`${BASE}${PAGE}`);
    await exercise(tab, "full load");

    // After a boosted link from / (the scripts ran once, on the first page).
    await tab.goto(`${BASE}/`);
    await tab.eval((p) => {
      const a = document.createElement("a");
      a.href = p;
      a.textContent = "components";
      document.body.append(a);
      window.htmx?.process(a);
      a.click();
    }, PAGE);
    await until(tab, new Function(`return location.pathname === ${JSON.stringify(PAGE)} && !!document.querySelector("[data-modal-open]")`), 8000);
    await tab.settle();
    await exercise(tab, "after a boosted link");

    // The demo form's 422: values kept, fields marked.
    await tab.goto(`${BASE}${PAGE}`);
    await tab.eval(() => {
      const t = document.getElementById("title");
      t.value = "Dune";
      t.form.requestSubmit();
    });
    check(await until(tab, () => document.getElementById("email")?.getAttribute("aria-invalid") === "true"), `${PAGE}: an invalid submit marks the fields (aria-invalid)`);
    check(await tab.eval(() => document.getElementById("title").value === "Dune"), `${PAGE}: and keeps what was typed (422)`);
    const unexpected = tab.errors.filter((e) => !/status of 422/.test(e));
    check(unexpected.length === 0, `no console errors${unexpected.length ? `: ${unexpected.join(" | ")}` : ""}`);
    code = failures ? 1 : 0;
  } catch (e) {
    console.log(`FAIL ${e.message}`);
    code = 1;
  } finally {
    await close?.();
  }
  console.log(`\nsmoke-components: ${failures} failure(s)`);
  process.exit(code);
}

await main();
