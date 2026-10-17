// The schema diagram's behavior. kernel/dashboard/erd.rs draws the diagram on the server (layout,
// edges, crow's feet), so without this script it is a scrollable SVG whose tables link to their
// pages. This adds, on top of the same SVG:
//   - pan (drag the background, arrow keys), zoom (wheel, + and -, 0 fits), on screens wider
//     than 720px (narrower screens keep native scrolling: the diagram pans horizontally);
//   - dragging a table, its edges following; positions remembered in localStorage (each access
//     in try/catch: a private window or blocked storage just doesn't remember), Reset layout;
//   - click or focus a table: it and its neighbours stay lit, the rest dims; click also shows
//     the table's detail (fetched from its own page's [data-table-detail] block);
//   - find a table, "keys only", colour by area or prefix, copy the Mermaid text.
// The edge geometry (route, glyph) mirrors erd.rs: change both together.
(() => {
  // ---- copy buttons (the Mermaid text) ------------------------------------------------------
  document.querySelectorAll("[data-copy]").forEach((btn) => {
    const target = document.getElementById(btn.dataset.copy);
    const status = btn.closest("section")?.querySelector("[data-copy-status]");
    if (!target || !navigator.clipboard) return;
    btn.hidden = false;
    btn.addEventListener("click", async () => {
      try {
        await navigator.clipboard.writeText(target.textContent);
        if (status) status.textContent = "Copied to the clipboard.";
        btn.textContent = "Copied";
        setTimeout(() => (btn.textContent = "Copy"), 1500);
      } catch {
        if (status) status.textContent = "Couldn't copy: select the text instead.";
      }
    });
  });

  const root = document.querySelector("[data-erd]");
  if (!root) return;
  const svg = root.querySelector("[data-erd-svg]");
  const scene = svg.querySelector("[data-erd-scene]");
  const viewport = root.querySelector("[data-erd-viewport]");
  const detail = root.querySelector("[data-erd-detail]");
  const LAYOUT = "dashboard-erd-layout";
  const PREFS = "dashboard-erd-prefs";
  const narrow = window.matchMedia("(max-width: 720px)");
  const W = Number(svg.getAttribute("width"));
  const H = Number(svg.getAttribute("height"));
  const VIEWBOX = svg.getAttribute("viewBox");
  const HEADER = 30;

  const store = {
    get(key) {
      try {
        return JSON.parse(localStorage.getItem(key) || "null");
      } catch {
        return null;
      }
    },
    set(key, value) {
      try {
        if (value === null) localStorage.removeItem(key);
        else localStorage.setItem(key, JSON.stringify(value));
      } catch {
        /* storage unavailable: nothing is remembered */
      }
    },
  };

  const tables = new Map();
  svg.querySelectorAll(".erd-t").forEach((el) => {
    const t = {
      el,
      name: el.dataset.table,
      x: Number(el.dataset.x),
      y: Number(el.dataset.y),
      w: Number(el.dataset.w),
      h: Number(el.dataset.h),
      hk: Number(el.dataset.hk),
      box: el.querySelector(".erd-box"),
      cols: [...el.querySelectorAll(".erd-col")].map((c) => ({ el: c, y: Number(c.dataset.y), yk: Number(c.dataset.yk) })),
    };
    t.x0 = t.x;
    t.y0 = t.y;
    tables.set(t.name, t);
  });
  const edges = [...svg.querySelectorAll(".erd-edge")]
    .map((el) => ({
      el,
      line: el.querySelector(".erd-line"),
      glyph: el.querySelector(".erd-glyph"),
      child: tables.get(el.dataset.from),
      parent: tables.get(el.dataset.to),
      fc: Number(el.dataset.fc),
      tc: Number(el.dataset.tc),
      pe: el.dataset.pe,
      ce: el.dataset.ce,
    }))
    .filter((e) => e.child && e.parent);

  let keysOnly = false;
  const r1 = (v) => Math.round(v * 10) / 10;
  const colY = (t, i) => {
    const c = t.cols[i];
    if (!c) return HEADER / 2;
    return keysOnly ? c.yk : c.y;
  };

  // ---- geometry (mirrors erd.rs) --------------------------------------------------------------
  const glyph = (x, y, s, end) => {
    const p = (dx) => r1(x + s * dx);
    const ring = (dx) => `M${r1(x + s * dx - 4)} ${r1(y)}a4 4 0 1 0 8 0a4 4 0 1 0 -8 0`;
    const tick = (dx) => `M${p(dx)} ${r1(y - 6)}v12`;
    if (end === "one") return tick(8) + tick(13);
    if (end === "zero-one") return tick(8) + ring(17);
    const x0 = r1(x);
    const tip = p(11);
    return `M${x0} ${r1(y - 6)}L${tip} ${r1(y)}M${x0} ${r1(y)}L${tip} ${r1(y)}M${x0} ${r1(y + 6)}L${tip} ${r1(y)}` + ring(17);
  };
  const route = (parent, py, child, cy, pe, ce) => {
    const y1 = parent.y + py;
    const y2 = child.y + cy;
    if (parent === child) {
      const x = parent.x + parent.w;
      return [`M${r1(x)} ${r1(y2)}C${r1(x + 56)} ${r1(y2)} ${r1(x + 56)} ${r1(y1)} ${r1(x)} ${r1(y1)}`, glyph(x, y1, 1, pe) + glyph(x, y2, 1, ce)];
    }
    const pc = parent.x + parent.w / 2;
    const cc = child.x + child.w / 2;
    let x1, s1, x2, s2;
    if (Math.abs(pc - cc) < 1) [x1, s1, x2, s2] = [parent.x + parent.w, 1, child.x + child.w, 1];
    else if (cc > pc) [x1, s1, x2, s2] = [parent.x + parent.w, 1, child.x, -1];
    else [x1, s1, x2, s2] = [parent.x, -1, child.x + child.w, 1];
    const bend = s1 === s2 ? 70 : Math.max(Math.abs(x2 - x1) / 2, 40);
    const d = `M${r1(x1)} ${r1(y1)}C${r1(x1 + s1 * bend)} ${r1(y1)} ${r1(x2 + s2 * bend)} ${r1(y2)} ${r1(x2)} ${r1(y2)}`;
    return [d, glyph(x1, y1, s1, pe) + glyph(x2, y2, s2, ce)];
  };
  const drawEdge = (e) => {
    const [d, g] = route(e.parent, colY(e.parent, e.tc), e.child, colY(e.child, e.fc), e.pe, e.ce);
    e.line.setAttribute("d", d);
    e.glyph.setAttribute("d", g);
  };
  const place = (t) => t.el.setAttribute("transform", `translate(${r1(t.x)} ${r1(t.y)})`);
  const redraw = (only) => edges.filter((e) => !only || e.child === only || e.parent === only).forEach(drawEdge);

  const saveLayout = () => {
    const moved = {};
    tables.forEach((t) => {
      if (t.x !== t.x0 || t.y !== t.y0) moved[t.name] = [r1(t.x), r1(t.y)];
    });
    store.set(LAYOUT, Object.keys(moved).length ? moved : null);
  };
  const saved = store.get(LAYOUT);
  if (saved && typeof saved === "object") {
    Object.entries(saved).forEach(([name, pos]) => {
      const t = tables.get(name);
      if (t && Array.isArray(pos) && pos.every(Number.isFinite)) {
        [t.x, t.y] = pos;
        place(t);
      }
    });
    redraw();
  }

  // ---- view: pan and zoom (wide screens) ------------------------------------------------------
  const view = { k: 1, tx: 0, ty: 0, live: false };
  const apply = () => {
    if (view.live) scene.setAttribute("transform", `translate(${r1(view.tx)} ${r1(view.ty)}) scale(${view.k.toFixed(3)})`);
  };
  const size = () => ({ vw: viewport.clientWidth, vh: viewport.clientHeight });
  const fit = () => {
    const { vw, vh } = size();
    let minX = Infinity, minY = Infinity, maxX = -Infinity, maxY = -Infinity;
    tables.forEach((t) => {
      minX = Math.min(minX, t.x);
      minY = Math.min(minY, t.y);
      maxX = Math.max(maxX, t.x + t.w);
      maxY = Math.max(maxY, t.y + t.h);
    });
    if (!Number.isFinite(minX)) return;
    const k = Math.min((vw - 32) / (maxX - minX), (vh - 32) / (maxY - minY), 1.25);
    view.k = Math.max(k, 0.08);
    view.tx = (vw - (maxX - minX) * view.k) / 2 - minX * view.k;
    view.ty = (vh - (maxY - minY) * view.k) / 2 - minY * view.k;
    apply();
  };
  const zoomAt = (factor, cx, cy) => {
    const k = Math.min(Math.max(view.k * factor, 0.08), 4);
    view.tx = cx - ((cx - view.tx) * k) / view.k;
    view.ty = cy - ((cy - view.ty) * k) / view.k;
    view.k = k;
    apply();
  };
  const setLive = (live) => {
    view.live = live;
    viewport.classList.toggle("erd-live", live);
    // Wide: the detail floats over the diagram's right side; narrow: it follows the diagram.
    if (live) viewport.append(detail);
    else viewport.after(detail);
    if (live) {
      svg.removeAttribute("width");
      svg.removeAttribute("height");
      const { vw, vh } = size();
      svg.setAttribute("viewBox", `0 0 ${vw} ${vh}`);
      fit();
    } else {
      svg.setAttribute("width", String(W));
      svg.setAttribute("height", String(H));
      svg.setAttribute("viewBox", VIEWBOX);
      scene.removeAttribute("transform");
    }
  };
  // Client pixels to diagram units.
  const toScene = (ev) => {
    const r = svg.getBoundingClientRect();
    if (view.live) return [(ev.clientX - r.left - view.tx) / view.k, (ev.clientY - r.top - view.ty) / view.k];
    return [((ev.clientX - r.left) * W) / r.width, ((ev.clientY - r.top) * H) / r.height];
  };

  // ---- focus and detail -------------------------------------------------------------------------
  let selected = null;
  const highlight = (t) => {
    svg.classList.toggle("focusing", !!t);
    tables.forEach((x) => x.el.classList.remove("sel", "near"));
    edges.forEach((e) => e.el.classList.remove("hot"));
    if (!t) return;
    t.el.classList.add("sel");
    edges.forEach((e) => {
      if (e.child === t || e.parent === t) {
        e.el.classList.add("hot");
        e.child.el.classList.add("near");
        e.parent.el.classList.add("near");
      }
    });
  };
  const closeDetail = () => {
    detail.hidden = true;
    detail.replaceChildren();
    selected = null;
    highlight(null);
  };
  const showDetail = async (t) => {
    selected = t;
    highlight(t);
    const head = document.createElement("div");
    head.className = "erd-detail-head";
    const h = document.createElement("h3");
    h.className = "mono";
    h.textContent = t.name;
    const close = document.createElement("button");
    close.type = "button";
    close.className = "btn quiet";
    close.textContent = "Close";
    close.addEventListener("click", () => {
      closeDetail();
      t.el.focus();
    });
    head.append(h, close);
    detail.replaceChildren(head, Object.assign(document.createElement("p"), { className: "hint", textContent: "Loading…" }));
    detail.hidden = false;
    try {
      const res = await fetch(t.el.getAttribute("href"), { headers: { accept: "text/html" } });
      const doc = new DOMParser().parseFromString(await res.text(), "text/html");
      const block = doc.querySelector("[data-table-detail]");
      if (selected !== t) return;
      detail.replaceChildren(head, block ? document.importNode(block, true) : Object.assign(document.createElement("p"), { textContent: "No details." }));
    } catch {
      if (selected === t) detail.replaceChildren(head, Object.assign(document.createElement("p"), { className: "hint", textContent: "Couldn't load the details: open the table's page instead." }));
    }
  };
  const reveal = (t) => {
    if (view.live) {
      const { vw, vh } = size();
      view.tx = vw / 2 - (t.x + t.w / 2) * view.k;
      view.ty = vh / 2 - (t.y + t.h / 2) * view.k;
      apply();
    } else {
      const r = svg.getBoundingClientRect();
      viewport.scrollTo({ left: (t.x * r.width) / W - 16, top: (t.y * r.height) / H - 16, behavior: "smooth" });
    }
  };

  // ---- pointer: pan, drag, click ------------------------------------------------------------------
  let drag = null;
  let suppressClick = false;
  svg.addEventListener("pointerdown", (ev) => {
    if (ev.button !== 0) return;
    const el = ev.target.closest(".erd-t");
    const t = el && tables.get(el.dataset.table);
    if (!t && !view.live) return; // narrow: let the page scroll
    const [sx, sy] = toScene(ev);
    drag = { t, sx, sy, x: t ? t.x : view.tx, y: t ? t.y : view.ty, cx: ev.clientX, cy: ev.clientY, moved: false, id: ev.pointerId };
  });
  svg.addEventListener("pointermove", (ev) => {
    if (!drag || ev.pointerId !== drag.id) return;
    const dx = ev.clientX - drag.cx;
    const dy = ev.clientY - drag.cy;
    if (!drag.moved && Math.hypot(dx, dy) < 4) return;
    if (!drag.moved) {
      drag.moved = true;
      svg.setPointerCapture(ev.pointerId);
      svg.classList.add("dragging");
    }
    if (drag.t) {
      const [sx, sy] = toScene(ev);
      drag.t.x = Math.max(0, drag.x + sx - drag.sx);
      drag.t.y = Math.max(0, drag.y + sy - drag.sy);
      place(drag.t);
      redraw(drag.t);
    } else {
      view.tx = drag.x + dx;
      view.ty = drag.y + dy;
      apply();
    }
  });
  const endDrag = (ev) => {
    if (!drag || ev.pointerId !== drag.id) return;
    if (drag.moved) {
      suppressClick = true;
      setTimeout(() => (suppressClick = false), 0);
      if (drag.t) saveLayout();
    }
    svg.classList.remove("dragging");
    drag = null;
  };
  svg.addEventListener("pointerup", endDrag);
  svg.addEventListener("pointercancel", endDrag);
  // A click (or Enter on a focused table) selects instead of navigating; the link stays for a
  // new tab (modifier keys) and for no-JS.
  svg.addEventListener("click", (ev) => {
    const el = ev.target.closest(".erd-t");
    if (!el) return;
    if (ev.metaKey || ev.ctrlKey || ev.shiftKey || ev.button === 1) return;
    ev.preventDefault();
    if (suppressClick) return;
    const t = tables.get(el.dataset.table);
    if (t) showDetail(t);
  });
  svg.addEventListener("focusin", (ev) => {
    const el = ev.target.closest(".erd-t");
    if (el && !selected) highlight(tables.get(el.dataset.table));
  });
  svg.addEventListener("focusout", () => {
    if (!selected) highlight(null);
  });
  viewport.addEventListener(
    "wheel",
    (ev) => {
      if (!view.live) return;
      ev.preventDefault();
      const r = svg.getBoundingClientRect();
      zoomAt(Math.exp(-ev.deltaY * 0.0015), ev.clientX - r.left, ev.clientY - r.top);
    },
    { passive: false },
  );
  viewport.addEventListener("keydown", (ev) => {
    if (ev.key === "Escape" && selected) {
      const t = selected;
      closeDetail();
      t.el.focus();
      return;
    }
    if (!view.live) return;
    const { vw, vh } = size();
    const step = 48;
    const keys = {
      ArrowLeft: () => (view.tx += step),
      ArrowRight: () => (view.tx -= step),
      ArrowUp: () => (view.ty += step),
      ArrowDown: () => (view.ty -= step),
      "+": () => zoomAt(1.2, vw / 2, vh / 2),
      "=": () => zoomAt(1.2, vw / 2, vh / 2),
      "-": () => zoomAt(1 / 1.2, vw / 2, vh / 2),
      0: fit,
    };
    const act = keys[ev.key];
    if (!act) return;
    ev.preventDefault();
    act();
    apply();
  });

  // ---- tools ----------------------------------------------------------------------------------
  const tools = root.querySelector("[data-erd-tools]");
  tools.hidden = false;
  root.querySelector("[data-erd-help]").hidden = false;
  const keysBtn = tools.querySelector("[data-erd-keys]");
  const setKeys = (on) => {
    keysOnly = on;
    keysBtn.setAttribute("aria-pressed", String(on));
    svg.classList.toggle("keys", on);
    tables.forEach((t) => {
      t.box.setAttribute("height", String(on ? t.hk : t.h));
      t.cols.forEach((c) => c.el.setAttribute("transform", `translate(0 ${on ? c.yk : c.y})`));
    });
    redraw();
  };
  const colorBtns = [...tools.querySelectorAll("[data-erd-color]")];
  const setColor = (by) => {
    svg.dataset.color = by;
    colorBtns.forEach((b) => b.setAttribute("aria-pressed", String(b.dataset.erdColor === by)));
    root.querySelectorAll("[data-legend]").forEach((l) => (l.hidden = l.dataset.legend !== by));
  };
  const savePrefs = () => store.set(PREFS, { keys: keysOnly, color: svg.dataset.color });
  keysBtn.addEventListener("click", () => {
    setKeys(!keysOnly);
    savePrefs();
  });
  colorBtns.forEach((b) =>
    b.addEventListener("click", () => {
      setColor(b.dataset.erdColor);
      savePrefs();
    }),
  );
  const prefs = store.get(PREFS);
  if (prefs && prefs.keys === true) setKeys(true);
  if (prefs && (prefs.color === "area" || prefs.color === "prefix")) setColor(prefs.color);
  tools.querySelectorAll("[data-erd-zoom]").forEach((b) =>
    b.addEventListener("click", () => {
      const { vw, vh } = size();
      if (b.dataset.erdZoom === "fit") fit();
      else zoomAt(b.dataset.erdZoom === "in" ? 1.25 : 0.8, vw / 2, vh / 2);
    }),
  );
  tools.querySelector("[data-erd-reset]").addEventListener("click", () => {
    tables.forEach((t) => {
      t.x = t.x0;
      t.y = t.y0;
      place(t);
    });
    redraw();
    store.set(LAYOUT, null);
    if (view.live) fit();
  });
  const search = tools.querySelector("[data-erd-search]");
  const find = () => {
    const q = search.value.trim().toLowerCase();
    if (!q) return;
    const t = tables.get(q) || [...tables.values()].find((x) => x.name.includes(q));
    if (!t) return;
    reveal(t);
    showDetail(t);
    t.el.focus({ preventScroll: true });
  };
  search.addEventListener("change", find);
  search.addEventListener("keydown", (ev) => {
    if (ev.key === "Enter") {
      ev.preventDefault();
      find();
    }
  });

  // The zoom tools only mean something in the pannable view.
  const zoomTools = tools.querySelector(".erd-zoom");
  const mode = () => {
    setLive(!narrow.matches);
    zoomTools.hidden = narrow.matches;
  };
  narrow.addEventListener("change", mode);
  window.addEventListener("resize", () => {
    if (!view.live) return;
    const { vw, vh } = size();
    svg.setAttribute("viewBox", `0 0 ${vw} ${vh}`);
  });
  mode();
})();
