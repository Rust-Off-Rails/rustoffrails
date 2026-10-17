// static/app.js: each behavior is one delegated listener, keyed by a data attribute. No inline
// scripts (CSP): this file is the only JS the app layout loads.
document.addEventListener("click", (e) => {
  const el = e.target.closest("[data-toggle]");
  if (el) document.getElementById(el.dataset.toggle)?.classList.toggle("hidden");
});

// A boosted navigation or form that fails (500, 403, 404…) would otherwise do nothing visible:
// htmx doesn't swap error responses. Show the server's error page instead, like a normal browser
// navigation would. Fragment requests (hx-get on a part of the page) keep htmx's behavior.
document.addEventListener("htmx:responseError", (e) => {
  if (!e.detail.boosted) return;
  const doc = new DOMParser().parseFromString(e.detail.xhr.responseText, "text/html");
  if (!doc.body) return;
  document.title = doc.title;
  document.body.replaceWith(document.adoptNode(doc.body));
  window.htmx?.process(document.body);
});

// A quick confirm on a form (data-confirm="…"); kernel pages use server-rendered confirmations.
document.addEventListener("submit", (e) => {
  const message = e.submitter?.dataset?.confirm ?? e.target.dataset?.confirm;
  if (message && !window.confirm(message)) e.preventDefault();
});

// A page the back/forward cache restores is reloaded, so it never shows stale data.
window.addEventListener("pageshow", (event) => {
  if (event.persisted) window.location.reload();
});
