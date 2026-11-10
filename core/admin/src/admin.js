// Swarog Admin portal behaviour: data-attribute delegation only (CSP `default-src 'self'`).
(function () {
  "use strict";

  // Loaded in <head> without defer so the theme is set before first paint.
  var root = document.documentElement;
  function storedTheme() {
    try {
      return localStorage.getItem("swg.theme");
    } catch (e) {
      return null;
    }
  }
  root.dataset.theme = storedTheme() ||
    (window.matchMedia && matchMedia("(prefers-color-scheme: dark)").matches ? "dark" : "light");

  function toggleTheme() {
    root.dataset.theme = root.dataset.theme === "dark" ? "light" : "dark";
    try {
      localStorage.setItem("swg.theme", root.dataset.theme);
    } catch (e) { /* storage unavailable */ }
  }

  // ---------- popovers and menus (bundle.js placeAt / openMenu / popover) ----------
  function placeAt(el, anchor, align) {
    var r = anchor.getBoundingClientRect();
    el.style.left = "0px";
    el.style.top = "0px";
    var w = el.offsetWidth, h = el.offsetHeight;
    var left = align === "left" ? r.left : r.right - w;
    left = Math.max(8, Math.min(left, innerWidth - w - 8));
    var top = r.bottom + 4;
    if (top + h > innerHeight - 8) top = Math.max(8, r.top - h - 4);
    el.style.left = left + "px";
    el.style.top = top + "px";
  }

  function closePop() {
    document.querySelectorAll(".menu[data-menu-popup]").forEach(function (m) { m.remove(); });
    document.querySelectorAll(".popover[data-popover-panel]").forEach(function (p) {
      p.hidden = true;
      delete p.dataset.src;
    });
  }

  function togglePopover(btn) {
    var panel = document.querySelector('.popover[data-popover-panel="' + btn.dataset.popover + '"]');
    if (!panel) return;
    var wasOpen = !panel.hidden;
    closePop();
    if (wasOpen) return;
    panel.hidden = false;
    placeAt(panel, btn, "right");
  }

  function openMenu(btn) {
    closePop();
    var el = document.createElement("div");
    el.className = "menu";
    el.dataset.menuPopup = "";
    el.setAttribute("aria-busy", "true");
    document.body.appendChild(el);
    placeAt(el, btn, "right");
    fetch(btn.dataset.menuUrl, { headers: { "HX-Request": "true" }, credentials: "same-origin" })
      .then(function (res) {
        if (res.redirected) {
          location.href = res.url;
          return "";
        }
        return res.text();
      })
      .then(function (html) {
        if (!el.isConnected) return;
        el.innerHTML = html;
        el.removeAttribute("aria-busy");
        if (window.htmx) htmx.process(el);
        placeAt(el, btn, "right");
      });
  }

  // ---------- modals ----------
  var modalRoot = function () { return document.getElementById("modal-root"); };

  function revealPending() {
    var ack = document.querySelector("[data-reveal] [data-ack]");
    return ack && !ack.checked;
  }

  function closeModal() {
    if (revealPending()) return;
    var r = modalRoot();
    if (r) r.innerHTML = "";
  }

  function focusModal() {
    var r = modalRoot();
    var target = r && r.querySelector("input:not([disabled]):not([type=hidden]),button.primary,button.danger");
    if (target) target.focus();
  }

  // Confirm stays disabled until the typed token matches and a required reason is given.
  function gate(form) {
    var go = form.querySelector(".go");
    if (!go) return;
    var ok = true;
    var typed = form.querySelector("[data-confirm-type]");
    if (typed && typed.value !== typed.dataset.confirmType) ok = false;
    var reason = form.querySelector("[data-reason-required]");
    if (reason && reason.value.trim() === "") ok = false;
    go.disabled = !ok;
  }

  function flipToggle(t) {
    var on = !t.classList.contains("on");
    t.classList.toggle("on", on);
    t.setAttribute("aria-checked", on ? "true" : "false");
    var hidden = t.nextElementSibling;
    if (hidden && hidden.type === "hidden") hidden.value = on ? "true" : "false";
  }

  // ---------- panels ----------
  function retry(btn) {
    var slot = btn.closest(".slot");
    if (!slot || !slot.dataset.panelUrl || !window.htmx) return;
    btn.disabled = true;
    var label = btn.querySelector("span");
    if (label) label.textContent = "Retrying…";
    htmx.ajax("GET", slot.dataset.panelUrl, { target: slot, swap: "outerHTML" });
  }

  // ---------- toasts ----------
  function armToasts(scope) {
    (scope || document).querySelectorAll("[data-toast]:not([data-armed])").forEach(function (t) {
      t.dataset.armed = "";
      setTimeout(function () { t.remove(); }, 2600);
    });
  }

  // ---------- command palette (registry.js openSearch) ----------
  var palette = { timer: 0, scope: "", seq: 0 };

  function paletteEl() { return document.querySelector("[data-palette]"); }

  function openPalette() {
    closeModal();
    closePop();
    var p = paletteEl();
    if (!p) return;
    p.hidden = false;
    var input = p.querySelector("[data-palette-input]");
    input.focus();
    input.select();
  }

  function closePalette() {
    var p = paletteEl();
    if (p) p.hidden = true;
  }

  function runSearch() {
    var p = paletteEl();
    if (!p) return;
    var q = p.querySelector("[data-palette-input]").value.trim();
    var list = p.querySelector("[data-palette-list]");
    var seq = ++palette.seq;
    if (!q) {
      list.innerHTML = "";
      return;
    }
    var url = "/admin/_search?q=" + encodeURIComponent(q) + "&kind=" + encodeURIComponent(palette.scope);
    fetch(url, { headers: { "HX-Request": "true" }, credentials: "same-origin" })
      .then(function (res) {
        if (res.redirected) {
          location.href = res.url;
          return "";
        }
        return res.text();
      })
      .then(function (html) {
        if (seq === palette.seq) list.innerHTML = html;
      });
  }

  function scheduleSearch() {
    clearTimeout(palette.timer);
    palette.timer = setTimeout(runSearch, 150);
  }

  function setScope(chip) {
    var chips = chip.parentNode.querySelectorAll("[data-scope]");
    chips.forEach(function (c) {
      c.style.background = "";
      c.style.color = "";
    });
    chip.style.background = "var(--selected)";
    chip.style.color = "var(--accent-fg)";
    palette.scope = chip.dataset.scope;
    runSearch();
  }

  function moveHighlight(step) {
    var items = Array.prototype.slice.call(document.querySelectorAll("[data-palette-list] a.palette-item"));
    if (!items.length) return;
    var at = items.findIndex(function (i) { return i.classList.contains("hl"); });
    items.forEach(function (i) { i.classList.remove("hl"); });
    var next = items[(at + step + items.length) % items.length];
    next.classList.add("hl");
    next.scrollIntoView({ block: "nearest" });
  }

  // ---------- delegation ----------
  document.addEventListener("click", function (ev) {
    var t = ev.target;
    var el;
    if ((el = t.closest("[data-action=nav-toggle]"))) {
      document.body.classList.toggle("nav-open");
      return;
    }
    if (document.body.classList.contains("nav-open") && !t.closest(".sidebar")) {
      document.body.classList.remove("nav-open");
    }
    if ((el = t.closest("[data-action=palette]"))) {
      openPalette();
      return;
    }
    if ((el = t.closest("[data-action=theme]"))) {
      toggleTheme();
      return;
    }
    if ((el = t.closest("[data-popover]"))) {
      togglePopover(el);
      return;
    }
    if ((el = t.closest("[data-menu-url]"))) {
      openMenu(el);
      return;
    }
    if ((el = t.closest("[data-scope]"))) {
      setScope(el);
      return;
    }
    if ((el = t.closest("[data-palette-retry]"))) {
      runSearch();
      return;
    }
    if ((el = t.closest("[data-panel-retry]"))) {
      retry(el);
      return;
    }
    if ((el = t.closest("[data-copy]"))) {
      var secret = el.closest(".secret-value").querySelector("[data-secret]");
      if (navigator.clipboard && secret) {
        navigator.clipboard.writeText(secret.textContent).then(function () {
          el.querySelector("[data-copy-idle]").hidden = true;
          el.querySelector("[data-copy-done]").hidden = false;
        });
      }
      return;
    }
    if ((el = t.closest("[data-toggle]"))) {
      flipToggle(el);
      return;
    }
    if ((el = t.closest("[data-modal-close]"))) {
      if (el.hasAttribute("data-ack-done")) {
        var r = modalRoot();
        if (r) r.innerHTML = "";
      } else {
        closeModal();
      }
      return;
    }
    if (t.matches("[data-palette]")) {
      closePalette();
      return;
    }
    if (t.matches("[data-scrim]")) {
      closeModal();
      return;
    }
    if ((el = t.closest("[data-href]")) && !t.closest("a,button:not([data-href]),input,select,textarea,label")) {
      location.href = el.dataset.href;
      return;
    }
    // An item's htmx request is in flight; hide its menu now, remove it after the swap.
    if ((el = t.closest(".menu[data-menu-popup] .menu-item:not(.disabled)"))) {
      el.closest(".menu").hidden = true;
      return;
    }
    if (!t.closest(".menu,.popover")) closePop();
  });

  document.addEventListener("input", function (ev) {
    var t = ev.target;
    if (t.matches("[data-palette-input]")) {
      scheduleSearch();
      return;
    }
    var form = t.closest("form[data-gate]");
    if (form) gate(form);
  });

  document.addEventListener("change", function (ev) {
    var t = ev.target;
    if (t.matches("[data-ack]")) {
      var done = t.closest(".modal").querySelector("[data-ack-done]");
      if (done) done.disabled = !t.checked;
    }
  });

  document.addEventListener("keydown", function (ev) {
    if ((ev.ctrlKey || ev.metaKey) && ev.key.toLowerCase() === "k") {
      ev.preventDefault();
      openPalette();
      return;
    }
    var p = paletteEl();
    var paletteOpen = p && !p.hidden;
    if (ev.key === "Escape") {
      if (paletteOpen) closePalette();
      else closeModal();
      closePop();
      return;
    }
    if (paletteOpen) {
      if (ev.key === "ArrowDown" || ev.key === "ArrowUp") {
        ev.preventDefault();
        moveHighlight(ev.key === "ArrowDown" ? 1 : -1);
      } else if (ev.key === "Enter") {
        var hl = p.querySelector("a.palette-item.hl");
        if (hl) {
          ev.preventDefault();
          location.href = hl.getAttribute("href");
        }
      } else if (ev.key === "Tab") {
        ev.preventDefault();
        var chips = Array.prototype.slice.call(p.querySelectorAll("[data-scope]"));
        var at = chips.findIndex(function (c) { return c.dataset.scope === palette.scope; });
        setScope(chips[(at + (ev.shiftKey ? -1 : 1) + chips.length) % chips.length]);
      }
      return;
    }
    if ((ev.key === "Enter" || ev.key === " ") && ev.target.matches("[data-toggle]")) {
      ev.preventDefault();
      flipToggle(ev.target);
    }
  });

  // Every portal POST carries the page to return to, minus a spent `reveal` token.
  document.addEventListener("htmx:configRequest", function (ev) {
    if (ev.detail.verb !== "post" || ev.detail.parameters._return) return;
    var url = new URL(location.href);
    url.searchParams.delete("reveal");
    ev.detail.parameters._return = url.pathname + url.search;
  });

  document.addEventListener("htmx:afterSwap", function (ev) {
    armToasts(ev.detail.target.parentNode || document);
    if (ev.detail.target.id === "modal-root") {
      closePop();
      focusModal();
    }
  });

  document.addEventListener("DOMContentLoaded", function () {
    armToasts(document);
    focusModal();
  });
})();
