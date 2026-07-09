// Vael in-game overlay window. A tiny always-on-top capsule that hovers over
// Dota. It reflects the coach state pushed via the Tauri `coach-state` event,
// shows the item icons the coach just recommended (`items-panel`), flashes
// short notes (`coach-note`), and can be dragged anywhere — the Rust side
// persists the position.

const cap = document.getElementById("cap");
const panel = document.getElementById("panel");
const note = document.getElementById("note");
const STATES = ["idle", "listen", "think", "answer"];

function paint(stateName, accent) {
  document.getElementById("ovDrone").innerHTML = window.VaelLogo.droneSVG({
    size: 22,
    uid: "ov",
    accent,
    glow: stateName !== "idle",
  });
  // Keep modifier classes (note-on) while swapping the state class.
  STATES.forEach((s) => cap.classList.remove(s, "on"));
  cap.classList.add(stateName);
  if (stateName !== "idle") cap.classList.add("on");
}

paint("idle", "#5A6473");

window.__TAURI__.event.listen("coach-state", (e) => {
  const s = STATES.includes(e.payload) ? e.payload : "idle";
  paint(s, s === "idle" ? "#5A6473" : "#A6FF3C");
});

// ---- drag: only by grabbing the capsule itself (not the item panel) ----
cap.addEventListener("mousedown", (e) => {
  if (e.button !== 0) return;
  document.body.classList.add("dragging");
  window.__TAURI__.window.getCurrentWindow().startDragging().catch(() => {});
});
document.addEventListener("mouseup", () => document.body.classList.remove("dragging"));

// ---- report the capsule's hit rect (physical px) so the Rust side keeps the
// window click-through everywhere except over the capsule: item icons and empty
// space then pass clicks straight through to Dota. ----
const invoke = window.__TAURI__ && window.__TAURI__.core && window.__TAURI__.core.invoke;
function reportHitRect() {
  if (!invoke) return;
  const r = cap.getBoundingClientRect();
  const d = window.devicePixelRatio || 1;
  invoke("set_overlay_hit_rect", {
    x: Math.round(r.left * d),
    y: Math.round(r.top * d),
    w: Math.round(r.width * d),
    h: Math.round(r.height * d),
  }).catch(() => {});
}
reportHitRect();
setInterval(reportHitRect, 150);

// ---- item icons panel ----
const esc = (s) => String(s == null ? "" : s).replace(/[&<>"']/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" }[c]));
let panelTimer = null;

window.__TAURI__.event.listen("items-panel", (e) => {
  const { items, ttlSec, dir } = e.payload || {};
  // dir "up": panel above the capsule (default dock). dir "down": capsule stays
  // on top and the panel unfolds below it.
  document.body.classList.toggle("dock-top", dir === "down");
  panel.innerHTML = (items || [])
    .slice(0, 6)
    .map((it) => `<img src="${esc(it.img)}" title="${esc(it.name)}" alt="${esc(it.name)}" draggable="false">`)
    .join("");
  panel.classList.add("on");
  clearTimeout(panelTimer);
  panelTimer = setTimeout(() => {
    panel.classList.remove("on");
    panel.innerHTML = "";
    window.__TAURI__.core.invoke("hide_items").catch(() => {});
  }, Math.max(3, Number(ttlSec) || 30) * 1000);
});

// ---- transient notes (e.g. "voice failed") ----
let noteTimer = null;
window.__TAURI__.event.listen("coach-note", (e) => {
  note.textContent = String(e.payload || "");
  cap.classList.add("note-on");
  clearTimeout(noteTimer);
  noteTimer = setTimeout(() => cap.classList.remove("note-on"), 5000);
});
