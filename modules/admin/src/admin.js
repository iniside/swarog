// GameOps admin portal — the only client script. Vanilla, event-delegated, no eval,
// no frameworks (htmx handles fragment swaps declaratively via hx-* attributes). Loaded
// as a same-origin file so the strict CSP (`default-src 'self'`, no inline) permits it.
(function () {
  "use strict";

  // Close every open kebab dropdown except an optional one to keep open.
  function closeMenus(except) {
    document.querySelectorAll(".menu[data-menu-panel]").forEach(function (panel) {
      if (panel === except) return;
      panel.hidden = true;
      var btn = document.querySelector('.kebab[data-menu="' + panel.dataset.menuPanel + '"]');
      if (btn) btn.setAttribute("aria-expanded", "false");
    });
  }

  // Clear any open modal fragment out of #modal-root.
  function closeModal() {
    var root = document.getElementById("modal-root");
    if (root) root.innerHTML = "";
  }

  // Kebab toggle + close affordances, all via one delegated click listener.
  document.addEventListener("click", function (ev) {
    var kebab = ev.target.closest(".kebab[data-menu]");
    if (kebab) {
      ev.stopPropagation();
      var id = kebab.getAttribute("data-menu");
      var panel = document.querySelector('.menu[data-menu-panel="' + id + '"]');
      if (!panel) return;
      var willOpen = panel.hidden;
      closeMenus(willOpen ? panel : null);
      panel.hidden = !willOpen;
      kebab.setAttribute("aria-expanded", willOpen ? "true" : "false");
      return;
    }
    // Clicking a menu item (navigate or htmx modal) dismisses the dropdown.
    if (ev.target.closest(".menu-item")) {
      closeMenus(null);
    }
    // Close the modal on the backdrop or an explicit close control.
    if (ev.target.closest("[data-modal-close]") || ev.target.hasAttribute("data-modal-overlay")) {
      closeModal();
    }
    // A click anywhere outside an open dropdown closes it.
    if (!ev.target.closest(".menu-wrap")) {
      closeMenus(null);
    }
  });

  // Escape closes the modal first, else any open dropdown.
  document.addEventListener("keydown", function (ev) {
    if (ev.key !== "Escape") return;
    var root = document.getElementById("modal-root");
    if (root && root.innerHTML.trim() !== "") {
      closeModal();
    } else {
      closeMenus(null);
    }
  });
})();
