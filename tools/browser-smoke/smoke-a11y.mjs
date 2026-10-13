// Accessibility smoke: axe-core (WCAG 2.2 AA) on each path, in the light AND the dark color
// scheme (prefers-color-scheme emulated), so a `bg-white` card with no `dark:` partner under a
// body that turned dark (1.09:1 contrast) fails here. Lighthouse checks one scheme only.
// The framework's own smoke; apps also keep their Playwright suite (test-standards §10).
//
//   PATHS="/ /session/new" BASE=… bash .offrails/tools/browser-smoke/run.sh a11y
//   SCENARIO=tests/e2e/smoke.scenario.mjs BASE=… bash .offrails/tools/browser-smoke/run.sh a11y
//
// The scenario may export a11yPaths (paths to check logged out), a11yAuthPaths (after login(page, base)).
import { BASE, axe, checker, launch, loginAndProve, preflight, scenario } from "./lib.mjs";

const { failures, check } = checker();
const sc = await scenario();
const envPaths = (process.env.PATHS ?? "").split(/\s+/).filter(Boolean);
const publicPaths = envPaths.length ? envPaths : sc.a11yPaths ?? ["/"];
const authPaths = sc.login ? sc.a11yAuthPaths ?? [] : [];

async function audit(page, paths, who) {
  for (const scheme of ["light", "dark"]) {
    await page.emulateMediaFeatures([{ name: "prefers-color-scheme", value: scheme }]);
    for (const p of paths) {
      await page.goto(`${BASE}${p}`, { waitUntil: "load" });
      const violations = await axe(page);
      check(violations.length === 0, `axe ${scheme.padEnd(5)} ${p} (${who}): ${violations.length} violation(s)`);
      for (const v of violations) {
        console.log(`     ${v.id} [${v.impact}] × ${v.count}`);
        v.nodes.forEach((n) => console.log(`       ${n}`));
      }
    }
  }
}

const browser = await launch();
try {
  const page = await browser.newPage();
  if (await preflight(page, check)) {
    await audit(page, publicPaths, "logged out");
    if (authPaths.length) {
      await loginAndProve(page, sc, check);
      await audit(page, authPaths, "logged in");
    }
  }
} finally {
  await browser.close();
}
console.log(failures.length ? `\n${failures.length} failure(s) against ${BASE}` : `\na11y smoke passed against ${BASE} (light + dark)`);
process.exit(failures.length ? 1 : 0);
