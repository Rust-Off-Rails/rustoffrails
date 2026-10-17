// The jobs dashboard's only script (loaded by its pages after kernel/admin.js). Everything works
// without it: links, GET filter forms and POST action forms. It adds keyboard shortcuts,
// "select all", copy buttons, and pauses live polling while the tab is hidden.
// SECURITY: no eval, no inline handlers, no HTML built from strings: the CSP (script-src 'self')
// stays strict and job data can't become markup
// (test: the_dashboard_loads_its_own_css_and_js).
(() => {
  "use strict";

  const $ = (sel, root = document) => root.querySelector(sel);
  const $$ = (sel, root = document) => Array.from(root.querySelectorAll(sel));
  const typing = (el) =>
    el instanceof HTMLElement &&
    (el.isContentEditable || ["INPUT", "SELECT", "TEXTAREA"].includes(el.tagName));

  // ---- live polling: don't poll a tab nobody is looking at ----
  document.addEventListener("htmx:beforeRequest", (event) => {
    const trigger = event.detail && event.detail.requestConfig && event.detail.requestConfig.triggeringEvent;
    // `every Ns` polls have no triggering DOM event; user actions do.
    if (document.hidden && !trigger) event.preventDefault();
  });

  // ---- help dialog ----
  const help = () => $("#kbd-help");
  const openHelp = () => {
    const dialog = help();
    if (dialog && !dialog.open) dialog.showModal();
  };
  document.addEventListener("click", (event) => {
    const target = event.target instanceof Element ? event.target : null;
    if (!target) return;
    if (target.closest("[data-open-help]")) openHelp();

    // Copy buttons: data-copy copies the sibling [data-copy-text]; data-copy-target an element by id.
    const copy = target.closest("[data-copy], [data-copy-target]");
    if (copy) {
      const id = copy.getAttribute("data-copy-target");
      const source = id ? document.getElementById(id) : copy.parentElement && $("[data-copy-text]", copy.parentElement);
      if (source && navigator.clipboard) {
        navigator.clipboard.writeText(source.textContent.trim()).then(() => {
          const label = copy.textContent;
          copy.textContent = "Copied";
          copy.classList.add("copied");
          setTimeout(() => {
            copy.textContent = label;
            copy.classList.remove("copied");
          }, 1500);
        });
      }
    }
  });

  // ---- selection on the job list ----
  const updateSelection = () => {
    const out = $("[data-selected-count]");
    if (!out) return;
    const n = $$('input[name="ids"]:checked').length;
    out.textContent = n === 0 ? "Select jobs to act on them" : `${n} selected`;
    out.classList.toggle("has", n > 0);
    const all = $("[data-select-all]");
    const boxes = $$('input[name="ids"]');
    if (all) {
      all.checked = boxes.length > 0 && n === boxes.length;
      all.indeterminate = n > 0 && n < boxes.length;
    }
  };
  document.addEventListener("change", (event) => {
    const target = event.target;
    if (!(target instanceof HTMLInputElement)) return;
    if (target.matches("[data-select-all]")) {
      for (const box of $$('input[name="ids"]')) box.checked = target.checked;
    }
    updateSelection();
  });
  document.addEventListener("htmx:afterSwap", updateSelection);

  // ---- keyboard: g o / g j / g e / g c, /, j k x Enter, l, ? ----
  let cursor = -1;
  let pendingG = false;
  const rows = () => $$("[data-row]");
  const moveCursor = (delta) => {
    const list = rows();
    if (!list.length) return;
    cursor = Math.max(0, Math.min(list.length - 1, cursor + delta));
    for (const [i, row] of list.entries()) row.classList.toggle("cursor", i === cursor);
    const link = $(".row-link", list[cursor]);
    if (link) link.focus({ preventScroll: false });
  };
  const go = { o: "/admin/jobs", j: "/admin/jobs/list", e: "/admin/jobs/errors", c: "/admin/jobs/cron" };

  document.addEventListener("keydown", (event) => {
    if (event.metaKey || event.ctrlKey || event.altKey || typing(event.target)) return;
    if (help() && help().open) return;
    const key = event.key;
    if (pendingG) {
      pendingG = false;
      if (go[key]) {
        event.preventDefault();
        window.location.assign(go[key]);
      }
      return;
    }
    switch (key) {
      case "g":
        pendingG = true;
        setTimeout(() => { pendingG = false; }, 1200);
        break;
      case "?":
        event.preventDefault();
        openHelp();
        break;
      case "/": {
        const search = $("[data-search]");
        if (search) {
          event.preventDefault();
          search.focus();
          search.select();
        } else {
          event.preventDefault();
          window.location.assign("/admin/jobs/list");
        }
        break;
      }
      case "j":
        event.preventDefault();
        moveCursor(1);
        break;
      case "k":
        event.preventDefault();
        moveCursor(-1);
        break;
      case "x": {
        const row = rows()[cursor];
        const box = row && $('input[name="ids"]', row);
        if (box) {
          event.preventDefault();
          box.checked = !box.checked;
          updateSelection();
        }
        break;
      }
      case "Enter": {
        const row = rows()[cursor];
        const link = row && $(".row-link", row);
        if (link && document.activeElement !== link) {
          event.preventDefault();
          link.click();
        }
        break;
      }
      case "l": {
        const live = $(".live");
        if (live) {
          event.preventDefault();
          live.click();
        }
        break;
      }
      default:
        break;
    }
  });
})();
