// static/kernel/mailbox.js: the dev mailbox's behavior. Loaded by every mailbox page through its
// Chrome (kernel/mailbox_web.rs), never by the app layout. No inline script anywhere (the CSP is
// script-src 'self'); everything works without it too, just without live updates and shortcuts.
(() => {
  const root = document.querySelector("[data-mailbox]");
  if (!root) return;
  const $ = (sel, el = document) => el.querySelector(sel);
  const $$ = (sel, el = document) => Array.from(el.querySelectorAll(sel));
  const announce = (text) => {
    const region = $("[data-announce]");
    if (region) region.textContent = text;
  };
  const baseTitle = document.title.replace(/^\(\d+\) /, "");
  const setUnread = (n) => {
    const badge = $("[data-unread-count]");
    if (badge) badge.textContent = n > 0 ? String(n) : "";
    document.title = n > 0 ? `(${n}) ${baseTitle}` : baseTitle;
  };
  setUnread(Number($("[data-unread-count]")?.textContent || 0));

  // ---- live inbox (Server-Sent Events) ------------------------------------------------------
  // WHY: one EventSource per tab, opened by this script (never by editing an attribute), and the
  // list is re-fetched as a fragment: the stream only says "something changed" and carries the
  // newest id as its cursor, so a reconnect resumes where it left off.
  const status = $("[data-live-status]");
  const statusText = $("[data-live-text]");
  const toggle = $("[data-live-toggle]");
  const list = $("[data-list]");
  let since = Number(root.dataset.since || 0);
  let source = null;
  let paused = false;
  try { paused = localStorage.getItem("mailbox:paused") === "1"; } catch { /* storage blocked */ }

  const setStatus = (state, text) => {
    if (!status) return;
    status.classList.toggle("is-live", state === "live");
    status.classList.toggle("is-error", state === "error");
    statusText.textContent = text;
  };

  const refreshList = async (newest) => {
    if (!list || !root.dataset.listSrc) return;
    const res = await fetch(root.dataset.listSrc, { headers: { Accept: "text/html" }, cache: "no-store" });
    if (!res.ok) return;
    const html = await res.text();
    const before = new Set($$(".mbx-item", list).map((li) => li.dataset.id));
    list.innerHTML = html;
    let arrived = 0;
    for (const li of $$(".mbx-item", list)) {
      if (!before.has(li.dataset.id) && Number(li.dataset.id) > since) {
        li.classList.add("is-new");
        arrived += 1;
      }
    }
    const total = res.headers.get("x-mailbox-total");
    const unread = res.headers.get("x-mailbox-unread");
    if (total !== null) {
      const t = $("[data-total]");
      if (t) t.textContent = total;
    }
    if (unread !== null) setUnread(Number(unread));
    if (arrived > 0) announce(`${arrived} new message${arrived === 1 ? "" : "s"}`);
    since = Math.max(since, newest);
  };

  const connect = () => {
    if (paused || source || !list) return;
    source = new EventSource(`/dev/mailbox/events?since=${since}`);
    source.addEventListener("open", () => setStatus("live", "Live"));
    source.addEventListener("mailbox", (event) => {
      try {
        const data = JSON.parse(event.data);
        refreshList(data.newest);
      } catch { /* a malformed event: the next one carries the full state */ }
    });
    source.addEventListener("error", () => setStatus("error", "Reconnecting…"));
  };
  const disconnect = () => {
    source?.close();
    source = null;
  };
  if (toggle && list) {
    toggle.hidden = false;
    const render = () => {
      toggle.textContent = paused ? "Resume" : "Pause";
      toggle.setAttribute("aria-pressed", String(paused));
      if (paused) setStatus("paused", "Paused");
    };
    toggle.addEventListener("click", () => {
      paused = !paused;
      try { localStorage.setItem("mailbox:paused", paused ? "1" : "0"); } catch { /* fine */ }
      if (paused) disconnect(); else connect();
      render();
    });
    render();
  }
  connect();
  window.addEventListener("pagehide", disconnect);

  // ---- copy buttons -------------------------------------------------------------------------
  const copy = async (text, button) => {
    try {
      await navigator.clipboard.writeText(text);
    } catch {
      const area = document.createElement("textarea");
      area.value = text;
      document.body.append(area);
      area.select();
      document.execCommand("copy");
      area.remove();
    }
    if (button) {
      const label = button.textContent;
      button.textContent = "Copied";
      button.classList.add("is-copied");
      setTimeout(() => { button.textContent = label; button.classList.remove("is-copied"); }, 1500);
    }
    announce("Copied to the clipboard");
  };
  document.addEventListener("click", (event) => {
    const button = event.target.closest("[data-copy], [data-copy-from]");
    if (!button) return;
    const text = button.dataset.copy ?? document.getElementById(button.dataset.copyFrom)?.textContent ?? "";
    copy(text, button);
  });

  // ---- links sent from another local address --------------------------------------------------
  // A message sent while the app ran on another port links there. Offer the same path here.
  for (const a of $$("[data-link]")) {
    let url;
    try { url = new URL(a.href); } catch { continue; }
    const local = ["localhost", "127.0.0.1", "[::1]", "0.0.0.0"].includes(url.hostname);
    if (local && url.origin !== location.origin) {
      const here = document.createElement("a");
      here.href = location.origin + url.pathname + url.search + url.hash;
      here.textContent = "Open here";
      here.className = "mbx-here";
      here.target = "_blank";
      here.rel = "noopener noreferrer";
      here.title = `Sent for ${url.origin}; open on ${location.origin}`;
      a.after(here);
    }
  }

  // ---- keyboard ----------------------------------------------------------------------------
  const keys = $("[data-keys]");
  const keysOpen = $("[data-keys-open]");
  if (keys && keysOpen && typeof keys.showModal === "function") {
    keysOpen.hidden = false;
    keysOpen.addEventListener("click", () => keys.showModal());
  }
  const typing = (el) => el && (el.isContentEditable || ["INPUT", "TEXTAREA", "SELECT"].includes(el.tagName));
  const move = (step) => {
    const links = $$(".mbx-item a");
    if (links.length === 0) return;
    const at = links.findIndex((a) => a.getAttribute("aria-current") === "true");
    const next = links[Math.min(Math.max(at + step, 0), links.length - 1)];
    if (next && next.getAttribute("aria-current") !== "true") location.href = next.href;
  };
  document.addEventListener("keydown", (event) => {
    if (event.metaKey || event.ctrlKey || event.altKey || typing(event.target) || keys?.open) return;
    const tabs = $$(".mbx-seg")[0]?.querySelectorAll("a") ?? [];
    switch (event.key) {
      case "j": move(1); break;
      case "k": move(-1); break;
      case "/": event.preventDefault(); $("[data-search]")?.focus(); break;
      case "c": { const b = $(".mbx-codes [data-copy]"); if (b) copy(b.dataset.copy, b); break; }
      case "o": { const a = $("[data-link]"); if (a) window.open(a.href, "_blank", "noopener,noreferrer"); break; }
      case "e": $("[data-key-eml]")?.click(); break;
      case "d": $("[data-key-delete]")?.requestSubmit(); break;
      case "1": case "2": case "3": case "4": tabs[Number(event.key) - 1]?.click(); break;
      case "?": keys?.showModal?.(); break;
      default: return;
    }
  });
})();
