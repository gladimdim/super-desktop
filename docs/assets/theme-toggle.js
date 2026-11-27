/* Syncs dark/light terminal mode (data-mode + sd-site-mode) across all pages. */
(function () {
  "use strict";
  var root = document.documentElement;
  try {
    var saved = localStorage.getItem("sd-site-mode");
    var mode = (saved === "light" || saved === "dark")
      ? saved
      : (window.matchMedia && window.matchMedia("(prefers-color-scheme: light)").matches ? "light" : "dark");
    root.dataset.mode = mode;
  } catch (e) {
    root.dataset.mode = "dark";
  }

  document.addEventListener("DOMContentLoaded", function () {
    var btn = document.getElementById("mode-toggle");
    if (!btn) return;
    function sync(mode) {
      root.dataset.mode = mode;
      var isLight = mode === "light";
      btn.textContent = isLight ? "🌙" : "☀️";
      btn.setAttribute("aria-pressed", String(isLight));
      btn.title = isLight ? "Switch to dark terminal theme" : "Switch to white terminal theme";
    }
    sync(root.dataset.mode === "light" ? "light" : "dark");
    btn.addEventListener("click", function () {
      var next = root.dataset.mode === "light" ? "dark" : "light";
      try { localStorage.setItem("sd-site-mode", next); } catch (e) {}
      sync(next);
    });
    if (window.matchMedia) {
      var mq = window.matchMedia("(prefers-color-scheme: light)");
      mq.addEventListener("change", function (ev) {
        try {
          if (localStorage.getItem("sd-site-mode")) return;
        } catch (e) {}
        sync(ev.matches ? "light" : "dark");
      });
    }
  });
})();
