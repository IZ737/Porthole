// Count launches in localStorage, and show which client-side route
// `spa_fallback` served index.html for.
const visits = Number(localStorage.getItem("visits") ?? 0) + 1;
localStorage.setItem("visits", String(visits));
document.getElementById("visits").textContent = visits;
document.getElementById("origin").textContent = location.origin;
document.getElementById("route").textContent =
  location.pathname === "/" ? "" : `Route: ${location.pathname}`;
