// logo.js — Vael spherical-drone mark + AI field, as plain inline SVG strings.
// Ported from the design handoff (logo-drone.jsx) to vanilla JS so the WebView
// needs no React. Each call takes a unique `uid` to keep gradient ids distinct.
//
// Wrapped in an IIFE so `droneSVG`/`aiFieldSVG` do NOT leak as global function
// declarations. They previously collided with app.js's
// `const { droneSVG, aiFieldSVG } = window.VaelLogo` — a top-level lexical vs.
// global-var clash that threw "Identifier 'droneSVG' has already been declared"
// and aborted app.js before any UI rendered. Consumers use window.VaelLogo.
(function () {
const VAEL_ACCENT = "#A6FF3C";

/**
 * Spherical drone logo. Returns an <svg> string.
 * @param {object} o
 * @param {number} [o.size=200]
 * @param {"visor"|"cyclops"} [o.variant="visor"]
 * @param {string} [o.accent="#A6FF3C"]
 * @param {boolean} [o.glow=true]
 * @param {string} [o.uid="d"]
 */
function droneSVG({ size = 200, variant = "visor", accent = VAEL_ACCENT, glow = true, uid = "d" } = {}) {
  const id = (s) => `${s}-${uid}`;
  const bodyId = id("body");
  const eyeId = id("eye");
  const eyeGlow = glow ? `filter:drop-shadow(0 0 ${size * 0.06}px ${accent})` : "";

  const eye = (cx, cy, r) => `
    <g style="${eyeGlow}">
      <circle cx="${cx}" cy="${cy}" r="${r + 4}" fill="#0A0D12" />
      <circle cx="${cx}" cy="${cy}" r="${r}" fill="url(#${eyeId})" />
      <circle cx="${cx}" cy="${cy}" r="${r}" fill="none" stroke="${accent}" stroke-width="1.2" opacity="0.9" />
      <circle cx="${cx - r * 0.32}" cy="${cy - r * 0.34}" r="${r * 0.26}" fill="#FFFFFF" opacity="0.85" />
    </g>`;

  let face = "";
  if (variant === "cyclops") {
    face = eye(100, 96, 17);
  } else {
    // visor
    face = `
      <rect x="34" y="74" width="132" height="36" rx="18" fill="#0B0E14" />
      <rect x="34" y="74" width="132" height="36" rx="18" fill="none" stroke="${accent}" stroke-width="1" opacity="0.35" />
      <circle cx="98" cy="92" r="4" fill="${accent}" opacity="0.3" />
      <g style="${eyeGlow}">
        <circle cx="132" cy="92" r="9" fill="url(#${eyeId})" />
        <circle cx="132" cy="92" r="9" fill="none" stroke="${accent}" stroke-width="1.1" />
        <circle cx="129" cy="89" r="2.4" fill="#FFFFFF" opacity="0.85" />
      </g>`;
  }

  const rim = glow
    ? `<path d="M150 150 A80 80 0 0 1 120 172" stroke="${accent}" stroke-width="2.5" fill="none" opacity="0.4" stroke-linecap="round" />`
    : "";

  return `<svg width="${size}" height="${size}" viewBox="0 0 200 200" fill="none" aria-label="Vael drone">
    <defs>
      <radialGradient id="${bodyId}" cx="36%" cy="30%" r="78%">
        <stop offset="0%" stop-color="#454D5E" />
        <stop offset="46%" stop-color="#262C38" />
        <stop offset="100%" stop-color="#12151C" />
      </radialGradient>
      <radialGradient id="${eyeId}" cx="42%" cy="38%" r="62%">
        <stop offset="0%" stop-color="#FFFFFF" />
        <stop offset="26%" stop-color="${accent}" />
        <stop offset="100%" stop-color="${accent}" />
      </radialGradient>
    </defs>
    <circle cx="100" cy="100" r="80" fill="url(#${bodyId})" />
    <circle cx="100" cy="100" r="80" stroke="rgba(255,255,255,0.06)" stroke-width="1" />
    <ellipse cx="74" cy="64" rx="30" ry="20" fill="rgba(255,255,255,0.16)" opacity="0.5" transform="rotate(-28 74 64)" />
    <path d="M28 112 Q100 132 172 112" stroke="rgba(0,0,0,0.32)" stroke-width="1.6" fill="none" />
    ${rim}
    ${face}
  </svg>`;
}

// Static particle field rendered BEHIND the drone (launch screen "AI matter").
const _AI_PARTS = [
  [60, 70, 1.6, 0.5], [240, 90, 2.2, 0.45], [210, 212, 1.4, 0.5], [80, 220, 2, 0.4],
  [150, 38, 1.8, 0.55], [270, 160, 1.3, 0.4], [40, 150, 1.5, 0.45], [258, 240, 1.6, 0.35],
  [120, 262, 1.2, 0.4], [200, 54, 1, 0.5], [55, 252, 1.4, 0.35], [246, 56, 1.2, 0.45],
  [34, 112, 1, 0.4], [176, 266, 1.5, 0.4], [286, 198, 1.2, 0.35], [100, 44, 1.3, 0.45],
];

function aiFieldSVG({ size = 380, accent = VAEL_ACCENT, uid = "f" } = {}) {
  const id = `aiNeb-${uid}`;
  const parts = _AI_PARTS.map((p) => `<circle cx="${p[0]}" cy="${p[1]}" r="${p[2]}" fill="${accent}" opacity="${p[3]}" />`).join("");
  return `<svg width="${size}" height="${size}" viewBox="0 0 300 300" fill="none"
      style="position:absolute;left:50%;top:50%;transform:translate(-50%,-50%);pointer-events:none">
    <defs>
      <radialGradient id="${id}" cx="50%" cy="50%" r="50%">
        <stop offset="0%" stop-color="${accent}" stop-opacity="0.18" />
        <stop offset="52%" stop-color="${accent}" stop-opacity="0.05" />
        <stop offset="100%" stop-color="${accent}" stop-opacity="0" />
      </radialGradient>
    </defs>
    <circle cx="150" cy="150" r="148" fill="url(#${id})" />
    <ellipse cx="150" cy="150" rx="142" ry="50" stroke="${accent}" stroke-opacity="0.22" stroke-width="1" transform="rotate(-20 150 150)" />
    <ellipse cx="150" cy="150" rx="122" ry="40" stroke="${accent}" stroke-opacity="0.14" stroke-width="1" transform="rotate(24 150 150)" />
    <circle cx="150" cy="150" r="134" stroke="${accent}" stroke-opacity="0.12" stroke-width="1" stroke-dasharray="1.5 11" />
    ${parts}
    <circle cx="232" cy="150" r="3" fill="${accent}" style="filter:drop-shadow(0 0 5px ${accent})" />
    <circle cx="76" cy="118" r="2.6" fill="${accent}" style="filter:drop-shadow(0 0 5px ${accent})" />
  </svg>`;
}

window.VaelLogo = { droneSVG, aiFieldSVG, ACCENT: VAEL_ACCENT };
})();
