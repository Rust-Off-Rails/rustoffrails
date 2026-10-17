// static/dev-reload.js: development only (the layout loads it when ctx.dev)
(() => {
  let seen = null;
  const tick = async () => {
    if (document.visibilityState === "visible") {
      try {
        const r = await fetch("/dev/reload", { cache: "no-store", headers: { Accept: "application/json" } });
        const v = r.ok && r.headers.get("content-type")?.includes("json") ? (await r.json()).version : null;
        if (v && seen && v !== seen) return location.reload();
        if (v) seen = v;
      } catch {
        // the server is restarting: keep polling
      }
    }
    setTimeout(tick, 1000);
  };
  tick();
})();
