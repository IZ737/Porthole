// Starter title bar behaviour: wires the buttons in `.titlebar` to
// Porthole's window controls. Drag and double-click-to-maximise need no
// code: Porthole handles any element with `data-porthole-drag`.
const win = window.porthole?.window;

if (win) {
  const actions = {
    minimize: () => win.minimize(),
    maximize: () => win.toggleMaximize(),
    close: () => win.close(),
  };
  for (const button of document.querySelectorAll(".titlebar [data-window]")) {
    button.addEventListener("click", actions[button.dataset.window]);
  }
  // `title_bar = "overlay"` on macOS: the OS keeps its own buttons.
  if (win.titleBar === "overlay") {
    document.documentElement.classList.add("titlebar-overlay");
  }
} else {
  // Opened in a normal browser (or with the native title bar): no custom bar.
  document.querySelector(".titlebar")?.remove();
  document.body.style.paddingTop = "0";
}
