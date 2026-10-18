// static/components.js: the behaviors of templates/components/ui.html (dropdown, tabs, modal,
// confirm dialog, dismissible toasts). Loaded once by layouts/application.html, like app.js.
// Links are boosted (hx-boost): this file runs on the first full load only, so every behavior is
// one delegated listener on `document`, keyed by a data attribute, never bound to elements
// (guides/frontend-htmx.md#boosted-navigation). No inline scripts or styles (CSP).
// GUIDE: .offrails/guides/views.md#components

// ---- dropdown: <details data-dropdown> (works without JS; this closes it like a menu) --------
document.addEventListener("click", (e) => {
  for (const d of document.querySelectorAll("details[data-dropdown][open]")) {
    if (!d.contains(e.target)) d.open = false;
  }
  // A menu item that does something closes its menu.
  const item = e.target.closest("details[data-dropdown] .menu-item");
  if (item) item.closest("details").open = false;
});
document.addEventListener("toggle", (e) => {
  const d = e.target;
  if (!(d instanceof HTMLDetailsElement) || !d.matches("[data-dropdown]") || !d.open) return;
  for (const other of document.querySelectorAll("details[data-dropdown][open]")) {
    if (other !== d) other.open = false;
  }
}, true);

// ---- tabs: [role=tablist][data-tabs] > [role=tab][aria-controls] (the panels are [role=tabpanel])
function selectTab(tab, focus) {
  const list = tab.closest("[data-tabs]");
  if (!list) return;
  for (const t of list.querySelectorAll('[role="tab"]')) {
    const on = t === tab;
    t.setAttribute("aria-selected", String(on));
    t.tabIndex = on ? 0 : -1;
    document.getElementById(t.getAttribute("aria-controls"))?.toggleAttribute("hidden", !on);
  }
  if (focus) tab.focus();
}
document.addEventListener("click", (e) => {
  const tab = e.target.closest('[data-tabs] [role="tab"]');
  if (tab) selectTab(tab, false);
});
document.addEventListener("keydown", (e) => {
  const tab = e.target.closest?.('[data-tabs] [role="tab"]');
  if (!tab || !["ArrowLeft", "ArrowRight", "Home", "End"].includes(e.key)) return;
  const tabs = [...tab.closest("[data-tabs]").querySelectorAll('[role="tab"]')];
  const i = tabs.indexOf(tab);
  const next = { ArrowLeft: i - 1, ArrowRight: i + 1, Home: 0, End: tabs.length - 1 }[e.key];
  e.preventDefault();
  selectTab(tabs[(next + tabs.length) % tabs.length], true);
});

// ---- modal: <dialog class="modal"> opened with showModal() (the page behind is inert, Esc closes)
const openers = new WeakMap();
const focusable = (root) => [...root.querySelectorAll(
  'a[href], button:not([disabled]), input:not([disabled]):not([type="hidden"]), select:not([disabled]), textarea:not([disabled]), [tabindex]:not([tabindex="-1"])',
)].filter((el) => el.getClientRects().length);

function openModal(dialog, opener) {
  if (!(dialog instanceof HTMLDialogElement) || dialog.open) return;
  const from = opener ?? document.activeElement;
  openers.set(dialog, from);
  // The browser gives focus back to what had it before showModal(): make that the opener.
  if (from instanceof HTMLElement && document.activeElement !== from) from.focus();
  dialog.showModal();
  (dialog.querySelector("[autofocus]") ?? focusable(dialog)[0])?.focus();
}
document.addEventListener("click", (e) => {
  const opener = e.target.closest("[data-modal-open]");
  if (opener) openModal(document.getElementById(opener.dataset.modalOpen), opener);
  const closer = e.target.closest("[data-modal-close]");
  if (closer) closer.closest("dialog")?.close();
  // A click on the backdrop (the dialog element itself, outside its box) closes it.
  if (e.target instanceof HTMLDialogElement && e.target.matches(".modal[open]")) {
    const r = e.target.getBoundingClientRect();
    const inside = e.clientX >= r.left && e.clientX <= r.right && e.clientY >= r.top && e.clientY <= r.bottom;
    if (!inside) e.target.close();
  }
});
// Tab and Shift+Tab stay inside an open modal.
document.addEventListener("keydown", (e) => {
  if (e.key !== "Tab") return;
  const dialog = document.querySelector("dialog.modal[open]");
  if (!dialog) return;
  const items = focusable(dialog);
  if (items.length === 0) return;
  const [first, last] = [items[0], items[items.length - 1]];
  if (e.shiftKey && (document.activeElement === first || !dialog.contains(document.activeElement))) {
    e.preventDefault();
    last.focus();
  } else if (!e.shiftKey && (document.activeElement === last || !dialog.contains(document.activeElement))) {
    e.preventDefault();
    first.focus();
  }
});
// `close` doesn't bubble: listen in the capture phase. Focus goes back to what opened it; a
// modal htmx loaded (data-modal-remove) leaves the page.
document.addEventListener("close", (e) => {
  const dialog = e.target;
  if (!(dialog instanceof HTMLDialogElement)) return;
  const opener = openers.get(dialog);
  if (opener?.isConnected) setTimeout(() => opener.focus(), 0);
  if (dialog.hasAttribute("data-modal-remove")) dialog.remove();
}, true);
// A modal htmx swapped in (`hx-get` → ui::modal(…, autoshow = true)) opens itself.
document.addEventListener("htmx:load", (e) => {
  const root = e.detail?.elt ?? e.target;
  if (!(root instanceof Element)) return;
  const found = root.matches("dialog[data-modal-autoshow]") ? [root] : [...root.querySelectorAll("dialog[data-modal-autoshow]")];
  for (const d of found) openModal(d, document.activeElement);
});

// ---- confirm: data-confirm="…" on a form or its submit button asks in #confirm-dialog -------
// (components/_confirm.html, in every layout). hx-confirm="…" on an htmx element asks there too.
function ask(message) {
  const dialog = document.getElementById("confirm-dialog");
  if (!(dialog instanceof HTMLDialogElement)) return Promise.resolve(window.confirm(message));
  dialog.querySelector("[data-confirm-message]").textContent = message;
  dialog.returnValue = "";
  return new Promise((resolve) => {
    dialog.addEventListener("close", () => resolve(dialog.returnValue === "ok"), { once: true });
    openModal(dialog, document.activeElement);
  });
}
const confirmed = new WeakSet();
// Capture phase: before htmx's own submit listener on a boosted form.
document.addEventListener("submit", (e) => {
  const form = e.target;
  const message = e.submitter?.dataset?.confirm ?? form.dataset?.confirm;
  if (!message) return;
  if (confirmed.has(form)) {
    confirmed.delete(form);
    return;
  }
  e.preventDefault();
  e.stopPropagation();
  const submitter = e.submitter;
  ask(message).then((ok) => {
    if (!ok) return;
    confirmed.add(form);
    form.requestSubmit(submitter && submitter.form === form ? submitter : undefined);
  });
}, true);
document.addEventListener("htmx:confirm", (e) => {
  const question = e.detail?.question;
  if (!question) return;
  e.preventDefault();
  ask(question).then((ok) => ok && e.detail.issueRequest(true));
});

// ---- toasts and dismissible alerts -----------------------------------------------------------
document.addEventListener("click", (e) => {
  const button = e.target.closest("[data-dismiss]");
  if (button) button.closest("[data-dismissible]")?.remove();
});
// data-toast-timeout="5000": gone after that many milliseconds (after a swap too).
document.addEventListener("htmx:load", (e) => {
  const root = e.detail?.elt ?? e.target;
  if (!(root instanceof Element)) return;
  for (const t of root.querySelectorAll("[data-toast-timeout]")) {
    const ms = Number(t.dataset.toastTimeout);
    if (ms > 0) setTimeout(() => t.remove(), ms);
  }
});
