// Mockup-only theme preference for the Lore Portal pages: ?theme=light|dark, else the saved choice, else the OS setting.
(function () {
  var KEY = "lore.theme";
  var params = new URLSearchParams(location.search);
  var theme = params.get("theme");
  if (theme !== "light" && theme !== "dark") {
    try { theme = localStorage.getItem(KEY); } catch (e) { theme = null; }
  }
  if (theme !== "light" && theme !== "dark") {
    theme = matchMedia("(prefers-color-scheme: dark)").matches ? "dark" : "light";
  }
  document.documentElement.dataset.theme = theme;

  var MOON = '<path d="M20 14.5A8 8 0 0 1 9.5 4a8 8 0 1 0 10.5 10.5z"></path>';
  var SUN = '<circle cx="12" cy="12" r="4"></circle><path d="M12 2v2M12 20v2M4.9 4.9l1.4 1.4M17.7 17.7l1.4 1.4M2 12h2M20 12h2M4.9 19.1l1.4-1.4M17.7 6.3l1.4-1.4"></path>';
  function icon(body) {
    return '<svg class="i" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.7" stroke-linecap="round" stroke-linejoin="round">' + body + "</svg>";
  }

  document.addEventListener("DOMContentLoaded", function () {
    var bar = document.querySelector(".topbar");
    if (!bar) return;
    var btn = document.createElement("button");
    btn.className = "top-btn";
    btn.type = "button";
    function paint() {
      var dark = document.documentElement.dataset.theme === "dark";
      btn.innerHTML = icon(dark ? SUN : MOON);
      btn.setAttribute("aria-label", dark ? "Switch to light theme" : "Switch to dark theme");
    }
    btn.addEventListener("click", function () {
      var next = document.documentElement.dataset.theme === "dark" ? "light" : "dark";
      document.documentElement.dataset.theme = next;
      try { localStorage.setItem(KEY, next); } catch (e) { /* ignore */ }
      paint();
    });
    paint();
    var account = bar.querySelector('[aria-label="Account menu"]');
    bar.insertBefore(btn, account);
  });
})();
