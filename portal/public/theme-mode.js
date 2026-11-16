// Runs before the first paint. The Console saves "system", "light" or "dark"
// under this key; when it shares this origin the Portal follows that choice.
// Without one, or without storage, portal.css follows the device.
(() => {
  let mode = null;
  try {
    mode = localStorage.getItem("trellis.theme");
  } catch (_) {}
  if (mode === "light" || mode === "dark")
    document.documentElement.setAttribute("data-theme", mode);
})();
