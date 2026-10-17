// Dev tools show live data (the mailbox changes under you), so a page restored from the
// back/forward cache is stale by definition: reload it instead of showing old mail.
window.addEventListener("pageshow", (event) => {
  if (event.persisted) window.location.reload();
});

// ---- theme switch (System / Light / Dark) ----------------------------------------------------
// The server already rendered <html data-theme> from the cookie, so this only handles changes.
// Without JS the switch is a POST form (controllers/theme.rs); with it, the change is instant:
// set data-theme, write the same cookie, and tell the page with a `themechange` event.
// A page whose JS reads colors (getComputedStyle, a canvas chart) re-reads them on
//   document.addEventListener("themechange", (e) => redraw(e.detail.scheme));
// e.detail = { theme: "system" | "light" | "dark", scheme: "light" | "dark" (what shows now) }.
(() => {
  const THEMES = ["system", "light", "dark"];
  // INVARIANT: the cookie name src/kernel/theme.rs reads (test: the_theme_switch_script_and_server_agree)
  const COOKIE = "offrails_theme";
  const MAX_AGE = 365 * 24 * 60 * 60;
  const dark = window.matchMedia("(prefers-color-scheme: dark)");
  const root = document.documentElement;
  const current = () => (THEMES.includes(root.dataset.theme) ? root.dataset.theme : "system");
  const scheme = (theme) => (theme === "dark" || (theme === "system" && dark.matches) ? "dark" : "light");
  const announce = (theme) =>
    document.dispatchEvent(new CustomEvent("themechange", { detail: { theme, scheme: scheme(theme) } }));

  function apply(theme) {
    root.dataset.theme = theme;
    document.querySelector('meta[name="color-scheme"]')
      ?.setAttribute("content", theme === "system" ? "light dark" : theme);
    // WHY: not HttpOnly, SameSite=Lax, a year, Secure on https: the cookie the server sets.
    const secure = window.location.protocol === "https:" ? "; Secure" : "";
    document.cookie = `${COOKIE}=${theme}; Path=/; SameSite=Lax; Max-Age=${MAX_AGE}${secure}`;
    for (const sw of document.querySelectorAll("[data-theme-switch]")) {
      for (const b of sw.querySelectorAll('button[name="theme"]')) {
        b.setAttribute("aria-pressed", String(b.value === theme));
      }
      const label = sw.querySelector("[data-theme-label]");
      const chosen = sw.querySelector(`button[value="${theme}"]`);
      if (label && chosen) label.textContent = chosen.textContent.trim();
      sw.querySelector("[data-theme-icon]")?.setAttribute("href", `#ti-${theme}`);
    }
    announce(theme);
  }

  const buttons = (sw) => [...sw.querySelectorAll('button[name="theme"]')];
  const close = (sw, focus) => {
    sw.open = false;
    if (focus) sw.querySelector("summary")?.focus();
  };

  // Choosing an option (click, Enter or Space): switch here instead of posting.
  document.addEventListener("submit", (e) => {
    if (!e.target.matches("[data-theme-form]")) return;
    const theme = e.submitter?.value;
    if (!THEMES.includes(theme)) return;
    e.preventDefault();
    apply(theme);
    close(e.target.closest("[data-theme-switch]"), true);
  });

  // Opening moves focus to the pressed option.
  document.addEventListener("toggle", (e) => {
    const sw = e.target;
    if (!(sw instanceof HTMLDetailsElement) || !sw.matches("[data-theme-switch]") || !sw.open) return;
    (buttons(sw).find((b) => b.getAttribute("aria-pressed") === "true") ?? buttons(sw)[0])?.focus();
  }, true);

  // Keyboard: arrows cycle System → Light → Dark (and around), Home/End, Escape closes.
  document.addEventListener("keydown", (e) => {
    const sw = e.target.closest?.("[data-theme-switch]");
    if (!sw) return;
    if (e.key === "Escape" && sw.open) {
      e.preventDefault();
      close(sw, true);
      return;
    }
    const list = buttons(sw);
    const at = list.indexOf(e.target);
    if (at < 0) return;
    const keys = { ArrowDown: at + 1, ArrowRight: at + 1, ArrowUp: at - 1, ArrowLeft: at - 1, Home: 0, End: list.length - 1 };
    const to = keys[e.key];
    if (to === undefined) return;
    e.preventDefault();
    list[(to + list.length) % list.length].focus();
  });

  // A click outside, or focus leaving the switch, closes it.
  document.addEventListener("click", (e) => {
    for (const sw of document.querySelectorAll("[data-theme-switch][open]")) {
      if (!sw.contains(e.target)) close(sw, false);
    }
  });
  document.addEventListener("focusout", (e) => {
    const sw = e.target.closest?.("[data-theme-switch]");
    if (sw?.open && e.relatedTarget && !sw.contains(e.relatedTarget)) close(sw, false);
  });

  // "System" follows the OS live: CSS recolors by itself, and JS hears about it too.
  dark.addEventListener("change", () => {
    if (current() === "system") announce("system");
  });
})();
