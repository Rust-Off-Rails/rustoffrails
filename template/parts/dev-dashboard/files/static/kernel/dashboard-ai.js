// Ask AI in the browser: the panel loads on first open, a question starts a run (POST ai/runs),
// and the run streams its steps and its end over Server-Sent Events. Everything the server sends
// is HTML it rendered with Askama (every model-written string escaped there); this file only
// places it. Without JavaScript the same forms post to ai/ask and the page shows the finished run.
//
// Loaded on every dashboard page and on the kernel pages with the Ask AI bar.
(() => {
  // The server builds and stores the context when the panel loads, so the preview shows exactly
  // what the first call sends (the typed question goes in its slot, here and on the server).
  const SLOT = "{your question}";

  // ---- a run's live stream: `step` events append, `end` fills the answer, then it closes ----
  const follow = (run) => {
    const src = run.dataset.stream;
    if (!src || run.dataset.following) return;
    run.dataset.following = "1";
    const steps = run.querySelector("[data-ai-steps]");
    const end = run.querySelector("[data-ai-end]");
    const status = run.querySelector("[data-ai-status-text]");
    const es = new EventSource(src);
    es.addEventListener("step", (e) => {
      steps.insertAdjacentHTML("beforeend", e.data);
      const title = steps.lastElementChild?.querySelector(".ai-step-title");
      if (status && title) status.textContent = title.textContent;
    });
    es.addEventListener("end", (e) => {
      es.close();
      end.innerHTML = e.data;
      run.querySelector("[data-ai-status] .spinner")?.remove();
      const stopped = end.querySelector(".ai-stopped");
      if (status) status.textContent = end.querySelector("[role=alert]") ? "Failed" : stopped ? stopped.textContent : "Answered";
      run.classList.remove("st-running");
      run.classList.add(end.querySelector("[role=alert]") ? "st-failed" : stopped ? "st-stopped" : "st-done");
      bindCopy(end);
      end.querySelectorAll("[data-ai-continue]").forEach(bindContinue);
    });
    es.addEventListener("error", () => {
      // EventSource reconnects by itself (the server resumes after the last step it sent); once
      // closed for good, point at the run's page.
      if (es.readyState === EventSource.CLOSED && status) status.textContent = "Connection lost: open the permalink.";
    });
  };

  // ---- "Copy fix prompt" ----
  const bindCopy = (scope) => {
    scope.querySelectorAll("[data-copy]").forEach((b) => {
      if (b.dataset.bound) return;
      b.dataset.bound = "1";
      b.hidden = !navigator.clipboard;
      b.addEventListener("click", async () => {
        const text = document.getElementById(b.dataset.copy)?.textContent ?? "";
        const done = b.parentElement.querySelector("[data-copied]");
        try {
          await navigator.clipboard.writeText(text);
          if (done) done.textContent = "Copied";
        } catch {
          if (done) done.textContent = "Couldn't copy: select the text below.";
        }
      });
    });
  };

  // ---- a form that starts a run (the panel's question, the overview's health summary) ----
  const bindForm = (form) => {
    if (form.dataset.bound) return;
    form.dataset.bound = "1";
    const scope = form.closest("[data-ai-scope]") || form.parentElement;
    const answer = scope.querySelector("[data-ai-answer]");
    const q = form.querySelector("[data-ai-question]");
    const pre = form.querySelector("[data-ai-preview]");
    const busy = form.querySelector("[data-ai-busy]");
    const send = form.querySelector("[data-ai-send]");
    const original = pre ? pre.textContent : "";
    const update = () => {
      if (!q || !pre) return;
      const text = q.value.trim();
      pre.textContent = text ? original.replace(SLOT, JSON.stringify(text).slice(1, -1)) : original;
    };
    q?.addEventListener("input", update);
    scope.querySelectorAll("[data-ai-prompt]").forEach((b) =>
      b.addEventListener("click", () => {
        q.value = b.textContent;
        update();
        q.focus();
      }),
    );
    if (!answer || !form.dataset.runs) return; // the no-JS page posts the form normally
    form.addEventListener("submit", async (e) => {
      e.preventDefault();
      if (q && !q.value.trim()) {
        q.focus();
        return;
      }
      send.disabled = true;
      if (busy) busy.hidden = false;
      try {
        const res = await fetch(form.dataset.runs, {
          method: "POST",
          body: new URLSearchParams(new FormData(form)),
          headers: { "x-dashboard-fragment": "1" },
        });
        // A fragment rendered by Askama: every model-written string is escaped on the server.
        answer.innerHTML = await res.text();
        answer.querySelectorAll("[data-ai-run]").forEach(follow);
        bindCopy(answer);
      } catch {
        answer.textContent = "Couldn't reach the server.";
      } finally {
        send.disabled = false;
        if (busy) busy.hidden = true;
      }
    });
  };

  // ---- "Continue the answer": the same run, one more call; its fragment replaces the run ----
  const bindContinue = (form) => {
    if (form.dataset.bound) return;
    form.dataset.bound = "1";
    form.addEventListener("submit", async (e) => {
      e.preventDefault();
      const run = form.closest("[data-ai-run]");
      const button = form.querySelector("button");
      if (button) button.disabled = true;
      try {
        const res = await fetch(form.action, { method: "POST", headers: { "x-dashboard-fragment": "1" } });
        if (!res.ok) throw new Error(String(res.status));
        // Rendered by Askama on the server: every model-written string is escaped there.
        const box = document.createElement("div");
        box.innerHTML = await res.text();
        const next = box.querySelector("[data-ai-run]");
        if (run && next) {
          run.replaceWith(next);
          follow(next);
          bindAll(next);
        }
      } catch {
        form.submit();
      }
    });
  };

  const bindAll = (scope) => {
    scope.querySelectorAll("[data-ai-continue]").forEach(bindContinue);
    scope.querySelectorAll("[data-ai-form]").forEach(bindForm);
    scope.querySelectorAll("[data-ai-run][data-stream]").forEach(follow);
    bindCopy(scope);
  };

  // ---- the panel (a dashboard page's, or the bar on another kernel page) ----
  // A dashboard tab's panel is open on its first visit in this browser session, then starts
  // closed (the server renders it open, so it is open without JavaScript too).
  const seenBefore = (tab) => {
    try {
      const key = `dashboard-ai-seen:${tab}`;
      if (sessionStorage.getItem(key)) return true;
      sessionStorage.setItem(key, "1");
    } catch {
      // Storage blocked: every visit counts as the first.
    }
    return false;
  };
  document.querySelectorAll("[data-ai-panel]").forEach((details) => {
    const body = details.querySelector("[data-ai-body]");
    let loaded = false;
    if (details.dataset.aiFirst && details.open && seenBefore(details.dataset.aiFirst)) details.open = false;
    const load = async () => {
      if (!details.open || loaded) return;
      loaded = true;
      body.setAttribute("aria-busy", "true");
      try {
        const res = await fetch(details.dataset.src, { headers: { "x-dashboard-fragment": "1" } });
        if (!res.ok) throw new Error(String(res.status));
        body.innerHTML = await res.text();
        bindAll(body);
      } catch {
        loaded = false; // the "Open the assistant" link stays
      } finally {
        body.removeAttribute("aria-busy");
      }
    };
    details.addEventListener("toggle", load);
    load();
  });
  bindAll(document);
})();
