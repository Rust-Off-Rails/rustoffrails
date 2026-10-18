// The trace component's behavior (waterfall.rs, _waterfall.html), the traces explorer, and trace
// embeds on other pages. Everything works without it: every waterfall row is server-rendered
// ("Every row" lists the folded ones), filters are a GET form, rows link to their permalink.
//
// Here: fold and unfold rows (state per trace in the URL hash), expand/collapse all, the side
// panel, copy as JSON, the explorer's inline rows (j/k/Enter/Esc), its scatter (click a dot, brush
// a time range), compare, holding the live list while a row is open, and lazy embeds
// ([data-trace-embed], [data-wf-src], [data-trace-inline]).
(() => {
  const ready = new WeakSet();

  // ---- one waterfall ----------------------------------------------------------------------------
  function initWaterfall(root, useHash) {
    if (!root || ready.has(root)) return;
    ready.add(root);
    const rows = [...root.querySelectorAll("tr.wf-row")];
    const byId = new Map(rows.map((r) => [r.id, r]));
    const caret = (r) => r.querySelector(".wf-caret");
    const open = (r) => caret(r)?.getAttribute("aria-expanded") === "true";
    const initial = new Map(rows.map((r) => [r.id, open(r)]));
    const key = `t${root.dataset.waterfall}`;

    const layout = () => {
      const visible = new Map();
      for (const r of rows) {
        const p = r.dataset.parent;
        const show = !p || (visible.get(p) && open(byId.get(p)));
        visible.set(r.id, show);
        r.hidden = !show;
      }
    };
    const setOpen = (r, value) => {
      const c = caret(r);
      if (!c) return;
      c.setAttribute("aria-expanded", String(value));
      c.setAttribute("aria-label", c.getAttribute("aria-label").replace(/^(Show|Hide)/, value ? "Hide" : "Show"));
    };
    const save = () => {
      if (!useHash) return;
      const changed = rows.filter((r) => caret(r) && open(r) !== initial.get(r.id)).map((r) => r.id.split("-").pop());
      const others = location.hash.replace(/^#/, "").split("&").filter((p) => p && !p.startsWith(`${key}=`));
      if (changed.length) others.push(`${key}=${changed.join(".")}`);
      history.replaceState(null, "", others.length ? `#${others.join("&")}` : location.pathname + location.search);
    };
    const restore = () => {
      if (!useHash) return;
      const part = location.hash.replace(/^#/, "").split("&").find((p) => p.startsWith(`${key}=`));
      if (!part) return;
      for (const n of part.split("=")[1].split(".")) {
        const r = byId.get(`w${root.dataset.waterfall}-${n}`);
        if (r) setOpen(r, !initial.get(r.id));
      }
    };
    const reveal = (id) => {
      let r = byId.get(id);
      if (!r) return;
      setOpen(r, true);
      while (r && r.dataset.parent) {
        r = byId.get(r.dataset.parent);
        if (r) setOpen(r, true);
      }
      layout();
      save();
      byId.get(id).scrollIntoView({ block: "center", behavior: matchMedia("(prefers-reduced-motion: reduce)").matches ? "auto" : "smooth" });
      select(byId.get(id));
    };

    // The side panel.
    const body = root.querySelector(".wf-body");
    const panel = root.querySelector("[data-wf-panel]");
    let selected = null;
    const close = () => {
      if (!panel) return;
      panel.hidden = true;
      body.classList.remove("with-panel");
      selected?.classList.remove("sel");
      selected = null;
    };
    const select = (r) => {
      const tpl = root.querySelector(`template#${CSS.escape(r.id)}-d`);
      if (!panel || !tpl) return;
      selected?.classList.remove("sel");
      selected = r;
      r.classList.add("sel");
      panel.replaceChildren(tpl.content.cloneNode(true));
      panel.querySelectorAll("[data-wf-copy], [data-wf-copy-text]").forEach((b) => (b.hidden = false));
      panel.hidden = false;
      body.classList.add("with-panel");
    };

    root.addEventListener("click", async (e) => {
      const t = e.target.closest("button, a");
      if (!t || !root.contains(t)) return;
      if (t.dataset.wfToggle) {
        const r = byId.get(t.dataset.wfToggle);
        setOpen(r, !open(r));
        layout();
        save();
      } else if (t.dataset.wfDetail) {
        e.preventDefault();
        const r = byId.get(t.dataset.wfDetail);
        if (selected === r) close();
        else select(r);
      } else if (t.dataset.wfReveal) {
        e.preventDefault();
        reveal(t.dataset.wfReveal);
      } else if (t.dataset.wfClose !== undefined) {
        close();
      } else if (t.dataset.wfAct) {
        const act = t.dataset.wfAct;
        for (const r of rows) {
          if (!caret(r) || !r.dataset.parent) continue;
          if (act === "expand") setOpen(r, true);
          else if (act === "collapse") setOpen(r, false);
          else if (act === "sql" && r.dataset.group !== undefined && r.dataset.kind === "sql") setOpen(r, t.getAttribute("aria-pressed") !== "true");
        }
        if (act === "sql") t.setAttribute("aria-pressed", String(t.getAttribute("aria-pressed") !== "true"));
        layout();
        save();
      } else if (t.dataset.wfCopy) {
        const res = await fetch(t.dataset.wfCopy, { headers: { Accept: "application/json" } });
        if (res.ok) await copy(t, await res.text());
      } else if (t.dataset.wfCopyText) {
        await copy(t, t.dataset.wfCopyText);
      }
    });
    panel?.addEventListener("click", async (e) => {
      const t = e.target.closest("button");
      if (!t) return;
      if (t.dataset.wfClose !== undefined) close();
      if (t.dataset.wfCopy) {
        const res = await fetch(t.dataset.wfCopy);
        if (res.ok) await copy(t, await res.text());
      }
      if (t.dataset.wfCopyText) await copy(t, t.dataset.wfCopyText);
    });
    root.addEventListener("keydown", (e) => {
      if (e.key === "Escape" && !panel?.hidden) {
        close();
        e.stopPropagation();
      }
    });
    root.querySelectorAll("[data-wf-act], [data-wf-copy]").forEach((b) => (b.hidden = false));
    root.querySelector("[data-wf-nojs]")?.setAttribute("hidden", "");
    restore();
    layout();
  }

  async function copy(button, text) {
    try {
      await navigator.clipboard.writeText(text);
      const was = button.textContent;
      button.textContent = "Copied";
      setTimeout(() => (button.textContent = was), 1200);
    } catch {
      button.textContent = "Copy failed";
    }
  }

  // ---- lazy waterfalls: an explorer row, an embed on another page -------------------------------
  async function load(box) {
    if (!box || box.dataset.loaded) return;
    box.dataset.loaded = "1";
    box.classList.add("loading");
    try {
      const res = await fetch(box.dataset.wfSrc || box.dataset.traceEmbed, { headers: { Accept: "text/html" } });
      const html = await res.text();
      if (res.ok || res.status === 404) box.innerHTML = html;
    } catch {
      // Keep the link that's already there.
    }
    box.classList.remove("loading");
    box.querySelectorAll("[data-waterfall]").forEach((w) => initWaterfall(w, false));
  }

  // ---- the explorer -----------------------------------------------------------------------------
  function initExplorer() {
    const list = document.querySelector("[data-tx-list]");
    const filters = document.querySelectorAll("[data-tx-filters]");
    filters.forEach((form) => {
      form.classList.add("auto");
      form.addEventListener("change", (e) => {
        if (e.target.matches("select, input[type=checkbox]")) form.requestSubmit();
      });
    });
    if (!list) return;

    // Hold the live list while a row is open or the list is being scrolled.
    let scrolled = 0;
    const hold = () => {
      const busy = list.querySelector("details[open]") || Date.now() - scrolled < 4000 || list.querySelector(".tx-pick:checked");
      if (busy) list.setAttribute("data-live-hold", "");
      else list.removeAttribute("data-live-hold");
    };
    addEventListener("scroll", () => {
      scrolled = Date.now();
      hold();
      setTimeout(hold, 4100);
    }, { passive: true });

    list.addEventListener("toggle", (e) => {
      const d = e.target;
      if (!(d instanceof HTMLDetailsElement)) return;
      if (d.open) load(d.querySelector("[data-wf-src]"));
      hold();
    }, true);

    // Compare: exactly two ticked.
    const cmp = document.querySelector("[data-tx-compare]");
    const hint = document.querySelector("[data-tx-compare-hint]");
    const picks = () => [...document.querySelectorAll(".tx-pick:checked")];
    const sync = () => {
      const n = picks().length;
      if (cmp) cmp.disabled = n !== 2;
      if (hint) hint.textContent = n === 2 ? "Ready." : n > 2 ? "Tick only two." : `Tick ${2 - n} more.`;
      hold();
    };
    list.addEventListener("change", (e) => {
      if (e.target.matches(".tx-pick")) sync();
    });
    sync();

    // Keyboard: j/k move between rows, Enter/Space open (the summary does it), Esc closes.
    const summaries = () => [...list.querySelectorAll(".tx-d > summary")];
    document.addEventListener("keydown", (e) => {
      if (e.target.closest("input, select, textarea, [contenteditable]") || e.metaKey || e.ctrlKey || e.altKey) return;
      const all = summaries();
      if (!all.length) return;
      const at = all.indexOf(document.activeElement?.closest?.(".tx-d")?.querySelector("summary"));
      if (e.key === "j" || e.key === "k") {
        e.preventDefault();
        const next = all[Math.max(0, Math.min(all.length - 1, at < 0 ? 0 : at + (e.key === "j" ? 1 : -1)))];
        list.querySelectorAll(".tx.kbd").forEach((x) => x.classList.remove("kbd"));
        next.closest(".tx").classList.add("kbd");
        next.focus();
        next.scrollIntoView({ block: "nearest" });
      } else if (e.key === "Escape" && at >= 0) {
        const d = all[at].parentElement;
        if (d.open) {
          d.open = false;
          all[at].focus();
        }
      }
    });

    // The scatter: click a dot to open its row; drag across it to zoom the time.
    list.addEventListener("click", (e) => {
      const dot = e.target.closest?.(".dot");
      if (!dot) return;
      const row = document.getElementById(`tx-${dot.dataset.id}`);
      const d = row?.querySelector("details");
      if (!d) return;
      d.open = true;
      row.scrollIntoView({ block: "center" });
      d.querySelector("summary").focus();
    });
    let drag = null;
    list.addEventListener("pointerdown", (e) => {
      const fig = e.target.closest?.("[data-tx-scatter]");
      if (!fig || e.target.closest(".dot")) return;
      const svg = fig.querySelector("svg");
      const box = svg.getBoundingClientRect();
      drag = { fig, svg, box, x0: e.clientX - box.left, brush: fig.querySelector("[data-tx-brush]") };
      drag.brush.hidden = false;
      svg.setPointerCapture(e.pointerId);
    });
    list.addEventListener("pointermove", (e) => {
      if (!drag) return;
      const x = Math.max(0, Math.min(drag.box.width, e.clientX - drag.box.left));
      drag.brush.setAttribute("x", Math.min(x, drag.x0));
      drag.brush.setAttribute("width", Math.abs(x - drag.x0));
      drag.x1 = x;
    });
    list.addEventListener("pointerup", () => {
      if (!drag) return;
      const { fig, box, x0, x1 } = drag;
      drag = null;
      if (x1 === undefined || Math.abs(x1 - x0) < 6) {
        fig.querySelector("[data-tx-brush]").hidden = true;
        return;
      }
      const from = Number(fig.dataset.from), to = Number(fig.dataset.to);
      const t = (x) => Math.round(from + (to - from) * (x / box.width));
      const form = document.querySelector("form.tx-filters");
      if (!form) return;
      const set = (name, value) => {
        let input = form.querySelector(`input[type=hidden][name=${name}]`);
        if (!input) {
          input = document.createElement("input");
          input.type = "hidden";
          input.name = name;
          form.append(input);
        }
        input.value = value;
      };
      set("from", t(Math.min(x0, x1)));
      set("to", t(Math.max(x0, x1)));
      const range = form.querySelector("select[name=range]");
      if (range) range.value = "";
      form.querySelector("input[name=minute]")?.remove();
      form.requestSubmit();
    });
  }

  // ---- other pages: embeds, and trace links that open inline -------------------------------------
  function initEmbeds(scope) {
    scope.querySelectorAll("[data-trace-embed]").forEach((box) => {
      if (box.closest("details:not([open])")) {
        box.closest("details").addEventListener("toggle", () => load(box), { once: true });
      } else {
        load(box);
      }
    });
    scope.querySelectorAll("details [data-wf-src]").forEach((box) => {
      const d = box.closest("details");
      if (d.closest("[data-tx-list]")) return;
      d.addEventListener("toggle", () => d.open && load(box));
    });
  }
  document.addEventListener("click", (e) => {
    const a = e.target.closest?.("a[data-trace-inline]");
    if (!a || e.metaKey || e.ctrlKey || e.shiftKey) return;
    e.preventDefault();
    const next = a.nextElementSibling;
    if (next?.classList.contains("trace-inline")) {
      next.remove();
      a.setAttribute("aria-expanded", "false");
      return;
    }
    const box = document.createElement("div");
    box.className = "trace-inline";
    box.dataset.traceEmbed = a.dataset.traceInline;
    a.after(box);
    a.setAttribute("aria-expanded", "true");
    load(box);
  });

  // Any live fragment with an open waterfall in it (the Requests page's drawers) holds still.
  document.addEventListener("toggle", (e) => {
    const live = e.target.closest?.("[data-live]");
    if (!live || live.matches("[data-tx-list]")) return;
    if (live.querySelector("details[data-inline-wf][open]")) live.setAttribute("data-live-hold", "");
    else live.removeAttribute("data-live-hold");
  }, true);

  // Live fragments replace the list: new rows and embeds need binding again.
  new MutationObserver((records) => {
    for (const r of records) {
      r.addedNodes.forEach((n) => {
        if (n.nodeType !== 1) return;
        n.querySelectorAll?.("[data-waterfall]").forEach((w) => initWaterfall(w, false));
        initEmbeds(n);
      });
    }
  }).observe(document.body, { childList: true, subtree: true });

  const pageWaterfalls = document.querySelectorAll("[data-waterfall]");
  pageWaterfalls.forEach((w) => initWaterfall(w, pageWaterfalls.length === 1));
  initExplorer();
  initEmbeds(document);
})();
