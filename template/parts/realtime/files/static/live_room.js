// static/live_room.js: the live room page (templates/live/show.html) on top of channels.js.
// Messages, presence and toasts are built by cloning <template> rows and filling them with
// textContent: text people typed is never parsed as HTML.
(() => {
  "use strict";
  const TYPING_EVERY = 3000;
  const TYPING_SHOWN = 5000;

  const start = (root) => {
    if (!window.OffRails || root.dataset.started) return;
    root.dataset.started = "1";
    const $ = (sel) => root.querySelector(sel);
    const io = window.OffRails.socket();
    const me = root.dataset.me;
    const list = $("[data-messages]");
    const palette = Array.from(root.querySelectorAll("[data-palette] li"), (li) => li.className);
    const avatarBase = $("[data-message-template]").content.querySelector("[data-f=avatar]").className
      .split(" ").filter((c) => !palette.includes(c)).join(" ");
    const errorBox = $("[data-error]");
    const showError = (message) => { errorBox.textContent = message || ""; };

    const localTimes = (scope) => {
      for (const t of scope.querySelectorAll("time[datetime]")) {
        const d = new Date(t.getAttribute("datetime"));
        if (!Number.isNaN(d.getTime())) t.textContent = d.toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" });
      }
    };
    localTimes(list);
    const nearBottom = () => list.scrollHeight - list.scrollTop - list.clientHeight < 80;
    list.scrollTop = list.scrollHeight;

    // ---- messages
    const add = (m) => {
      if (document.getElementById(`message-${m.id}`)) return;
      const row = $("[data-message-template]").content.firstElementChild.cloneNode(true);
      row.id = `message-${m.id}`;
      row.dataset.id = String(m.id);
      const avatar = row.querySelector("[data-f=avatar]");
      avatar.className = `${avatarBase} ${palette[m.color] ?? palette[0]}`;
      avatar.textContent = m.initials;
      row.querySelector("[data-f=author]").textContent = m.author;
      row.querySelector("[data-f=body]").textContent = m.body;
      row.querySelector("[data-f=at]").setAttribute("datetime", m.at);
      localTimes(row);
      const stick = nearBottom() || String(m.author_id) === me;
      list.append(row);
      $("[data-empty]")?.remove();
      typists.delete(String(m.author_id));
      renderTyping();
      if (stick) list.scrollTop = list.scrollHeight;
    };

    // ---- typing
    const typists = new Map();
    const typingLine = $("[data-typing]");
    const renderTyping = () => {
      const now = Date.now();
      for (const [key, t] of typists) if (t.until < now) typists.delete(key);
      const names = Array.from(typists.values(), (t) => t.name);
      typingLine.textContent = names.length === 0 ? ""
        : names.length === 1 ? `${names[0]} is typing…`
        : names.length === 2 ? `${names[0]} and ${names[1]} are typing…`
        : "Several people are typing…";
    };
    setInterval(renderTyping, 1000);

    // ---- presence
    const people = $("[data-presence]");
    const count = $("[data-count]");
    const renderPeople = (here) => {
      here.sort((a, b) => (a.key === me ? -1 : b.key === me ? 1 : String(a.name).localeCompare(String(b.name))));
      people.replaceChildren(...here.map((p) => {
        const li = $("[data-person-template]").content.firstElementChild.cloneNode(true);
        li.dataset.key = p.key;
        const avatar = li.querySelector("[data-f=avatar]");
        avatar.classList.add(...(palette[p.color] ?? palette[0]).split(" "));
        li.querySelector("[data-f=initials]").textContent = p.initials ?? "?";
        li.querySelector("[data-f=name]").textContent = p.key === me ? `${p.name} (you)` : p.name;
        const wave = li.querySelector("[data-wave]");
        if (p.key === me) wave.remove();
        else wave.setAttribute("aria-label", `Wave at ${p.name}`);
        return li;
      }));
      count.textContent = `(${here.length})`;
    };

    // ---- toasts
    const toast = (text) => {
      const el = $("[data-toast-template]").content.firstElementChild.cloneNode(true);
      el.textContent = text;
      $("[data-toasts]").append(el);
      setTimeout(() => el.remove(), 4000);
    };

    // ---- connection state
    const statusText = $("[data-status]");
    const dot = $("[data-status-dot]");
    const dotColors = Array.from(root.querySelectorAll("[data-status-colors] [data-state]"));
    const LABELS = { connecting: "Connecting…", open: "Live", reconnecting: "Reconnecting…", revoked: "Signed out" };
    io.onStatus((status, transport) => {
      statusText.textContent = status === "open" && transport !== "websocket" ? `Live (${transport})` : LABELS[status] ?? status;
      for (const c of dotColors) dot.classList.remove(...c.className.split(" "));
      const color = dotColors.find((c) => c.dataset.state === status);
      if (color) dot.classList.add(...color.className.split(" "));
      root.dataset.status = status;
      root.dataset.transport = transport ?? "";
    });

    // ---- channels
    const room = io.channel(root.dataset.topic, { since: Number(root.dataset.since) });
    room.on("message", add);
    room.on("typing", (t) => { typists.set(t.key, { name: t.name, until: Date.now() + TYPING_SHOWN }); renderTyping(); });
    room.on("gap", () => window.location.reload());
    room.onJoin(() => showError(""));
    room.onError((e) => showError(e.message));
    new window.OffRails.Presence(room).onSync(renderPeople);
    const personal = io.channel(root.dataset.userTopic);
    personal.on("waved", (w) => toast(`${w.from} waved at you`));

    const form = $("[data-composer]");
    const input = form.querySelector("textarea");
    let lastTyping = 0;
    input.addEventListener("input", () => {
      if (!input.value.trim() || Date.now() - lastTyping < TYPING_EVERY) return;
      lastTyping = Date.now();
      room.push("typing", {}).catch(() => {});
    });
    input.addEventListener("keydown", (e) => {
      if (e.key === "Enter" && !e.shiftKey && !e.isComposing) { e.preventDefault(); form.requestSubmit(); }
    });
    form.addEventListener("submit", (e) => {
      e.preventDefault();
      const body = input.value.trim();
      if (!body) return;
      showError("");
      room.push("post", { body }).then(() => { input.value = ""; lastTyping = 0; }, (err) => showError(err.message));
    });
    people.addEventListener("click", (e) => {
      const button = e.target.closest("[data-wave]");
      if (!button) return;
      const li = button.closest("[data-key]");
      const name = li.querySelector("[data-f=name]").textContent;
      room.push("wave", { to: Number(li.dataset.key) }).then(() => toast(`You waved at ${name}`), (err) => showError(err.message));
    });

    // Leaving the page through a boosted link: leave the topics, so presence updates for the others.
    const leave = (e) => {
      if (!e.detail.boosted || !root.isConnected) return;
      room.leave();
      personal.leave();
      document.removeEventListener("htmx:beforeSwap", leave);
    };
    document.addEventListener("htmx:beforeSwap", leave);

    room.join();
    personal.join();
  };

  const boot = () => document.querySelectorAll("[data-live-room]").forEach(start);
  document.addEventListener("DOMContentLoaded", boot);
  document.addEventListener("htmx:load", boot);
  // The session ended (signed out here or in another tab): this page can't continue.
  window.addEventListener("offrails:revoked", () => {
    if (document.querySelector("[data-live-room]")) window.location.assign("/live");
  });
})();
