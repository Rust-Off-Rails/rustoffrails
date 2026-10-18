// The dev dashboard's browser beacon (samples/dev-dashboard, rum.rs): rage clicks, dead clicks,
// clicks followed by an error, script errors, htmx errors, slow interactions and Web Vitals,
// batched and sent to the dashboard. Loaded once by the app layout, so every listener is on
// document (it keeps working after boosted navigation and swaps) and it binds nothing per page.
// It never sends text, input values or the URL search part: elements are named by id, data-testid,
// data-rum, name, role and their tag path.
(function () {
  "use strict";
  if (window.offrailsRum) return;
  window.offrailsRum = true;

  var me = document.currentScript;
  if (!me || !me.dataset.rum) return;
  var endpoint = me.dataset.rum;
  var pageRid = me.dataset.rid || "";
  var nav = "load";
  var queue = [];
  var timer = null;
  var sentThisPage = 0;
  var MAX_PER_PAGE = 60;
  var RAGE_CLICKS = 3, RAGE_MS = 700, DEAD_MS = 1000, SLOW_MS = 200;
  var seenErrors = {};

  function path() { return location.pathname; }

  function cssIdent(s) { return /^[A-Za-z][A-Za-z0-9_-]{0,60}$/.test(s) ? s : null; }

  // A stable, text-free name for an element.
  function selector(el) {
    if (!el || el.nodeType !== 1) return "";
    var id = el.id && cssIdent(el.id);
    if (id) return "#" + id;
    var tag = el.tagName.toLowerCase();
    var attrs = ["data-testid", "data-rum", "name"];
    for (var i = 0; i < attrs.length; i++) {
      var v = el.getAttribute(attrs[i]);
      if (v && cssIdent(v)) return tag + "[" + attrs[i] + "=" + v + "]";
    }
    var role = el.getAttribute("role");
    var parts = [];
    var node = el;
    for (var depth = 0; node && node.nodeType === 1 && depth < 4; depth++) {
      var t = node.tagName.toLowerCase();
      if (node.id && cssIdent(node.id)) { parts.unshift("#" + node.id); break; }
      var n = 1, sib = node;
      while ((sib = sib.previousElementSibling)) if (sib.tagName === node.tagName) n++;
      parts.unshift(n > 1 ? t + ":nth-of-type(" + n + ")" : t);
      if (t === "main" || t === "body") break;
      node = node.parentElement;
    }
    var s = parts.join(" > ");
    if (role && cssIdent(role)) s += "[role=" + role + "]";
    return s.slice(0, 160);
  }

  function push(ev) {
    if (sentThisPage >= MAX_PER_PAGE) return;
    sentThisPage++;
    queue.push(ev);
    if (!timer) timer = setTimeout(flush, 3000);
  }

  function flush() {
    clearTimeout(timer);
    timer = null;
    if (!queue.length) return;
    var body = JSON.stringify({ v: 1, page: path(), rid: pageRid, nav: nav, events: queue.splice(0, 40) });
    var blob = new Blob([body], { type: "text/plain" });
    if (!(navigator.sendBeacon && navigator.sendBeacon(endpoint, blob))) {
      try { fetch(endpoint, { method: "POST", body: body, keepalive: true, credentials: "same-origin", headers: { "content-type": "text/plain" } }); } catch (e) { /* the page is going away */ }
    }
    if (queue.length) timer = setTimeout(flush, 1000);
  }

  // A URL as kept: its path and line, never the origin or the search part.
  function src(file, line, col) {
    if (!file) return "";
    var p = file;
    try { p = new URL(file, location.href).pathname; } catch (e) { p = String(file).split("?")[0]; }
    return p + (line ? ":" + line + (col ? ":" + col : "") : "");
  }

  function stackOf(err) {
    if (!err || !err.stack) return "";
    return String(err.stack).split("\n").slice(0, 6).map(function (l) {
      return l.replace(/https?:\/\/[^\s)]+/g, function (u) {
        var m = /^(.*?)(:\d+(?::\d+)?)?$/.exec(u);
        try { return new URL(m[1]).pathname + (m[2] || ""); } catch (e) { return ""; }
      });
    }).join("\n");
  }

  var clickable = "a[href], button, [role=button], [role=link], [role=tab], [role=menuitem], input[type=submit], input[type=button], summary, label[for], [hx-get], [hx-post], [data-action]";
  var lastClick = null;
  var bursts = {};
  var lastRequest = {};

  function trusted(e) {
    return e.isTrusted || (e.target && e.target.closest && e.target.closest("[data-rum-lab]"));
  }

  document.addEventListener("click", function (e) {
    if (!trusted(e)) return;
    var el = e.target.closest && e.target.closest(clickable);
    if (!el) return;
    var sel = selector(el);
    var now = performance.now();
    lastClick = { sel: sel, at: now };
    // Rage: several clicks on one element in a short time.
    var b = bursts[sel];
    if (!b || now - b.start > RAGE_MS) b = bursts[sel] = { start: now, n: 0, reported: false };
    b.n++;
    if (b.n >= RAGE_CLICKS && !b.reported) {
      b.reported = true;
      setTimeout(function () {
        push({ t: "rage", sel: sel, n: b.n, ms: Math.round(performance.now() - b.start), rid: lastRequest[sel] || "" });
      }, 2000);
    }
    // Dead: nothing happens within a second.
    var href = el.getAttribute("href");
    if (el.tagName === "A" && href && href.charAt(0) !== "#" && !el.hasAttribute("download")) return;
    if (el.tagName === "SUMMARY" || el.tagName === "LABEL") return;
    if (el.disabled || el.getAttribute("aria-disabled") === "true" || el.closest("[data-rum-async]")) return;
    watchDead(el, sel, now);
  }, true);

  var watching = null;
  function watchDead(el, sel, at) {
    if (watching) watching.stop(false);
    var done = false;
    var mo = new MutationObserver(function () { stop(true); });
    function stop(alive) {
      if (done) return;
      done = true;
      mo.disconnect();
      clearTimeout(t);
      document.removeEventListener("htmx:beforeRequest", onReq);
      window.removeEventListener("pagehide", onLeave);
      document.removeEventListener("focusin", onFocus);
      watching = null;
      if (!alive) return;
    }
    function onReq() { stop(true); }
    function onLeave() { stop(true); }
    function onFocus(ev) { if (ev.target !== el) stop(true); }
    var t = setTimeout(function () {
      if (done) return;
      var fetched = performance.getEntriesByType("resource").some(function (r) {
        // Requests of its own (this beacon, the dev reload poll) say nothing about the click.
        return r.startTime >= at && r.initiatorType !== "beacon" && r.name.indexOf(endpoint) < 0 && r.name.indexOf("/dev/reload") < 0;
      });
      var hashMoved = location.hash !== startHash;
      stop(true);
      if (!fetched && !hashMoved) push({ t: "dead", sel: sel });
    }, DEAD_MS);
    var startHash = location.hash;
    mo.observe(document.documentElement, { subtree: true, childList: true, attributes: true, characterData: true });
    document.addEventListener("htmx:beforeRequest", onReq);
    window.addEventListener("pagehide", onLeave);
    document.addEventListener("focusin", onFocus);
    watching = { stop: stop };
  }

  function report(kind, message, file, line, col, err) {
    var key = kind + "|" + message + "|" + file + line;
    seenErrors[key] = (seenErrors[key] || 0) + 1;
    if (seenErrors[key] > 3) return;
    var afterClick = lastClick && performance.now() - lastClick.at < 1000 ? lastClick.sel : "";
    if (afterClick) {
      if (watching) watching.stop(false);
      push({ t: "error_click", sel: afterClick, msg: String(message).slice(0, 300) });
    }
    push({ t: kind, msg: String(message).slice(0, 300), src: src(file, line, col), stack: stackOf(err).slice(0, 1200), sel: afterClick });
  }

  window.addEventListener("error", function (e) {
    if (e.target && e.target !== window && e.target.tagName) {
      var u = e.target.src || e.target.href || "";
      report("js_error", "failed to load " + e.target.tagName.toLowerCase() + " " + src(u), u, 0, 0, null);
      return;
    }
    report("js_error", e.message || "Script error", e.filename, e.lineno, e.colno, e.error);
  }, true);

  window.addEventListener("unhandledrejection", function (e) {
    var r = e.reason;
    report("rejection", (r && r.message) || String(r).slice(0, 200), "", 0, 0, r);
  });

  // htmx: errors, the request id of each answer, boosted navigation.
  var started = {};
  document.addEventListener("htmx:beforeRequest", function (e) {
    var d = e.detail || {};
    if (d.boosted) { sendVitals(); flush(); }
    started[selector(d.elt)] = performance.now();
  });
  document.addEventListener("htmx:afterRequest", function (e) {
    var d = e.detail || {};
    var xhr = d.xhr;
    var rid = xhr && xhr.getResponseHeader && xhr.getResponseHeader("x-request-id");
    var sel = selector(d.elt);
    if (rid && /^[A-Za-z0-9-]{1,64}$/.test(rid)) {
      lastRequest[sel] = rid;
      if (d.boosted) { pageRid = rid; nav = "boosted"; sentThisPage = 0; resetVitals(); }
    }
  });
  ["htmx:responseError", "htmx:sendError", "htmx:swapError", "htmx:timeout", "htmx:targetError"].forEach(function (name) {
    document.addEventListener(name, function (e) {
      var d = e.detail || {};
      var xhr = d.xhr;
      var rid = xhr && xhr.getResponseHeader && xhr.getResponseHeader("x-request-id");
      push({ t: "htmx", kind: name.slice(5), status: (xhr && xhr.status) || 0, sel: selector(d.elt), rid: rid || "" });
    });
  });

  // Web Vitals: LCP, CLS and INP for this page view, sent when it is hidden or left.
  var lcp = 0, cls = 0, inp = 0, vitalsSent = false;
  function resetVitals() { lcp = 0; cls = 0; inp = 0; vitalsSent = false; }
  function observe(type, fn, opts) {
    try {
      var po = new PerformanceObserver(function (list) { list.getEntries().forEach(fn); });
      po.observe(Object.assign({ type: type, buffered: true }, opts || {}));
    } catch (e) { /* not supported in this browser */ }
  }
  observe("largest-contentful-paint", function (en) { if (nav === "load") lcp = en.startTime; });
  observe("layout-shift", function (en) { if (!en.hadRecentInput) cls += en.value; });
  observe("event", function (en) {
    if (!en.interactionId) return;
    inp = Math.max(inp, en.duration);
    if (en.duration > SLOW_MS) push({ t: "slow", sel: selector(en.target), ms: Math.round(en.duration) });
  }, { durationThreshold: 40 });
  function sendVitals() {
    if (vitalsSent) return;
    vitalsSent = true;
    if (lcp > 0) push({ t: "vital", name: "LCP", value: Math.round(lcp) });
    push({ t: "vital", name: "CLS", value: Math.round(cls * 1000) / 1000 });
    if (inp > 0) push({ t: "vital", name: "INP", value: Math.round(inp) });
  }
  document.addEventListener("visibilitychange", function () {
    if (document.visibilityState === "hidden") { sendVitals(); flush(); }
  });
  window.addEventListener("pagehide", function () { sendVitals(); flush(); });
})();
