// static/auth.js: small enhancements for the sign-in and account pages. Every form works without
// it. The app layout loads it once, on every page (`samples: app head`), because links and forms
// are boosted: a page that arrives through a boosted click never runs a script in its own <head>.
// So each behavior is one listener on `document`, keyed by a data attribute: it works on the first
// page, on every boosted page and on every htmx swap (a form re-rendered after a failed attempt),
// with nothing to bind again. No inline scripts (CSP).
(() => {
  "use strict";
  // Loaded twice (an older page still has it in its head block): bind once.
  if (window.offrailsAuth) return;
  window.offrailsAuth = true;

  // Show/hide a password: <button type="button" data-reveal="password_id">, with an optional
  // [data-reveal-label] inside it.
  document.addEventListener("click", (e) => {
    const button = e.target.closest?.("[data-reveal]");
    if (!button) return;
    const input = document.getElementById(button.dataset.reveal);
    if (!input) return;
    const show = input.type === "password";
    input.type = show ? "text" : "password";
    button.setAttribute("aria-pressed", String(show));
    const label = button.querySelector("[data-reveal-label]");
    if (label) label.textContent = show ? "Hide" : "Show";
    input.focus();
  });

  // Caps Lock warning: <input data-capslock="hint_id"> and a hidden hint element. Any key or
  // pointer event on the field says whether Caps Lock is on.
  const capsLock = (e) => {
    const input = e.target.closest?.("[data-capslock]");
    if (!input || typeof e.getModifierState !== "function") return;
    document.getElementById(input.dataset.capslock)?.classList.toggle("hidden", !e.getModifierState("CapsLock"));
  };
  for (const type of ["keydown", "keyup", "mousedown"]) document.addEventListener(type, capsLock);
  // Leaving the field hides the hint (it's about what you type here).
  document.addEventListener("focusout", (e) => {
    const input = e.target.closest?.("[data-capslock]");
    if (input) document.getElementById(input.dataset.capslock)?.classList.add("hidden");
  });

  // One-time codes: keep digits only and submit once six are in (typed, pasted or autofilled).
  // The form submits once; a failed attempt re-renders it (a new element), so the next six submit.
  document.addEventListener("input", (e) => {
    const input = e.target.closest?.("[data-otp]");
    if (!input) return;
    const digits = input.value.replace(/\D/g, "").slice(0, 6);
    if (input.value !== digits) input.value = digits;
    const form = input.form;
    if (digits.length === 6 && form && !form.dataset.submitted) {
      form.dataset.submitted = "1";
      form.requestSubmit();
    }
  });
  // Its request ended without replacing the form (a network error, an error page): allow the
  // next submit.
  document.addEventListener("htmx:afterRequest", (e) => {
    const form = e.detail?.elt?.closest?.("form[data-submitted]");
    if (form) delete form.dataset.submitted;
  });

  // Copy a value: <button type="button" data-copy="element_id">.
  document.addEventListener("click", async (e) => {
    const button = e.target.closest?.("[data-copy]");
    if (!button) return;
    const source = document.getElementById(button.dataset.copy);
    if (!source || !navigator.clipboard) return;
    try {
      await navigator.clipboard.writeText(source.innerText.trim());
      const before = button.dataset.copyLabel ?? button.textContent;
      button.dataset.copyLabel = before;
      button.textContent = "Copied";
      setTimeout(() => { button.textContent = before; delete button.dataset.copyLabel; }, 1500);
    } catch {
      // Clipboard refused: the value is on the page to copy by hand.
    }
  });

  // Print: <button type="button" data-print>.
  document.addEventListener("click", (e) => {
    if (e.target.closest?.("[data-print]")) window.print();
  });
})();
