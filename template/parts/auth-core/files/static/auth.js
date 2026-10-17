// static/auth.js: small enhancements for the sign-in and account pages. Every form works without
// it; each behavior is one delegated listener keyed by a data attribute (no inline scripts: CSP).

// Show/hide a password: <button type="button" data-reveal="password_id">.
document.addEventListener("click", (e) => {
  const button = e.target.closest("[data-reveal]");
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

// Caps Lock warning: <input data-capslock="hint_id"> and a hidden hint element.
const capsLock = (e) => {
  const input = e.target.closest?.("[data-capslock]");
  if (!input || typeof e.getModifierState !== "function") return;
  document.getElementById(input.dataset.capslock)?.classList.toggle("hidden", !e.getModifierState("CapsLock"));
};
document.addEventListener("keydown", capsLock);
document.addEventListener("keyup", capsLock);

// One-time codes: keep digits only and submit once six are in (typed, pasted or autofilled).
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
// A failed attempt re-renders the form; allow the next submit.
document.addEventListener("htmx:afterSwap", () => {
  document.querySelectorAll("form[data-submitted]").forEach((f) => delete f.dataset.submitted);
});

// Copy a value: <button type="button" data-copy="element_id">.
document.addEventListener("click", async (e) => {
  const button = e.target.closest("[data-copy]");
  if (!button) return;
  const source = document.getElementById(button.dataset.copy);
  if (!source || !navigator.clipboard) return;
  try {
    await navigator.clipboard.writeText(source.innerText.trim());
    const before = button.textContent;
    button.textContent = "Copied";
    setTimeout(() => { button.textContent = before; }, 1500);
  } catch {
    // Clipboard refused: the value is on the page to copy by hand.
  }
});

// Print: <button type="button" data-print>.
document.addEventListener("click", (e) => {
  if (e.target.closest("[data-print]")) window.print();
});
