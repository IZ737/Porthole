// Show the window state, kept up to date by `maximized-change`.
const state = document.getElementById("state");
const show = (maximized) => {
  state.textContent = maximized ? "maximised" : "not maximised";
};
const win = window.porthole?.window;
if (win) {
  win.isMaximized().then(show);
  win.on("maximized-change", show);
} else {
  state.textContent = "not in Porthole";
}
