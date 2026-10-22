// Mockup-only preferences on top of the design-system bundle: theme, annotation layer and forced panel state.
(() => {
  const s = UI.state;
  try {
    s.theme = localStorage.getItem("swg.theme") || (matchMedia("(prefers-color-scheme: dark)").matches ? "dark" : "light");
    s.annot = localStorage.getItem("swg.annot") === "1";
  } catch (e) { /* storage unavailable */ }
  const params = new URLSearchParams(location.search);
  if (params.get("theme")) s.theme = params.get("theme");
  if (params.get("annot")) s.annot = params.get("annot") === "1";
  if (params.get("state")) s.force = params.get("state");
  document.documentElement.dataset.theme = s.theme;
})();
