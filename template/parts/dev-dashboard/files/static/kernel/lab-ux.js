// The load lab's UX trouble pages (lab.rs): the button that throws, and the self-playing
// simulation (?simulate=1) the Frontend tab's demo and tests use. Loaded in the page body, so
// it runs after a boosted navigation too; event delegation, guarded against running twice.
(function () {
  "use strict";
  if (!window.offrailsLabUx) {
    window.offrailsLabUx = true;
    document.addEventListener("click", function (e) {
      if (e.target.closest && e.target.closest("[data-lab-throw]")) {
        throw new Error("lab: the coupon service returned an unexpected shape");
      }
    });
  }
  function wait(ms) { return new Promise(function (r) { setTimeout(r, ms); }); }
  function burst(el, n, gap) {
    var p = Promise.resolve();
    for (var i = 0; i < n; i++) p = p.then(function () { el.click(); return wait(gap); });
    return p;
  }
  function play() {
    var root = document.querySelector("[data-lab-simulate]");
    if (!root || root.dataset.played) return;
    root.dataset.played = "1";
    var $ = function (s) { return document.querySelector(s); };
    var p = wait(400);
    if ($("#slow-report")) {
      p = p.then(function () { return burst($("#slow-report"), 6, 90); })
        .then(function () { return wait(2500); })
        .then(function () { return burst($("#save-draft"), 4, 120); })
        .then(function () { return wait(1500); })
        .then(function () { return burst($("#save-draft"), 4, 120); })
        .then(function () { return wait(1500); })
        .then(function () { return burst($("#save-draft"), 3, 150); })
        .then(function () { return wait(1500); })
        .then(function () { try { $("#apply-coupon").click(); } catch (e) { /* reported by the beacon */ } return wait(1500); })
        .then(function () { var a = $("#to-widget"); if (a) a.click(); });
    } else if ($("#show-code")) {
      p = p.then(function () { return wait(800); })
        .then(function () { return burst($("#show-code"), 5, 110); })
        .then(function () { return wait(1500); })
        .then(function () { return burst($("#show-code"), 4, 300); })
        .then(function () { return wait(1500); })
        .then(function () { return burst($("#show-code"), 3, 150); })
        .then(function () { return wait(1500); })
        .then(function () { document.dispatchEvent(new Event("visibilitychange")); });
    }
  }
  document.addEventListener("htmx:load", play);
  if (document.readyState !== "loading") play(); else document.addEventListener("DOMContentLoaded", play);
})();
