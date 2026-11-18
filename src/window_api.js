// Injected by Porthole into the app's main frame before any page script
// runs. The launcher fills in the constants below: `controls` is true when
// the app draws its own title bar, `titleBar` is the mode in effect
// ("custom", or "overlay" on macOS), and `resizeEdges` is true where a
// frameless window needs the page to provide resize edges (Linux).
(() => {
  "use strict";
  if (window.__portholeInjected) return;
  Object.defineProperty(window, "__portholeInjected", { value: true });

  const controls = __PORTHOLE_CONTROLS__;
  const titleBar = __PORTHOLE_TITLE_BAR__;
  const resizeEdges = __PORTHOLE_RESIZE__;
  const post = (message) => window.ipc.postMessage(message);

  // Plain links to other sites open in the system browser instead of
  // taking the app's window away from the app.
  document.addEventListener("click", (event) => {
    if (event.defaultPrevented || event.button !== 0) return;
    if (event.metaKey || event.ctrlKey || event.shiftKey || event.altKey) return;
    const link = event.target instanceof Element && event.target.closest("a[href]");
    if (!link || link.hasAttribute("download")) return;
    const target = (link.getAttribute("target") || "").toLowerCase();
    if (target && target !== "_self" && target !== "_top") return;
    let url;
    try {
      url = new URL(link.href, location.href);
    } catch {
      return;
    }
    if (url.origin === location.origin) return;
    if (!["http:", "https:", "mailto:"].includes(url.protocol)) return;
    event.preventDefault();
    window.open(url.href, "_blank", "noopener");
  });

  if (!controls) return;

  // window.porthole.window: the fixed set of window controls a custom
  // title bar needs. Not a general bridge to the launcher.
  const listeners = new Map();
  let maximized = false;
  let waiting = [];

  const api = {
    titleBar,
    minimize: () => post("minimize"),
    toggleMaximize: () => post("toggle-maximize"),
    close: () => post("close"),
    isMaximized: () =>
      new Promise((resolve) => {
        waiting.push(resolve);
        post("is-maximized");
      }),
    on(name, callback) {
      if (!listeners.has(name)) listeners.set(name, new Set());
      listeners.get(name).add(callback);
      return () => api.off(name, callback);
    },
    off(name, callback) {
      listeners.get(name)?.delete(callback);
    },
  };
  const porthole = window.porthole || {};
  porthole.window = Object.freeze(api);
  window.porthole = porthole;

  // The launcher reports the maximised state through this.
  Object.defineProperty(window, "__portholeMaximized", {
    value: (value) => {
      const changed = value !== maximized;
      maximized = value;
      const resolvers = waiting;
      waiting = [];
      resolvers.forEach((resolve) => resolve(value));
      document.documentElement?.toggleAttribute("data-porthole-maximized", value);
      if (changed) {
        for (const callback of listeners.get("maximized-change") || []) {
          try {
            callback(value);
          } catch (err) {
            console.error(err);
          }
        }
      }
    },
  });

  // Elements inside a drag region that keep their own clicks.
  const INTERACTIVE =
    "button, a, input, select, textarea, label, summary, [contenteditable], [data-porthole-no-drag]";

  const EDGE = 5;
  const edgeAt = (x, y) => {
    if (!resizeEdges || maximized) return "";
    const w = window.innerWidth;
    const h = window.innerHeight;
    const ns = y < EDGE ? "n" : y >= h - EDGE ? "s" : "";
    const ew = x < EDGE ? "w" : x >= w - EDGE ? "e" : "";
    return ns + ew;
  };

  document.addEventListener(
    "mousedown",
    (event) => {
      if (event.button !== 0) return;
      const edge = edgeAt(event.clientX, event.clientY);
      if (edge) {
        event.preventDefault();
        post("resize-" + edge);
        return;
      }
      const el = event.target instanceof Element ? event.target : null;
      const region = el && el.closest("[data-porthole-drag]");
      if (!region) return;
      const own = el.closest(INTERACTIVE);
      if (own && region.contains(own)) return;
      event.preventDefault();
      // A double-click on a title bar maximises, as the OS bar does.
      post(event.detail === 2 ? "toggle-maximize" : "drag");
    },
    true,
  );

  if (resizeEdges) {
    const CURSORS = {
      n: "ns-resize", s: "ns-resize", e: "ew-resize", w: "ew-resize",
      ne: "nesw-resize", sw: "nesw-resize", nw: "nwse-resize", se: "nwse-resize",
    };
    let shown = "";
    document.addEventListener(
      "mousemove",
      (event) => {
        const edge = edgeAt(event.clientX, event.clientY);
        if (edge === shown) return;
        shown = edge;
        document.documentElement.style.cursor = edge ? CURSORS[edge] : "";
      },
      true,
    );
  }

  post("is-maximized");
})();
