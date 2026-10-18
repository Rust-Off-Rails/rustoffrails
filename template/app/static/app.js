// static/app.js: each behavior is one delegated listener on `document`, keyed by a data attribute.
// No inline scripts (CSP): the app's own JS (parts add theirs at `samples: app head` in the layout).
// Links are boosted (hx-boost): this file runs once, on the first full load, and never again, so
// never bind to elements directly or on DOMContentLoaded (guides/frontend-htmx.md#boosted-navigation).
document.addEventListener("click", (e) => {
  const el = e.target.closest("[data-toggle]");
  const target = el && document.getElementById(el.dataset.toggle);
  if (!target) return;
  target.classList.toggle("hidden");
  if (el.hasAttribute("aria-expanded")) el.setAttribute("aria-expanded", String(!target.classList.contains("hidden")));
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

// A confirm on a form (data-confirm="…") asks in the layout's dialog: static/components.js.

// A page the back/forward cache restores is reloaded, so it never shows stale data.
window.addEventListener("pageshow", (event) => {
  if (event.persisted) window.location.reload();
});

// More behaviors (snippets, your own) go above this line, each one delegated listener.
// samples: app js
