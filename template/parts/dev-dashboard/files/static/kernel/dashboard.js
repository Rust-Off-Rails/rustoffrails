// The dev dashboard's behavior: live fragments over Server-Sent Events, the live log tail with
// pause and resume, and the charts' hover readout. Everything works without it: the pages are
// server-rendered, the charts are SVG with a data table, and a reload shows the latest numbers.
//
// The charts take their colors from CSS (light-dark() tokens), so a theme change needs nothing
// here. With prefers-reduced-motion, live fragments update at most every 5 seconds and nothing
// animates.
(() => {
  const reduced = window.matchMedia("(prefers-reduced-motion: reduce)");

  // ---- live fragments: <div data-live="cards"> gets the "cards" event's HTML ------------------
  const root = document.querySelector("[data-dashboard]");
  const src = root && root.dataset.stream;
  if (root && src) {
    const status = root.querySelector("[data-live-status]");
    const statusText = root.querySelector("[data-live-text]");
    const toggle = root.querySelector("[data-live-toggle]");
    const pending = new Map();
    let es = null;
    let paused = false;
    let flushTimer = null;

    const setStatus = (state, text) => {
      if (!status) return;
      status.classList.remove("on", "paused");
      if (state) status.classList.add(state);
      statusText.textContent = text;
    };
    const apply = (key, html) => {
      const el = document.querySelector(`[data-live="${key}"]`);
      if (!el) return;
      // Keep open <details> (a chart's data table) open across updates.
      const open = [...el.querySelectorAll("details[open]")].map((d) => d.closest("[id]")?.id);
      el.innerHTML = html;
      open.forEach((id) => id && document.getElementById(id)?.querySelector("details")?.setAttribute("open", ""));
      bindCharts(el);
    };
    const flush = () => {
      flushTimer = null;
      pending.forEach((html, key) => apply(key, html));
      pending.clear();
    };
    const connect = () => {
      es = new EventSource(src);
      es.addEventListener("open", () => setStatus("on", "Live"));
      es.addEventListener("error", () => setStatus("", "Reconnecting…"));
      document.querySelectorAll("[data-live]").forEach((el) => {
        es.addEventListener(el.dataset.live, (e) => {
          if (reduced.matches) {
            pending.set(el.dataset.live, e.data);
            if (!flushTimer) flushTimer = setTimeout(flush, 5000);
          } else {
            apply(el.dataset.live, e.data);
          }
        });
      });
    };
    const stop = () => {
      if (es) es.close();
      es = null;
    };
    if (toggle) {
      toggle.hidden = false;
      toggle.addEventListener("click", () => {
        paused = !paused;
        toggle.setAttribute("aria-pressed", String(paused));
        toggle.textContent = paused ? "Resume" : "Pause";
        if (paused) {
          stop();
          setStatus("paused", "Paused");
        } else {
          connect();
        }
      });
    }
    // A hidden tab doesn't need a stream.
    document.addEventListener("visibilitychange", () => {
      if (paused) return;
      if (document.hidden) stop();
      else if (!es) connect();
    });
    connect();
  }

  // ---- the live log tail ---------------------------------------------------------------------
  const logs = document.querySelector("[data-logs]");
  if (logs && logs.dataset.logStream) {
    const MAX = 500;
    const status = document.querySelector("[data-log-status]");
    const statusText = document.querySelector("[data-log-status-text]");
    const toggle = document.querySelector("[data-log-toggle]");
    const buffered = document.querySelector("[data-log-buffered]");
    const empty = document.querySelector("[data-logs-empty]");
    let held = [];
    let paused = false;
    const add = (html) => {
      logs.insertAdjacentHTML("afterbegin", html);
      const row = logs.firstElementChild;
      if (row && !reduced.matches) row.classList.add("fresh");
      while (logs.children.length > MAX) logs.lastElementChild.remove();
      if (empty) empty.hidden = true;
    };
    const es = new EventSource(logs.dataset.logStream);
    es.addEventListener("open", () => {
      status?.classList.add("on");
      if (statusText) statusText.textContent = paused ? "Paused" : "Live";
    });
    es.addEventListener("error", () => {
      status?.classList.remove("on");
      if (statusText) statusText.textContent = "Reconnecting…";
    });
    es.addEventListener("log", (e) => {
      // A line already on the page (the backlog) isn't added twice.
      if (e.lastEventId && logs.querySelector(`[data-log-id="${e.lastEventId}"]`)) return;
      if (paused) {
        held.push(e.data);
        if (held.length > MAX) held.shift();
        if (buffered) buffered.textContent = `${held.length} new line${held.length === 1 ? "" : "s"} held while paused.`;
      } else {
        add(e.data);
      }
    });
    if (toggle) {
      toggle.hidden = false;
      toggle.addEventListener("click", () => {
        paused = !paused;
        toggle.setAttribute("aria-pressed", String(paused));
        toggle.textContent = paused ? "Resume" : "Pause";
        status?.classList.toggle("paused", paused);
        if (statusText) statusText.textContent = paused ? "Paused" : "Live";
        if (!paused) {
          held.forEach(add);
          held = [];
          if (buffered) buffered.textContent = "";
        }
      });
    }
  }

  // ---- charts: crosshair, synchronized cursors, readout, drag-to-zoom, drill into traces --------
  // Every chart on a page shares one cursor time; the chart under the pointer (or with focus)
  // shows the readout. Drag (or Shift+arrows) selects a range to zoom into; a click (or Enter)
  // on Requests, Latency or Errors opens that minute's traces. Colors come from CSS, so a theme
  // change (the host's `themechange` event) needs no redraw.
  const units = {
    bytes: (v) => (v >= 1073741824 ? `${(v / 1073741824).toFixed(1)} GB` : v >= 1048576 ? `${(v / 1048576).toFixed(1)} MB` : v >= 1024 ? `${(v / 1024).toFixed(1)} KB` : `${Math.round(v)} B`),
    percent: (v) => `${Math.round(v)}%`,
    rate: (v) => `${v >= 10 ? Math.round(v) : v.toFixed(1)}/s`,
    micros: (v) => (v >= 1e6 ? `${(v / 1e6).toFixed(2)}s` : v >= 1000 ? `${(v / 1000).toFixed(1)}ms` : `${Math.round(v)}µs`),
    count: (v) => `${Math.round(v)}`,
  };
  const charts = [];
  let cursorT = null;
  const base = () => document.querySelector("[data-dashboard]")?.dataset.base || "/dev/dashboard";
  const clock = (t) => new Date(t * 1000).toISOString().slice(11, 19);

  function model(fig) {
    const svg = fig.querySelector("svg.plot");
    const times = (fig.dataset.times || "").split(",").filter(Boolean).map(Number);
    return {
      fig,
      svg,
      wrap: fig.querySelector(".plot-wrap"),
      times,
      from: Number(fig.dataset.from),
      to: Number(fig.dataset.to),
      x0: Number(svg.dataset.x0),
      x1: Number(svg.dataset.x1),
      fmt: units[fig.dataset.unit] || units.count,
      drill: fig.hasAttribute("data-drill"),
      lines: [...svg.querySelectorAll("path.line")].map((p) => ({
        name: p.dataset.name,
        cls: [...p.classList].find((c) => /^s\d$/.test(c)),
        values: (p.dataset.values || "").split(",").filter(Boolean).map(Number),
      })),
      cursor: svg.querySelector(".cursor"),
      sel: svg.querySelector(".selection"),
      tip: null,
      anchor: null,
    };
  }
  const xOf = (c, t) => c.x0 + ((t - c.from) / Math.max(c.to - c.from, 1)) * (c.x1 - c.x0);
  const tOf = (c, vx) => c.from + ((Math.min(Math.max(vx, c.x0), c.x1) - c.x0) / (c.x1 - c.x0)) * (c.to - c.from);
  const nearest = (c, t) => {
    let best = -1;
    let d = Infinity;
    c.times.forEach((x, i) => {
      if (Math.abs(x - t) < d) { d = Math.abs(x - t); best = i; }
    });
    return best;
  };
  const vxOf = (c, e) => {
    const box = c.svg.getBoundingClientRect();
    return ((e.clientX - box.left) / box.width) * c.svg.viewBox.baseVal.width;
  };

  function readout(c, i) {
    if (!c.tip) {
      c.tip = document.createElement("div");
      c.tip.className = "readout";
      c.tip.setAttribute("aria-hidden", "true");
      c.wrap.appendChild(c.tip);
    }
    c.tip.textContent = "";
    const head = document.createElement("div");
    head.className = "when";
    head.textContent = `${clock(c.times[i])} UTC`;
    c.tip.appendChild(head);
    c.lines.forEach((l) => {
      const row = document.createElement("div");
      const sw = document.createElement("span");
      sw.className = `sw ${l.cls}`;
      row.appendChild(sw);
      row.appendChild(document.createTextNode(`${l.name} ${c.fmt(l.values[i] ?? 0)}`));
      c.tip.appendChild(row);
    });
    if (c.drill) {
      const hint = document.createElement("div");
      hint.className = "hint-line";
      hint.textContent = "click: this minute's traces";
      c.tip.appendChild(hint);
    }
    c.tip.hidden = false;
    const box = c.svg.getBoundingClientRect();
    const px = ((xOf(c, c.times[i]) / c.svg.viewBox.baseVal.width) * box.width);
    const w = c.tip.offsetWidth;
    c.tip.style.left = `${Math.min(Math.max(px + 12, 0), box.width - w)}px`;
  }

  // Moves every chart's cursor to time `t`; `active` (the chart in use) shows the readout.
  function sync(t, active) {
    cursorT = t;
    charts.forEach((c) => {
      if (!c.cursor) return;
      if (t === null || !c.times.length) {
        c.cursor.setAttribute("hidden", "");
        if (c.tip) c.tip.hidden = true;
        return;
      }
      const i = nearest(c, t);
      const x = xOf(c, c.times[i]).toFixed(1);
      c.cursor.setAttribute("x1", x);
      c.cursor.setAttribute("x2", x);
      c.cursor.removeAttribute("hidden");
      if (c === active) readout(c, i);
      else if (c.tip) c.tip.hidden = true;
    });
  }

  function select(c, t1, t2) {
    if (!c.sel) return;
    if (t1 === null) {
      c.sel.setAttribute("hidden", "");
      return;
    }
    const a = xOf(c, Math.min(t1, t2));
    const b = xOf(c, Math.max(t1, t2));
    c.sel.setAttribute("x", a.toFixed(1));
    c.sel.setAttribute("width", Math.max(b - a, 1).toFixed(1));
    c.sel.removeAttribute("hidden");
  }

  function zoom(t1, t2) {
    const from = Math.floor(Math.min(t1, t2));
    const to = Math.ceil(Math.max(t1, t2));
    if (to - from < 10) return false;
    window.location.assign(`${base()}/charts?from=${from}&to=${to}`);
    return true;
  }
  function drill(c, t) {
    if (!c.drill) return;
    window.location.assign(`${base()}/traces?minute=${Math.floor(t / 60)}`);
  }

  function bindCharts(scope) {
    // Charts inside a swapped fragment are new elements: forget the old ones.
    for (let i = charts.length - 1; i >= 0; i--) {
      if (!document.contains(charts[i].fig)) charts.splice(i, 1);
    }
    scope.querySelectorAll("[data-chart]").forEach((fig) => {
      if (fig.dataset.bound) return;
      fig.dataset.bound = "1";
      const c = model(fig);
      if (!c.wrap || !c.svg) return;
      charts.push(c);
      let down = null;
      c.wrap.addEventListener("pointermove", (e) => {
        if (!c.times.length) return;
        const t = tOf(c, vxOf(c, e));
        sync(t, c);
        if (down !== null) select(c, down, t);
      });
      c.wrap.addEventListener("pointerleave", () => {
        if (down === null) sync(null, null);
      });
      c.wrap.addEventListener("pointerdown", (e) => {
        if (!c.times.length || e.button !== 0) return;
        down = tOf(c, vxOf(c, e));
        c.wrap.setPointerCapture(e.pointerId);
      });
      c.wrap.addEventListener("pointerup", (e) => {
        if (down === null) return;
        const t = tOf(c, vxOf(c, e));
        const start = down;
        down = null;
        select(c, null);
        const dx = Math.abs(xOf(c, t) - xOf(c, start)) / (c.x1 - c.x0) * c.svg.getBoundingClientRect().width;
        if (dx > 6) zoom(start, t);
        else drill(c, c.times[nearest(c, t)]);
      });
      c.wrap.addEventListener("focus", () => {
        if (!c.times.length) return;
        sync(cursorT ?? c.times[c.times.length - 1], c);
      });
      c.wrap.addEventListener("blur", () => {
        if (c.tip) c.tip.hidden = true;
      });
      c.wrap.addEventListener("keydown", (e) => {
        if (!c.times.length) return;
        let i = nearest(c, cursorT ?? c.times[c.times.length - 1]);
        const step = e.ctrlKey || e.metaKey ? 10 : 1;
        if (e.key === "ArrowLeft" || e.key === "ArrowRight" || e.key === "Home" || e.key === "End") {
          e.preventDefault();
          if (e.shiftKey && c.anchor === null) c.anchor = c.times[i];
          if (e.key === "ArrowLeft") i = Math.max(0, i - step);
          if (e.key === "ArrowRight") i = Math.min(c.times.length - 1, i + step);
          if (e.key === "Home") i = 0;
          if (e.key === "End") i = c.times.length - 1;
          if (!e.shiftKey) c.anchor = null;
          sync(c.times[i], c);
          select(c, c.anchor, c.times[i]);
        } else if (e.key === "Enter") {
          e.preventDefault();
          if (c.anchor !== null && zoom(c.anchor, c.times[i])) return;
          drill(c, c.times[i]);
        } else if (e.key === "Escape") {
          c.anchor = null;
          select(c, null);
          sync(null, null);
        }
      });
    });
    if (cursorT !== null) sync(cursorT, null);
  }
  bindCharts(document);

  // Ask AI lives in dashboard-ai.js (also loaded on other kernel pages for the Ask AI bar).
})();
