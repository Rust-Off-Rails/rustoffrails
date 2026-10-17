// static/channels.js: the offrails.v1 client. One connection per tab, many channels, any
// transport: WebSocket first, then Server-Sent Events + POST, then long polling, chosen
// automatically when a proxy blocks one. No framework, no build step, no inline script.
//
//   const ch = OffRails.socket().channel("room:lobby", { since: 41 });
//   ch.on("message", (data) => add(data));
//   new OffRails.Presence(ch).onSync((people) => render(people));
//   ch.push("post", { body: "hi" }).then(clear).catch((e) => show(e.message));
//   ch.join();
//
// Guide: .offrails/guides/channels.md#the-js-client
(() => {
  "use strict";
  if (window.OffRails) return;

  const PROTOCOL = "offrails.v1";
  const ORDER = ["websocket", "sse", "longpoll"];
  const BACKOFF = [250, 1000, 2000, 5000, 10000];
  const OPEN_TIMEOUT = 5000;
  const PING_AFTER = 25000;
  const PONG_WITHIN = 10000;
  const REPLY_TIMEOUT = 10000;
  const PROBE_EVERY = 60000;
  const STORE_KEY = "offrails:transport";

  // Every wait gets plus or minus 25%, so clients don't come back in lockstep after a restart.
  const jitter = (ms) => Math.round(ms * (0.75 + Math.random() * 0.5));
  const remember = (name) => { try { sessionStorage.setItem(STORE_KEY, name); } catch { /* storage blocked */ } };
  const remembered = () => { try { return sessionStorage.getItem(STORE_KEY); } catch { return null; } };
  const wsUrl = (path) => `${location.protocol === "https:" ? "wss" : "ws"}://${location.host}${path}`;

  // ---- transports: open(), send(text), close(); they report to the connection ----------------

  class WebSocketTransport {
    constructor(conn) { this.conn = conn; this.name = "websocket"; }
    open() {
      const ws = new WebSocket(wsUrl("/socket"), PROTOCOL);
      this.ws = ws;
      ws.onopen = () => this.conn.opened(this);
      ws.onmessage = (e) => this.conn.received(this, e.data);
      ws.onclose = (e) => this.conn.closed(this, e.code);
    }
    send(text) { if (this.ws?.readyState === WebSocket.OPEN) this.ws.send(text); }
    close() { this.closing = true; try { this.ws?.close(1000); } catch { /* already closed */ } }
  }

  // Frames go up as POSTs carrying the relay's token; same-origin fetches pass the CSRF check.
  const postFrame = (transport, path, text) => {
    fetch(path, {
      method: "POST",
      credentials: "same-origin",
      headers: { "content-type": "application/json", "x-offrails-token": transport.token },
      body: text,
    }).then((r) => { if (r.status !== 202) transport.fail(); }, () => transport.fail());
  };

  class SseTransport {
    constructor(conn) { this.conn = conn; this.name = "sse"; }
    open() {
      const es = new EventSource("/socket/sse");
      this.es = es;
      es.addEventListener("hello", (e) => {
        const hello = JSON.parse(e.data);
        this.id = hello.conn;
        this.token = hello.token;
        this.conn.opened(this);
      });
      es.addEventListener("frame", (e) => this.conn.received(this, e.data));
      // A reopened EventSource would be a new server connection without its joins: never let it
      // reconnect by itself; the connection reconnects and rejoins instead.
      es.onerror = () => this.fail();
    }
    send(text) { if (this.id) postFrame(this, `/socket/sse/${this.id}`, text); }
    fail() { if (this.done) return; this.done = true; this.es?.close(); this.conn.closed(this, 1006); }
    close() { this.closing = true; this.done = true; this.es?.close(); }
  }

  class LongPollTransport {
    constructor(conn) { this.conn = conn; this.name = "longpoll"; }
    async open() {
      this.abort = new AbortController();
      try {
        const r = await fetch("/socket/poll", { method: "POST", credentials: "same-origin", signal: this.abort.signal });
        // Signed out: stop, as after a revocation.
        if (r.status === 401) { this.done = true; return this.conn.revoke(); }
        if (!r.ok) return this.fail();
        const hello = await r.json();
        this.id = hello.conn;
        this.token = hello.token;
      } catch { return this.fail(); }
      this.conn.opened(this);
      while (!this.done) {
        try {
          const r = await fetch(`/socket/poll/${this.id}`, {
            credentials: "same-origin",
            headers: { "x-offrails-token": this.token },
            signal: this.abort.signal,
          });
          if (r.status === 401) { this.done = true; return this.conn.revoke(); }
          if (r.status !== 200) return this.fail();
          for (const frame of await r.json()) this.conn.received(this, frame);
        } catch { return this.fail(); }
      }
    }
    send(text) { if (this.id) postFrame(this, `/socket/poll/${this.id}`, text); }
    fail() { if (this.done) return; this.done = true; this.abort?.abort(); this.conn.closed(this, 1006); }
    close() { this.closing = true; this.done = true; this.abort?.abort(); }
  }

  const TRANSPORTS = { websocket: WebSocketTransport, sse: SseTransport, longpoll: LongPollTransport };

  // ---- the connection --------------------------------------------------------------------------

  class Connection {
    constructor() {
      this.channels = new Map();
      this.pending = new Map();
      this.statusFns = [];
      this.ref = 0;
      this.attempt = 0;
      this.transport = null;
      this.trying = null;
      this.revoked = false;
      this.lastHeard = Date.now();
      this.pinging = null;
      setInterval(() => this.heartbeat(), 5000);
    }

    get transportName() { return this.transport?.name ?? null; }

    onStatus(fn) { this.statusFns.push(fn); fn(this.status ?? "connecting", this.transportName); return this; }
    setStatus(status) {
      this.status = status;
      for (const fn of this.statusFns) fn(status, this.transportName);
    }

    channel(topic, opts = {}) {
      let ch = this.channels.get(topic);
      if (!ch) { ch = new Channel(this, topic, opts); this.channels.set(topic, ch); }
      return ch;
    }

    connect() {
      if (this.revoked || this.transport || this.trying || this.timer) return;
      const last = ORDER.indexOf(remembered());
      this.tryTransport(last >= 0 ? last : 0);
    }

    tryTransport(i) {
      if (this.revoked) return;
      const t = new TRANSPORTS[ORDER[i]](this);
      t.index = i;
      this.trying = t;
      this.setStatus(this.attempt === 0 ? "connecting" : "reconnecting");
      // A transport that doesn't open in time, or closes first, is blocked here: try the next.
      t.openTimer = setTimeout(() => { if (this.trying === t) { t.close(); this.blocked(t); } }, OPEN_TIMEOUT);
      t.open();
    }

    blocked(t) {
      clearTimeout(t.openTimer);
      if (this.trying !== t) return;
      this.trying = null;
      if (t.index + 1 < ORDER.length) this.tryTransport(t.index + 1);
      else this.scheduleReconnect(0);
    }

    opened(t) {
      clearTimeout(t.openTimer);
      if (t.probe) return this.upgraded(t);
      if (this.trying !== t) { t.close(); return; }
      this.trying = null;
      this.transport = t;
      this.attempt = 0;
      this.lastHeard = Date.now();
      remember(t.name);
      this.setStatus("open");
      for (const ch of this.channels.values()) if (ch.wanted) ch.rejoin();
      if (t.name !== "websocket") this.scheduleProbe();
    }

    // On a fallback, try the WebSocket again now and then; when it opens, move every channel over
    // (each resumes after its cursor).
    scheduleProbe() {
      clearTimeout(this.probeTimer);
      this.probeTimer = setTimeout(() => {
        if (!this.transport || this.transport.name === "websocket" || this.revoked) return;
        const probe = new WebSocketTransport(this);
        probe.probe = true;
        probe.openTimer = setTimeout(() => { probe.close(); this.scheduleProbe(); }, OPEN_TIMEOUT);
        probe.open();
      }, jitter(PROBE_EVERY));
    }

    upgraded(probe) {
      probe.probe = false;
      const old = this.transport;
      this.transport = probe;
      if (old) old.close();
      remember("websocket");
      this.failPending("unavailable", "Reconnected; try again.");
      this.setStatus("open");
      for (const ch of this.channels.values()) if (ch.wanted) ch.rejoin();
    }

    closed(t, code) {
      clearTimeout(t.openTimer);
      if (t.probe) { this.scheduleProbe(); return; }
      if (this.trying === t) { this.blocked(t); return; }
      if (this.transport !== t) return;
      this.transport = null;
      this.failPending("unavailable", "Connection lost; try again.");
      for (const ch of this.channels.values()) ch.dropped();
      if (code === 4401 || this.revoked) { this.revoke(); return; }
      const bye = this.lastBye;
      this.lastBye = null;
      // Over the connection cap: wait longest before trying again.
      this.scheduleReconnect(bye?.code === "limit" ? BACKOFF.length - 1 : this.attempt);
    }

    scheduleReconnect(step) {
      if (this.revoked) return;
      this.attempt = Math.max(this.attempt, step) + 1;
      this.setStatus("reconnecting");
      const wait = jitter(BACKOFF[Math.min(this.attempt - 1, BACKOFF.length - 1)]);
      this.timer = setTimeout(() => { this.timer = null; this.connect(); }, wait);
    }

    revoke() {
      this.revoked = true;
      if (this.trying) { clearTimeout(this.trying.openTimer); this.trying.close(); this.trying = null; }
      this.transport?.close();
      this.transport = null;
      this.setStatus("revoked");
      window.dispatchEvent(new CustomEvent("offrails:revoked"));
    }

    failPending(code, message) {
      for (const [, p] of this.pending) { clearTimeout(p.timer); p.reject({ code, message }); }
      this.pending.clear();
    }

    // Browsers hide WebSocket pings, so the client asks after a quiet spell; a missing answer
    // means the server is gone: reconnect.
    heartbeat() {
      const t = this.transport;
      if (!t || this.pinging) return;
      if (Date.now() - this.lastHeard < PING_AFTER) return;
      this.pinging = this.request({ t: "ping" }, PONG_WITHIN)
        .catch(() => { if (this.transport === t) { t.close(); this.closed(t, 1006); } })
        .finally(() => { this.pinging = null; });
    }

    request(frame, timeout = REPLY_TIMEOUT) {
      return new Promise((resolve, reject) => {
        const t = this.transport;
        if (!t) { reject({ code: "unavailable", message: "Not connected." }); return; }
        const ref = String(++this.ref);
        const timer = setTimeout(() => {
          this.pending.delete(ref);
          reject({ code: "timeout", message: "No answer; try again." });
        }, timeout);
        this.pending.set(ref, { resolve, reject, timer });
        t.send(JSON.stringify({ ...frame, ref }));
      });
    }

    received(t, raw) {
      if (t !== this.transport) return;
      this.lastHeard = Date.now();
      let msg;
      try { msg = typeof raw === "string" ? JSON.parse(raw) : raw; } catch { return; }
      if (!msg || typeof msg !== "object") return;
      const reply = msg.ref != null ? this.pending.get(String(msg.ref)) : null;
      switch (msg.t) {
        case "ok":
        case "pong":
          if (reply) { this.pending.delete(String(msg.ref)); clearTimeout(reply.timer); reply.resolve(msg); }
          break;
        case "err":
          if (reply) {
            this.pending.delete(String(msg.ref)); clearTimeout(reply.timer);
            reply.reject({ code: msg.code, message: msg.message });
          } else if (msg.topic) {
            this.channels.get(msg.topic)?.kicked(msg);
          }
          break;
        case "event": this.channels.get(msg.topic)?.event(msg); break;
        case "presence": this.channels.get(msg.topic)?.presence(msg); break;
        case "bye":
          this.lastBye = msg;
          if (msg.code === "revoked") this.revoked = true;
          break;
        default: break;
      }
    }
  }

  // ---- channels --------------------------------------------------------------------------------

  class Channel {
    constructor(conn, topic, opts) {
      this.conn = conn;
      this.topic = topic;
      this.since = Number.isFinite(opts.since) ? opts.since : null;
      this.handlers = new Map();
      this.state = "closed";
      this.wanted = false;
    }

    on(event, fn) {
      if (!this.handlers.has(event)) this.handlers.set(event, []);
      this.handlers.get(event).push(fn);
      return this;
    }
    onJoin(fn) { return this.on("offrails:join", fn); }
    onError(fn) { return this.on("offrails:error", fn); }
    emit(event, ...args) { for (const fn of this.handlers.get(event) ?? []) fn(...args); }

    join() {
      this.wanted = true;
      if (this.conn.transport) this.rejoin();
      else this.conn.connect();
      return this;
    }

    rejoin() {
      clearTimeout(this.retry);
      this.state = "joining";
      this.emit("offrails:joining");
      const frame = { t: "join", topic: this.topic };
      if (this.since != null) frame.since = this.since;
      this.conn.request(frame).then((reply) => {
        this.state = "joined";
        if (reply.gap) this.emit("gap");
        if (this.since == null && Number.isFinite(reply.data?.seq)) this.since = reply.data.seq;
        this.emit("offrails:join", reply.data);
      }, (err) => {
        this.state = "errored";
        this.emit("offrails:error", err);
        // Refused for good: stop asking. Busy or cut off: ask again in a moment.
        if (["forbidden", "unknown_topic", "limit"].includes(err.code)) { this.wanted = false; return; }
        if (err.code !== "unavailable" || this.conn.transport) this.retry = setTimeout(() => { if (this.wanted && this.conn.transport) this.rejoin(); }, jitter(2000));
      });
    }

    leave() {
      this.wanted = false;
      clearTimeout(this.retry);
      if (this.state === "joined" && this.conn.transport) this.conn.request({ t: "leave", topic: this.topic }).catch(() => {});
      this.state = "closed";
      this.conn.channels.delete(this.topic);
    }

    push(event, data = {}) {
      if (this.state !== "joined") return Promise.reject({ code: "not_joined", message: "Not connected yet; try again." });
      return this.conn.request({ t: "push", topic: this.topic, event, data }).then((reply) => reply.data);
    }

    dropped() { if (this.state !== "closed") this.state = "dropped"; this.emit("offrails:dropped"); }

    kicked(msg) {
      this.state = "closed";
      this.wanted = false;
      this.emit("offrails:error", { code: msg.code, message: msg.message });
    }

    event(msg) {
      // Replays and live events may overlap: anything at or below the cursor was already seen.
      if (msg.seq != null) {
        if (this.since != null && msg.seq <= this.since) return;
        this.since = msg.seq;
      }
      if (msg.event === "refresh") refreshRegion(msg.data?.region);
      this.emit(msg.event, msg.data, msg);
    }

    presence(msg) { this.emit("offrails:presence", msg); }
  }

  // ---- presence ---------------------------------------------------------------------------------

  // Who is here, per person (tabs and devices of one person are one entry). After every (re)join,
  // diffs wait until that join's snapshot arrives, then apply on top of it; applying a diff twice
  // changes nothing.
  class Presence {
    constructor(channel) {
      this.state = {};
      this.synced = false;
      this.buffer = [];
      this.fns = [];
      channel.on("offrails:joining", () => { this.synced = false; this.buffer = []; });
      channel.on("offrails:presence", (msg) => this.frame(msg));
    }
    onSync(fn) { this.fns.push(fn); return this; }
    list() { return Object.entries(this.state).map(([key, meta]) => ({ key, ...meta })); }
    apply(diff) {
      for (const [key, meta] of Object.entries(diff.joins ?? {})) this.state[key] = meta;
      for (const key of Object.keys(diff.leaves ?? {})) delete this.state[key];
    }
    frame(msg) {
      if (msg.here) {
        this.state = { ...msg.here };
        for (const diff of this.buffer) this.apply(diff);
        this.buffer = [];
        this.synced = true;
      } else if (!this.synced) {
        this.buffer.push(msg);
        return;
      } else {
        this.apply(msg);
      }
      const people = this.list();
      for (const fn of this.fns) fn(people);
    }
  }

  // ---- htmx refreshes ---------------------------------------------------------------------------

  // channels::refresh(topic, region) on the server: every element with data-live-region="<region>"
  // re-fetches itself through its normal, authorized GET (data-src).
  const refreshRegion = (region) => {
    if (typeof region !== "string" || !/^[a-z0-9-]{1,64}$/.test(region) || !window.htmx) return;
    for (const el of document.querySelectorAll(`[data-live-region="${region}"]`)) {
      if (el.dataset.src) window.htmx.ajax("GET", el.dataset.src, { target: el, swap: "outerHTML", headers: { "X-Live-Refresh": "1" } });
    }
  };

  let shared = null;
  const socket = () => (shared ??= new Connection());

  window.OffRails = { socket, Connection, Channel, Presence, transports: ORDER, refreshRegion };
})();
