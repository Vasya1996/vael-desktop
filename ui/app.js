// Vael desktop frontend. Talks to the cloud server over WebSocket, captures the
// mic during push-to-talk, plays the streamed voice answer, and drives the
// 5-screen UI (library / login / launch / in-match coach / settings). The Rust
// side handles the GSI listener, the global PTT hotkey, process detection, the
// tray, the in-game overlay window, and installing the Dota config.

const TAURI = window.__TAURI__;
const invoke = TAURI.core.invoke;
const listen = TAURI.event.listen;
const appWindow = TAURI.window.getCurrentWindow();
const { droneSVG, aiFieldSVG } = window.VaelLogo;

const LS = {
  token: "vael_token",
  user: "vael_user",
  mic: "vael_mic_device",
  out: "vael_out_device",
  lang: "vael_lang",
};

const SITE_URL = "https://vaelcopilot.tractioneye.xyz";

const state = {
  token: localStorage.getItem(LS.token) || "",
  user: safeParse(localStorage.getItem(LS.user)),
  serverBase: "", // resolved from default_server_url() (cloud) — not user-editable
  googleEnabled: false,
  lang: localStorage.getItem(LS.lang) || "en", // interface language: en (default) | ru
  screen: "login", // login | library | launch | settings
  authBusy: false,
  authError: "",
  authMode: "login", // login | register — explicit tabs, no silent auto-register
  billing: null, // latest /billing/status payload (null = unknown)
  // connectivity / flags
  ws: null,
  connected: false,
  gsi: null, // { port, token, pttKey }
  settings: { pttKey: "F8", autostart: false },
  dotaRunning: false,
  dotaInstalled: true,
  micPermission: true,
  coachState: "idle", // idle | listen | think | answer
  // audio
  mic: null,
  micDeviceId: localStorage.getItem(LS.mic) || "",
  outDeviceId: localStorage.getItem(LS.out) || "",
  audioCtx: null,
  node: null,
  recording: false,
  reqId: null,
  serverTurnId: null, // requestId of an adopted server-initiated (composition) turn
  player: null,
  // settings ui
  pttCapturing: false,
};

function safeParse(s) { try { return s ? JSON.parse(s) : null; } catch { return null; } }
const $ = (id) => document.getElementById(id);
const el = (sel, root = document) => root.querySelector(sel);
const wsUrlFrom = (base) => base.replace(/^http/i, "ws").replace(/\/+$/, "") + "/ws";
const trimSlash = (s) => s.replace(/\/+$/, "");

// ============================================================
// i18n — interface language (English by default, switchable in Settings)
// ============================================================
const I18N = {
  en: {
    nav_library: "Library", nav_settings: "Settings",
    sec_home: "Home", sec_login: "Sign in", sec_settings: "Settings",
    win_min: "Minimize", win_max: "Maximize", win_close: "Close",
    hdr_login: "Sign in", player: "Player",
    login_title: "Sign in to Vael", login_sub: "Voice AI coach for Dota 2",
    login_google: "Continue with Google", login_or: "or",
    login_email: "Email", login_password: "Password",
    login_tab_in: "Sign in", login_tab_up: "Create account",
    login_submit_in: "Sign in", login_submit_up: "Create account",
    login_note: "One account for the app and the site",
    err_need_creds: "Enter email and password", err_bad_creds: "Wrong email or password",
    err_email_exists: "This email is already registered — use the Sign in tab",
    err_login_failed: "Could not sign in", err_google_off: "Google sign-in isn't configured on the server yet",
    err_google_failed: "Google sign-in failed", err_server_rejected: "Server rejected sign-in",
    set_usage: "Usage", usage_tokens: "Tokens this period",
    usage_reset: "Resets", usage_none: "No active subscription",
    usage_open_site: "Choose a plan", usage_upgrade: "Upgrade to PRO",
    note_limit: "Token limit reached — upgrade to PRO", note_nosub: "No active subscription",
    lib_label: "Library", lib_sub: "1 game available · hover the cover to launch with the coach",
    lib_steam_installed: "STEAM · INSTALLED", lib_steam_missing: "STEAM · NOT FOUND",
    lib_play: "▶ Play with AI companion", lib_pick: "Choose a plan", lib_hover_hint: "hover → button",
    lib_soon: "SOON", lib_other: "More games",
    coach_idle_name: "Idle", coach_idle_desc: "waiting for key", coach_idle_hint: "MIC OFF", coach_idle_chip: "READY",
    coach_listen_name: "Listening", coach_listen_desc: "key held", coach_listen_hint: "PTT HELD", coach_listen_chip: "LISTENING",
    coach_think_name: "Thinking", coach_think_desc: "processing", coach_think_hint: "~1.5–2.5 S", coach_think_chip: "THINKING",
    coach_answer_name: "Answering", coach_answer_desc: "voice playing", coach_answer_hint: "VOICE", coach_answer_chip: "ANSWERING",
    launch_title: "Launching Dota 2…", launch_ready: "VAEL READY",
    launch_foot: "Launching via Steam · the window will minimize to the tray",
    set_label: "Settings", set_ptt: "Push-to-talk key",
    set_ptt_warn: "Won't conflict with Dota's in-game voice",
    set_mic: "Microphone", set_out: "Voice output device",
    set_autostart: "Launch with Windows", set_lang: "Language",
    set_vision: "Read enemy lineup from screen (beta)",
    set_vision_note: "Off = unchanged. On = Vael reads the enemy heroes off your scoreboard; unsure reads are never reported.",
    set_foot: "Vael is active only while Dota is running. Otherwise it's idle, with no mic listening.",
    set_notif: "Notifications / errors",
    alert_mic_title: "No microphone", alert_mic_desc: "Access not granted or no device — check the mic permission.",
    alert_server_title: "No server connection", alert_server_desc: "Coach unavailable — check your internet connection.",
    alert_dota_title: "Dota not found", alert_dota_desc: "The game isn't installed or wasn't found via Steam.",
    alert_ok: "All good.", set_account: "Account", set_logout: "Sign out",
    note_unheard: "Didn't catch that — say it again", note_voice: "Voice failed — ask again", note_error: "Coach error — try again",
    dev_default: "Default", dev_unnamed: "Device",
    ptt_press: "Press a key…", ptt_busy: "Key in use", key_space: "Space",
  },
  ru: {
    nav_library: "Библиотека", nav_settings: "Настройки",
    sec_home: "Главная", sec_login: "Вход", sec_settings: "Настройки",
    win_min: "Свернуть", win_max: "Развернуть", win_close: "Закрыть",
    hdr_login: "Вход", player: "Игрок",
    login_title: "Вход в Vael", login_sub: "Голосовой AI-коуч для Dota 2",
    login_google: "Продолжить с Google", login_or: "или",
    login_email: "Email", login_password: "Пароль",
    login_tab_in: "Войти", login_tab_up: "Создать аккаунт",
    login_submit_in: "Войти", login_submit_up: "Создать аккаунт",
    login_note: "Один аккаунт для приложения и сайта",
    err_need_creds: "Введи email и пароль", err_bad_creds: "Неверный email или пароль",
    err_email_exists: "Такой email уже зарегистрирован — перейди на вкладку «Войти»",
    err_login_failed: "Не удалось войти", err_google_off: "Вход через Google пока не настроен на сервере",
    err_google_failed: "Вход через Google не удался", err_server_rejected: "Сервер отклонил вход",
    set_usage: "Расход токенов", usage_tokens: "Токены за период",
    usage_reset: "Сброс", usage_none: "Подписка не активна",
    usage_open_site: "Выбери тариф", usage_upgrade: "Перейти на PRO",
    note_limit: "Лимит токенов исчерпан — перейди на PRO", note_nosub: "Подписка не активна",
    lib_label: "Библиотека", lib_sub: "1 игра доступна · наведи на обложку, чтобы запустить с коучем",
    lib_steam_installed: "STEAM · УСТАНОВЛЕНА", lib_steam_missing: "STEAM · НЕ НАЙДЕНА",
    lib_play: "▶ Играть с AI companion", lib_pick: "Выбери тариф", lib_hover_hint: "наведение → кнопка",
    lib_soon: "СКОРО", lib_other: "Другие игры",
    coach_idle_name: "Покой", coach_idle_desc: "ждёт клавишу", coach_idle_hint: "МИКРОФОН ВЫКЛ", coach_idle_chip: "ГОТОВ",
    coach_listen_name: "Слушает", coach_listen_desc: "клавиша зажата", coach_listen_hint: "PTT ЗАЖАТА", coach_listen_chip: "СЛУШАЮ",
    coach_think_name: "Думает", coach_think_desc: "обрабатывает", coach_think_hint: "~1.5–2.5 С", coach_think_chip: "ДУМАЮ",
    coach_answer_name: "Отвечает", coach_answer_desc: "голос звучит", coach_answer_hint: "ГОЛОС", coach_answer_chip: "ОТВЕЧАЮ",
    launch_title: "Запускаем Dota 2…", launch_ready: "VAEL ГОТОВ К РАБОТЕ",
    launch_foot: "Запуск через Steam · окно свернётся в трей",
    set_label: "Настройки", set_ptt: "Клавиша push-to-talk",
    set_ptt_warn: "Не конфликтует с внутриигровым голосом Dota",
    set_mic: "Микрофон", set_out: "Устройство вывода голоса",
    set_autostart: "Запускать вместе с Windows", set_lang: "Язык",
    set_vision: "Распознавать состав врага с экрана (бета)",
    set_vision_note: "Выкл — как сейчас. Вкл — Vael читает героев врага с табло; при сомнении ничего не сообщает.",
    set_foot: "Vael активен только когда запущена Dota. Иначе — простой, без прослушивания микрофона.",
    set_notif: "Уведомления / ошибки",
    alert_mic_title: "Нет микрофона", alert_mic_desc: "Доступ не выдан или устройства нет — проверь разрешение микрофона.",
    alert_server_title: "Нет связи с сервером", alert_server_desc: "Коуч недоступен — проверь интернет-соединение.",
    alert_dota_title: "Dota не найдена", alert_dota_desc: "Игра не установлена или не найдена через Steam.",
    alert_ok: "Всё в порядке.", set_account: "Аккаунт", set_logout: "Выйти из аккаунта",
    note_unheard: "Не расслышал — повтори", note_voice: "Озвучка не сработала", note_error: "Ошибка коуча — повтори",
    dev_default: "По умолчанию", dev_unnamed: "Устройство",
    ptt_press: "Нажми клавишу…", ptt_busy: "Клавиша занята", key_space: "Пробел",
  },
};
function t(key) {
  const lang = (I18N[state.lang] ? state.lang : "en");
  return I18N[lang][key] ?? I18N.en[key] ?? key;
}
// Localized coach metadata for the current state.
function cmeta(s) {
  return { name: t(`coach_${s}_name`), desc: t(`coach_${s}_desc`), hint: t(`coach_${s}_hint`), chip: t(`coach_${s}_chip`) };
}
// Apply language to the static chrome (nav labels, window-button tooltips).
function applyChrome() {
  document.documentElement.lang = state.lang;
  const set = (id, txt) => { const e = $(id); if (e) e.textContent = txt; };
  set("navLibLabel", t("nav_library"));
  set("navSetLabel", t("nav_settings"));
  const title = (id, txt) => { const e = $(id); if (e) e.title = txt; };
  title("winMin", t("win_min")); title("winMax", t("win_max")); title("winClose", t("win_close"));
}

// ---------- boot ----------
// ============================================================
// Auto-update — on launch, check our signed feed; if a newer build exists,
// download it with a progress bar (like Dota's updater) and relaunch into it.
// Uses the global plugin API (withGlobalTauri); fails safe — any error or a
// missing updater never blocks the app.
// ============================================================
function updT(ru, en) { return (state && state.lang === "ru") ? ru : en; }

function ensureUpdateOverlay() {
  let ov = document.getElementById("vaelUpdate");
  if (ov) return ov;
  ov = document.createElement("div");
  ov.id = "vaelUpdate";
  ov.style.cssText = "position:fixed;inset:0;z-index:99999;display:none;align-items:center;justify-content:center;background:rgba(8,11,16,.88);font:14px/1.4 system-ui,Segoe UI,sans-serif;color:#eaf0f8";
  ov.innerHTML =
    '<div style="width:340px;max-width:80vw;text-align:center;padding:22px 24px;border:1px solid #243042;border-radius:14px;background:#0f141b">' +
    '<div style="font-weight:600;margin-bottom:6px">Vael</div>' +
    '<div id="vaelUpdText" style="color:#9fb0c3;margin-bottom:14px"></div>' +
    '<div style="height:8px;border-radius:6px;background:#1b2430;overflow:hidden">' +
    '<div id="vaelUpdFill" style="height:100%;width:0%;background:linear-gradient(90deg,#ff7a18,#ffae52);transition:width .15s"></div>' +
    '</div></div>';
  document.body.appendChild(ov);
  return ov;
}
function showUpdateBar(text) { const ov = ensureUpdateOverlay(); ov.style.display = "flex"; setUpdateText(text); }
function hideUpdateBar() { const ov = document.getElementById("vaelUpdate"); if (ov) ov.style.display = "none"; }
function setUpdateText(t) { const e = document.getElementById("vaelUpdText"); if (e && t != null) e.textContent = t; }
function setUpdateProgress(frac) { const e = document.getElementById("vaelUpdFill"); if (e) e.style.width = Math.round(Math.max(0, Math.min(1, frac)) * 100) + "%"; }

async function runUpdateCheck() {
  const U = window.__TAURI__;
  const updater = U && U.updater;
  const proc = U && U.process;
  if (!updater || !updater.check) return; // updater unavailable (e.g. dev) — skip silently
  let update;
  try { update = await updater.check(); } catch (e) { console.warn("update check failed", e); return; }
  if (!update) return;                    // already on the latest version
  try {
    showUpdateBar(updT(`Загружается обновление ${update.version}…`, `Downloading update ${update.version}…`));
    let total = 0, got = 0;
    await update.downloadAndInstall((ev) => {
      if (ev.event === "Started") { total = (ev.data && ev.data.contentLength) || 0; setUpdateProgress(0); }
      else if (ev.event === "Progress") { got += (ev.data && ev.data.chunkLength) || 0; setUpdateProgress(total ? got / total : 0); }
      else if (ev.event === "Finished") { setUpdateProgress(1); setUpdateText(updT("Установка обновления…", "Installing update…")); }
    });
    if (proc && proc.relaunch) await proc.relaunch(); // Windows: installer already closed the app; this brings the new version back
  } catch (e) {
    console.warn("update install failed", e);
    hideUpdateBar(); // never strand the user on an update error — fall through to the app
  }
}

window.addEventListener("DOMContentLoaded", init);

async function init() {
  // Auto-update first (non-blocking): if a newer signed build exists, show the
  // progress bar and relaunch into it; otherwise the app boots normally.
  runUpdateCheck();
  // logo + sidebar icons
  $("brandLogo").innerHTML = droneSVG({ size: 26, uid: "hdr" });
  $("navLibIc").innerHTML = gridIcon();
  $("navSetIc").innerHTML = gearIcon();
  applyChrome();

  wireChrome();

  if (!state.serverBase) {
    try { state.serverBase = await invoke("default_server_url"); } catch { state.serverBase = "https://ward.134.209.80.75.sslip.io"; }
  }

  try { state.gsi = await invoke("get_gsi_info"); if (state.gsi?.pttKey) state.settings.pttKey = state.gsi.pttKey; } catch {}
  try { const s = await invoke("get_settings"); if (s) state.settings = { ...state.settings, ...s }; } catch {}
  try { state.dotaInstalled = await invoke("dota_installed"); } catch {}
  fetchAuthConfig(); // best-effort, async

  await wireTauriEvents();

  if (state.token) {
    state.screen = "library";
    render();
    onAuthed();
  } else {
    state.screen = "login";
    render();
  }
}

async function fetchAuthConfig() {
  try {
    const res = await fetch(trimSlash(state.serverBase) + "/auth/config");
    const data = await res.json();
    state.googleEnabled = !!data.google;
  } catch { state.googleEnabled = false; }
  if (state.screen === "login") render();
}

function wireChrome() {
  $("winMin").onclick = () => appWindow.minimize();
  $("winMax").onclick = () => appWindow.toggleMaximize();
  $("winClose").onclick = () => appWindow.close(); // Rust hides to tray
  document.querySelectorAll(".nav-item").forEach((n) => {
    n.onclick = () => {
      if (!state.token) return;
      state.screen = n.dataset.nav;
      render();
    };
  });
}

async function wireTauriEvents() {
  await listen("gsi", (e) => {
    // Remember the latest in-match clock so vision reads can be stamped with it.
    const gt = e.payload && e.payload.map && e.payload.map.game_time;
    if (typeof gt === "number") state.lastGameTime = gt;
    if (state.ws && state.ws.readyState === WebSocket.OPEN) {
      state.ws.send(JSON.stringify({ type: "gsi", payload: e.payload }));
    }
    // Roster probe: a new match id re-arms the probe loop (fresh roster to read).
    const matchid = e.payload && e.payload.map && e.payload.map.matchid;
    if (matchid && matchid !== "0" && matchid !== state.rosterMatchId) {
      state.rosterMatchId = matchid;
      state.rosterComplete = false;
      stopRosterLoop();
    }
    // GSI sends partial deltas: a block is omitted when unchanged since the last
    // update, not when it no longer applies (shared/gsi.ts). So a payload with NO
    // `hero` key at all means "hero unchanged this tick" and must be a no-op here —
    // treating it as "no hero" would stop/restart the loop on every other tick and,
    // since startRosterLoop() fires an immediate probe, spam probes far faster than
    // the intended ~4s cadence. Only a `hero` block that IS present but carries no
    // valid hero name (menu/spectate) is a positive "no match" signal worth acting on.
    const hero = e.payload && e.payload.hero;
    if (hero) {
      const heroName = hero.name;
      if (typeof heroName === "string" && heroName.startsWith("npc_dota_hero_")) {
        if (state.settings.vision && state.ws && state.ws.readyState === WebSocket.OPEN) startRosterLoop();
      } else {
        stopRosterLoop(); // left to menu — stop polling but keep rosterComplete as-is
      }
    }
  });
  // CV enemy reads off the player's own screen → forwarded to the server as a VisionMsg,
  // stamped with the latest GSI game_time. The Rust core only emits this when the vision
  // flag is on (off by default), so for normal users this listener never fires.
  await listen("vision", (e) => {
    if (!(state.ws && state.ws.readyState === WebSocket.OPEN)) return;
    const p = e.payload || {};
    if (!Array.isArray(p.observations) || p.observations.length === 0) return;
    state.ws.send(JSON.stringify({
      type: "vision",
      source: p.source || "scoreboard",
      gameTimeStamp: state.lastGameTime || 0,
      observations: p.observations,
    }));
  });
  await listen("ptt-start", () => startRecording());
  await listen("ptt-end", () => stopRecording());
  await listen("dota-status", (e) => {
    state.dotaRunning = !!(e.payload && e.payload.running);
    // Privacy: only hold the mic open while Dota runs. Pre-warming on launch also
    // moves the permission prompt out of the middle of a match.
    if (state.dotaRunning) ensureMic();
    else { releaseMic(); if (state.coachState !== "idle") setCoachState("idle"); }
    updateCoachUI();
    if (state.screen === "library" || state.screen === "settings") render();
  });
}

// Roster probe: while the vision flag is on and a match is live, snapshot the
// top bar every 4s and send it for server-side hero recognition, until the
// server reports the roster complete (case "roster_status" in handleControl).
function startRosterLoop() {
  if (state.rosterTimer || state.rosterComplete) return;
  probeRoster();
  state.rosterTimer = setInterval(probeRoster, 4000);
}

function stopRosterLoop() {
  if (state.rosterTimer) { clearInterval(state.rosterTimer); state.rosterTimer = null; }
}

async function probeRoster() {
  if (!state.settings.vision) { stopRosterLoop(); return; } // toggled off mid-match — self-terminate, send nothing
  let dataBase64;
  try { dataBase64 = await invoke("vision_snapshot_topbar"); }
  catch (e) { console.warn("roster snapshot failed", e); return; }
  if (state.ws && state.ws.readyState === WebSocket.OPEN) {
    state.ws.send(JSON.stringify({
      type: "roster_probe",
      image: { mime: "image/jpeg", dataBase64 },
      gameTime: state.lastGameTime || 0,
    }));
  }
}

// ============================================================
// rendering
// ============================================================
function render() {
  const signedIn = !!state.token;
  const showSidebar = signedIn && (state.screen === "library" || state.screen === "settings");
  $("sidebar").classList.toggle("hidden", !showSidebar);

  // Login is a standalone screen: hide the whole app header (only the window
  // title bar + the sign-in card remain), per the app design.
  $("appheader").classList.toggle("hidden", state.screen === "login");

  // header section title
  const sections = { login: t("sec_login"), library: t("sec_home"), launch: t("sec_home"), settings: t("sec_settings") };
  $("hdrSection").textContent = sections[state.screen] || t("sec_home");

  // sidebar active state
  document.querySelectorAll(".nav-item").forEach((n) => n.classList.toggle("active", n.dataset.nav === state.screen));

  renderAuthArea();

  const content = $("content");
  content.className = "";
  if (state.screen === "login") { content.classList.add("centered"); content.innerHTML = loginScreen(); wireLogin(); }
  else if (state.screen === "library") { content.classList.add("padded"); content.innerHTML = libraryScreen(); wireLibrary(); }
  else if (state.screen === "launch") { content.classList.add("centered"); content.innerHTML = launchScreen(); }
  else if (state.screen === "settings") { content.classList.add("padded"); content.innerHTML = settingsScreen(); wireSettings(); }

  updateCoachUI();
}

function renderAuthArea() {
  const area = $("authArea");
  if (state.token && state.user) {
    const name = state.user.displayName || t("player");
    area.innerHTML = `<div class="profile"><span class="pname">${esc(name)}</span><div class="avatar">${esc((name[0] || "P").toUpperCase())}</div></div>`;
  } else {
    area.innerHTML = `<button class="btn-login" id="hdrLogin">${t("hdr_login")}</button>`;
    const b = $("hdrLogin");
    if (b) b.onclick = () => { state.screen = "login"; render(); };
  }
}

// ---------- screen: login ----------
function loginScreen() {
  const login = state.authMode === "login";
  return `
    <div class="login-bg-glow"></div>
    <div class="login-bg-grid"></div>
    <div class="login-card">
      <div class="login-head">
        <span class="l-drone">${droneSVG({ size: 46, uid: "login" })}</span>
        <div class="l-title">${t("login_title")}</div>
        <div class="l-sub">${t("login_sub")}</div>
      </div>
      <button class="btn-ghost" id="googleBtn"><span class="g-badge">G</span>${t("login_google")}</button>
      <div class="login-or"><div class="bar"></div><span>${t("login_or")}</span><div class="bar"></div></div>
      <div class="auth-tabs">
        <button class="auth-tab ${login ? "on" : ""}" id="tabLogin">${t("login_tab_in")}</button>
        <button class="auth-tab ${login ? "" : "on"}" id="tabRegister">${t("login_tab_up")}</button>
      </div>
      <div class="field"><label>${t("login_email")}</label><input id="email" type="email" placeholder="you@email.com" autocomplete="username" /></div>
      <div class="field"><label>${t("login_password")}</label><input id="password" type="password" placeholder="••••••••" autocomplete="current-password" /></div>
      <button class="btn-primary full" id="authBtn">${state.authBusy ? '<span class="spinner"></span>' : ""}${t(login ? "login_submit_in" : "login_submit_up")}</button>
      <div class="login-error" id="loginError">${esc(state.authError)}</div>
      <div class="login-note">${t("login_note")}</div>
    </div>`;
}

function wireLogin() {
  const btn = $("authBtn");
  if (btn) btn.onclick = emailAuth;
  const g = $("googleBtn");
  if (g) g.onclick = googleAuth;
  // Tabs switch the mode in place; typed fields survive (no full re-render).
  const setMode = (m) => {
    state.authMode = m;
    state.authError = "";
    $("tabLogin")?.classList.toggle("on", m === "login");
    $("tabRegister")?.classList.toggle("on", m === "register");
    const b = $("authBtn");
    if (b) b.innerHTML = t(m === "login" ? "login_submit_in" : "login_submit_up");
    const e = $("loginError");
    if (e) e.textContent = "";
  };
  const tl = $("tabLogin");
  if (tl) tl.onclick = () => setMode("login");
  const tr = $("tabRegister");
  if (tr) tr.onclick = () => setMode("register");
  ["email", "password"].forEach((f) => {
    const i = $(f);
    if (i) i.onkeydown = (e) => { if (e.key === "Enter") emailAuth(); };
  });
}

// ---------- screen: library ----------
function libraryScreen() {
  const coachPanel = state.dotaRunning ? coachPanelHTML() : "";
  return `
    <div class="mono-label">${t("lib_label")}</div>
    ${coachPanel}
    <div class="tiles">
      <div class="tile" id="dotaTile">
        <div class="tile-art">
          <div class="layer l-base"></div>
          <div class="layer l-glow"></div>
          <div class="layer l-streak"></div>
          <div class="tile-title"><div class="t-name">DOTA 2</div><div class="t-sub">${state.dotaInstalled ? t("lib_steam_installed") : t("lib_steam_missing")}</div></div>
          <div class="tile-overlay">${playButtonHTML()}</div>
        </div>
        <div class="tile-cap"><span class="c-name">Dota 2</span></div>
      </div>
      <div class="tile-soon">
        <div class="soon-box"><div class="soon-plus">+</div><div class="soon-tag">${t("lib_soon")}</div></div>
        <div class="soon-cap">${t("lib_other")}</div>
      </div>
    </div>`;
}

// True unless we positively know the subscription is inactive (unknown → allow;
// the server is the real gate). Drives the two-state launch button.
function isSubscribed() {
  return !(state.billing && state.billing.active === false);
}

// Two states: "Play with AI companion" (subscribed) vs "Choose a plan ↗" (no sub).
function playButtonHTML() {
  return isSubscribed()
    ? `<button class="btn-primary" id="playBtn">${t("lib_play")}</button>`
    : `<button class="btn-ghost play-pick" id="pickBtn">${t("lib_pick")}<span class="pick-arr">↗</span></button>`;
}

function wireLibrary() {
  const play = $("playBtn");
  if (play) play.onclick = (e) => { e.stopPropagation(); onPlay(); };
  const pick = $("pickBtn");
  if (pick) pick.onclick = (e) => { e.stopPropagation(); openPricing(); };
  const tile = $("dotaTile");
  if (tile) tile.onclick = () => { if (isSubscribed()) onPlay(); else openPricing(); };
}

function coachPanelHTML() {
  const m = cmeta(state.coachState);
  return `<div class="coach-panel" id="coachPanel">
      <div class="orb ${state.coachState}" id="coachOrb">${orbInner(state.coachState)}</div>
      <div class="c-text"><div class="c-name">${m.name}</div><div class="c-desc">${m.desc}</div><div class="c-hint">${m.hint}</div></div>
    </div>`;
}

// ---------- screen: launch ----------
function launchScreen() {
  return `
    <div class="launch">
      <div class="stage">${aiFieldSVG({ size: 240, uid: "launch" })}<div class="drone">${droneSVG({ size: 118, uid: "launchd" })}</div></div>
      <div class="l-title">${t("launch_title")}</div>
      <div class="l-ready">${t("launch_ready")}</div>
      <div class="l-bar"><div class="fill" id="launchFill"></div></div>
      <div class="l-foot">${t("launch_foot")}</div>
    </div>`;
}

// ---------- screen: settings ----------
function settingsScreen() {
  const s = state.settings;
  return `
    <div class="settings-wrap">
      <div class="settings-col">
        <div class="mono-label">${t("set_label")}</div>
        <div class="srow">
          <div class="srow-main"><span class="s-label">${t("set_ptt")}</span><div class="keycap" id="pttCap">${esc(prettyKey(s.pttKey))}</div></div>
          <div class="srow-note amber"><span>⚠</span>${t("set_ptt_warn")}</div>
        </div>
        <div class="srow"><div class="srow-main"><span class="s-label">${t("set_mic")}</span><div class="dropdown"><select id="micSel"></select><span class="val" id="micVal">${t("dev_default")}</span><span class="caret">▾</span></div></div></div>
        <div class="srow"><div class="srow-main"><span class="s-label">${t("set_out")}</span><div class="dropdown"><select id="outSel"></select><span class="val" id="outVal">${t("dev_default")}</span><span class="caret">▾</span></div></div></div>
        <div class="srow"><div class="srow-main"><span class="s-label">${t("set_lang")}</span><div class="dropdown"><select id="langSel"><option value="en">English</option><option value="ru">Русский</option></select><span class="val" id="langVal"></span><span class="caret">▾</span></div></div></div>
        <div class="srow"><div class="srow-main"><span class="s-label">${t("set_autostart")}</span><div class="toggle ${s.autostart ? "on" : ""}" id="autoTog"><div class="knob"></div></div></div></div>
        <div class="srow last"><div class="srow-main"><span class="s-label">${t("set_vision")}</span><div class="toggle ${s.vision ? "on" : ""}" id="visionTog"><div class="knob"></div></div></div><div class="srow-note"><span>👁</span>${t("set_vision_note")}</div></div>
        <div class="settings-foot">${t("set_foot")}</div>
      </div>
      <div class="settings-notif">
        <div class="mono-label">${t("set_notif")}</div>
        ${alertHTML("red", t("alert_mic_title"), t("alert_mic_desc"), state.micPermission)}
        ${alertHTML("amber", t("alert_server_title"), t("alert_server_desc"), state.connected)}
        ${alertHTML("mut", t("alert_dota_title"), t("alert_dota_desc"), state.dotaInstalled)}
        <div style="margin-top:14px"><div class="mono-label">${t("set_usage")}</div>
          <div class="usage-box" id="usageBox">${usageHTML()}</div>
        </div>
        <div style="margin-top:8px"><div class="mono-label">${t("set_account")}</div>
          <button class="btn-ghost" id="logoutBtn" style="margin-top:10px">${t("set_logout")}</button>
        </div>
      </div>
    </div>`;
}

// Usage block in Settings (matches the app design): plan badge + percent on top,
// big spent/limit number, token bar, reset date + "Upgrade to PRO" when critical.
// Bar colour by spend: green <70%, amber 70–89%, red ≥90%.
function usageHTML() {
  const b = state.billing;
  if (!b || !b.active) {
    return `<div class="u-none">${t("usage_none")}</div>
      <button class="btn-ghost u-btn" id="usageSiteBtn">${t("usage_open_site")}</button>`;
  }
  const u = b.usage || { tokens: 0, limit: 0, periodEnd: 0 };
  const planName = String(b.plan || "").toUpperCase();
  const pct = u.limit ? Math.min(100, Math.round((u.tokens / u.limit) * 100)) : 0;
  const tone = pct >= 90 ? "full" : pct >= 70 ? "warn" : "";
  const reset = u.periodEnd
    ? `${t("usage_reset")} ${new Date(u.periodEnd).toLocaleDateString(state.lang === "ru" ? "ru-RU" : "en-US", { day: "numeric", month: "long" })}`
    : "";
  // Upgrade only makes sense for LIGHT, and only once it actually matters (≥90%).
  const upgrade = b.plan === "light" && pct >= 90
    ? `<button class="u-pro" id="usageSiteBtn">${t("usage_upgrade")}</button>` : "";
  return `
    <div class="u-top"><span class="u-plan">${esc(planName)}</span><span class="u-pct ${tone}">${pct}%</span></div>
    <div class="u-nums"><span class="u-used">${fmtTokens(u.tokens)}</span><span class="u-lim">/ ${fmtTokens(u.limit)}</span></div>
    <div class="u-bar"><div class="u-fill ${tone}" style="width:${pct}%"></div></div>
    <div class="u-foot"><span class="u-reset">${reset}</span>${upgrade}</div>`;
}

function alertHTML(tone, title, desc, ok) {
  // When the condition is OK we dim it; only real problems stand out.
  const cls = ok ? "alert ok" : `alert ${tone}`;
  return `<div class="${cls}"><div class="a-dot"></div><div><div class="a-title">${title}</div><div class="a-desc">${ok ? t("alert_ok") : desc}</div></div></div>`;
}

function wireSettings() {
  // PTT key capture
  const cap = $("pttCap");
  cap.onclick = () => beginPttCapture(cap);

  // mic + output device dropdowns
  populateDevices();
  $("micSel").onchange = (e) => { state.micDeviceId = e.target.value; localStorage.setItem(LS.mic, state.micDeviceId); el(".val", e.target.parentElement).textContent = selLabel(e.target); if (state.mic) rebuildMic(); };
  $("outSel").onchange = (e) => { state.outDeviceId = e.target.value; localStorage.setItem(LS.out, state.outDeviceId); el(".val", e.target.parentElement).textContent = selLabel(e.target); };

  // autostart toggle
  $("autoTog").onclick = async () => {
    const next = !state.settings.autostart;
    try { await invoke("set_autostart", { enabled: next }); state.settings.autostart = next; $("autoTog").classList.toggle("on", next); }
    catch (e) { console.error("autostart", e); }
  };

  // enemy-vision toggle (off by default) — turns the screen-reading CV layer on/off
  const visionTog = $("visionTog");
  if (visionTog) visionTog.onclick = async () => {
    const next = !state.settings.vision;
    try { await invoke("vision_set_enabled", { enabled: next }); state.settings.vision = next; visionTog.classList.toggle("on", next); if (!next) stopRosterLoop(); }
    catch (e) { console.error("vision toggle", e); }
  };

  // interface language (RUS / ENG) — re-renders the whole UI on change
  const langSel = $("langSel");
  if (langSel) {
    langSel.value = state.lang;
    $("langVal").textContent = selLabel(langSel);
    langSel.onchange = (e) => {
      state.lang = e.target.value === "ru" ? "ru" : "en";
      localStorage.setItem(LS.lang, state.lang);
      applyChrome();
      render();
    };
  }

  $("logoutBtn").onclick = logout;

  // Usage: render from the cached status, then refresh from the server.
  const wireUsage = () => { const b = $("usageSiteBtn"); if (b) b.onclick = openPricing; };
  wireUsage();
  fetchBilling().then(() => {
    const box = $("usageBox");
    if (box && state.screen === "settings") { box.innerHTML = usageHTML(); wireUsage(); }
  });
}

const selLabel = (sel) => sel.options[sel.selectedIndex]?.textContent || t("dev_default");

async function populateDevices() {
  let devices = [];
  try { devices = await navigator.mediaDevices.enumerateDevices(); } catch {}
  // enumerateDevices() only fills in REAL labels (e.g. "AirPods (Vasiliy)") once the
  // page holds mic permission. If they're blank and the mic isn't already open, open
  // it briefly to unlock the real names, then release immediately. No prompt on
  // Windows (the Rust PermissionRequested handler grants it) and the mic is not held
  // open — so Settings still shows the user's actual device names, not "Устройство 1".
  const needLabels = devices.some((d) => (d.kind === "audioinput" || d.kind === "audiooutput") && !d.label);
  if (needLabels && !state.mic) {
    try {
      const tmp = await navigator.mediaDevices.getUserMedia({ audio: true });
      tmp.getTracks().forEach((t) => t.stop());
      devices = await navigator.mediaDevices.enumerateDevices();
    } catch (e) { console.warn("device labels", e); }
  }
  const mics = dedupeDevices(devices.filter((d) => d.kind === "audioinput"));
  const outs = dedupeDevices(devices.filter((d) => d.kind === "audiooutput"));
  fillSelect($("micSel"), mics, state.micDeviceId, $("micVal"));
  fillSelect($("outSel"), outs, state.outDeviceId, $("outVal"));
}

// Windows lists each device several times — the "default" and "communications"
// aliases carry the SAME label as the real endpoint, so the picker showed the
// headset name many times. Drop those aliases (we already offer "По умолчанию")
// and keep one entry per real device (by group + name).
function dedupeDevices(list) {
  const seen = new Set();
  return list.filter((d) => {
    if (!d.deviceId || d.deviceId === "default" || d.deviceId === "communications") return false;
    const key = (d.groupId || "") + "|" + (d.label || d.deviceId);
    if (seen.has(key)) return false;
    seen.add(key);
    return true;
  });
}

function fillSelect(sel, devices, current, valEl) {
  if (!sel) return;
  sel.innerHTML = `<option value="">${esc(t("dev_default"))}</option>` +
    devices.map((d, i) => `<option value="${esc(d.deviceId)}">${esc(d.label || t("dev_unnamed") + " " + (i + 1))}</option>`).join("");
  sel.value = devices.some((d) => d.deviceId === current) ? current : "";
  if (valEl) valEl.textContent = selLabel(sel);
}

// ---------- PTT key capture ----------
function beginPttCapture(cap) {
  if (state.pttCapturing) return;
  state.pttCapturing = true;
  cap.classList.add("capturing");
  cap.textContent = t("ptt_press");
  const onKey = async (e) => {
    // Wait for a real key — ignore lone modifier presses.
    if (/^(Shift|Control|Alt|Meta)(Left|Right)$/.test(e.code)) return;
    e.preventDefault();
    const accel = mapKeyToAccelerator(e);
    if (!accel) return;
    cleanup();
    try {
      await invoke("set_ptt_key", { key: accel });
      state.settings.pttKey = accel;
      if (state.gsi) state.gsi.pttKey = accel;
      cap.textContent = prettyKey(accel);
    } catch (err) {
      // The OS refused to grab this key (already held by another app, etc.).
      console.error("set_ptt_key", err);
      cap.textContent = t("ptt_busy");
      setTimeout(() => { cap.textContent = prettyKey(state.settings.pttKey); }, 1600);
    }
  };
  function cleanup() { state.pttCapturing = false; cap.classList.remove("capturing"); window.removeEventListener("keydown", onKey, true); }
  window.addEventListener("keydown", onKey, true);
  // safety: cancel after 6s
  setTimeout(() => { if (state.pttCapturing) { cleanup(); cap.textContent = prettyKey(state.settings.pttKey); } }, 6000);
}

// Any keyboard key can be the push-to-talk key (like Dota's own bind). We pass the
// W3C key code straight through ("KeyT", "Digit5", "Space", "Backquote", "F8", …) —
// the global-shortcut backend accepts those names — and prefix held modifiers so
// combos (Ctrl+Shift+X) work too. Lone modifiers are ignored. Mouse buttons aren't
// supported by the OS global-hotkey API; a key the OS can't grab -> "Клавиша занята".
function mapKeyToAccelerator(e) {
  const code = e.code;
  if (!code || /^(Shift|Control|Alt|Meta)(Left|Right)$/.test(code)) return null;
  const mods = [];
  if (e.ctrlKey) mods.push("Control");
  if (e.altKey) mods.push("Alt");
  if (e.shiftKey) mods.push("Shift");
  if (e.metaKey) mods.push("Super");
  return [...mods, code].join("+");
}

// Friendly keycap label for an accelerator code (e.g. "KeyT" -> "T", "Control+KeyX" -> "Ctrl+X").
function prettyKey(accel) {
  if (!accel) return "—";
  const one = (tok) => {
    if (tok === "Control") return "Ctrl";
    if (tok === "Super") return "Win";
    if (tok === "Space") return t("key_space");
    if (/^Key.$/.test(tok)) return tok.slice(3);
    if (/^Digit.$/.test(tok)) return tok.slice(5);
    if (/^Numpad/.test(tok)) return "Num " + tok.slice(6);
    const m = {
      Backquote: "`", Minus: "-", Equal: "=", BracketLeft: "[", BracketRight: "]",
      Backslash: "\\", Semicolon: ";", Quote: "'", Comma: ",", Period: ".", Slash: "/",
      ArrowUp: "↑", ArrowDown: "↓", ArrowLeft: "←", ArrowRight: "→",
      Escape: "Esc", Delete: "Del", Insert: "Ins", PageUp: "PgUp", PageDown: "PgDn",
      PrintScreen: "PrtSc", ScrollLock: "ScrLk", CapsLock: "Caps",
    };
    return m[tok] || tok;
  };
  return accel.split("+").map(one).join("+");
}

// ============================================================
// auth
// ============================================================
async function emailAuth() {
  if (state.authBusy) return;
  const email = ($("email")?.value || "").trim();
  const password = $("password")?.value || "";
  if (!email || !password) { setAuthError(t("err_need_creds")); return; }
  setAuthBusy(true);
  // Explicit modes — no silent auto-register: a typo in the email must show an
  // error, not quietly create a fresh (unpaid) account.
  try {
    const path = state.authMode === "register" ? "/auth/register" : "/auth/login";
    const data = await postAuth(path, { email, password });
    await finalizeLogin(data.token, data.user);
  } catch (e) {
    const m = e.message || "";
    const msg = /invalid email or password/i.test(m)
      ? t("err_bad_creds")
      : /already registered/i.test(m)
        ? t("err_email_exists")
        : m || t("err_login_failed");
    setAuthError(msg);
  }
}

async function postAuth(path, body) {
  const res = await fetch(trimSlash(state.serverBase) + path, {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify(body),
  });
  const data = await res.json().catch(() => ({}));
  if (!res.ok) throw new Error(data.error || t("err_login_failed"));
  return data;
}

async function googleAuth() {
  if (state.authBusy) return;
  if (!state.googleEnabled) { setAuthError(t("err_google_off")); return; }
  setAuthBusy(true);
  try {
    const token = await invoke("google_login", { serverBase: trimSlash(state.serverBase) });
    const me = await fetch(trimSlash(state.serverBase) + "/auth/me", { headers: { authorization: "Bearer " + token } });
    const data = await me.json();
    if (!me.ok) throw new Error(data.error || t("err_server_rejected"));
    await finalizeLogin(token, data.user);
  } catch (e) {
    setAuthBusy(false);
    setAuthError(typeof e === "string" ? e : e.message || t("err_google_failed"));
  }
}

async function finalizeLogin(token, user) {
  state.token = token;
  state.user = user;
  localStorage.setItem(LS.token, token);
  localStorage.setItem(LS.user, JSON.stringify(user));
  state.authBusy = false;
  state.authError = "";
  state.screen = "library";
  render();
  onAuthed();
}

// Patch the button/error in place (not a full re-render) so typed fields survive.
function submitLabel() {
  return t(state.authMode === "register" ? "login_submit_up" : "login_submit_in");
}
function setAuthBusy(b) {
  state.authBusy = b;
  const btn = $("authBtn");
  if (btn) { btn.disabled = b; btn.innerHTML = (b ? '<span class="spinner"></span>' : "") + submitLabel(); }
  const g = $("googleBtn");
  if (g) { g.style.pointerEvents = b ? "none" : ""; g.style.opacity = b ? "0.7" : ""; }
}
function setAuthError(msg) {
  state.authError = msg;
  state.authBusy = false;
  const e = $("loginError"); if (e) e.textContent = msg;
  const btn = $("authBtn"); if (btn) { btn.disabled = false; btn.innerHTML = submitLabel(); }
  const g = $("googleBtn"); if (g) { g.style.pointerEvents = ""; g.style.opacity = ""; }
}

function logout() {
  localStorage.removeItem(LS.token);
  localStorage.removeItem(LS.user);
  state.token = ""; state.user = null;
  state.billing = null;
  closeWs();
  state.screen = "login";
  render();
}

async function onAuthed() {
  try { await invoke("install_gsi_config"); } catch (e) { console.warn("gsi install", e); }
  // Refresh subscription, then re-render so the launch button shows the right
  // state ("Play" vs "Choose a plan") and Settings shows real usage.
  fetchBilling().then(() => {
    if (state.screen === "library" || state.screen === "settings") render();
  });
  connectWs();
}

// ---------- billing ----------
async function fetchBilling() {
  if (!state.token) return null;
  try {
    const res = await fetch(trimSlash(state.serverBase) + "/billing/status", {
      headers: { authorization: "Bearer " + state.token },
    });
    if (!res.ok) throw new Error(String(res.status));
    state.billing = await res.json();
  } catch (e) {
    // Unknown is not "blocked": the server enforces anyway; don't lock the UI on a blip.
    state.billing = null;
  }
  return state.billing;
}

// Open the site's pricing/tariffs section (where the user picks a plan / enters
// the access code). The browser handles the rest; the app never takes payment.
function openPricing() {
  invoke("open_url", { url: SITE_URL + "/#pricing" }).catch(() => {
    try { window.open(SITE_URL + "/#pricing"); } catch {}
  });
}

// ---------- play / launch ----------
async function onPlay() {
  if (!state.token) { state.screen = "login"; render(); return; }
  // No active subscription → send to the site to pick a plan; don't launch.
  // (An exhausted token limit still launches — the server stays silent and
  // Settings → Usage shows the red bar with the upgrade button.)
  const b = await fetchBilling();
  if (b && b.active === false) {
    if (state.screen === "library") render(); // reflect the "Choose a plan" button
    openPricing();
    return;
  }
  state.screen = "launch";
  render();
  // animate progress, kick Steam, then drop to tray.
  const fill = $("launchFill");
  requestAnimationFrame(() => { if (fill) fill.style.width = "62%"; });
  if (!state.connected) connectWs();
  try { await invoke("launch_dota"); } catch (e) { console.error("launch", e); }
  setTimeout(() => { if (fill) fill.style.width = "100%"; }, 1400);
  setTimeout(() => {
    state.screen = "library";
    render();
    appWindow.hide().catch(() => {}); // minimize to tray; tray "Открыть Vael" brings it back
  }, 2600);
}

// ============================================================
// coach state machine + indicator
// ============================================================
function setCoachState(s) {
  if (state.coachState === s) return;
  state.coachState = s;
  updateCoachUI();
  invoke("set_coach_state", { state: s }).catch(() => {});
}

// Short human text in the overlay instead of silent failures.
function showCoachNote(text) {
  invoke("overlay_note", { text }).catch(() => {});
}

function noteForServerError(message) {
  const m = String(message || "");
  if (/speech|understand/i.test(m)) return t("note_unheard");
  if (/voice|voiced/i.test(m)) return t("note_voice");
  return t("note_error");
}

function updateCoachUI() {
  const chip = $("coachChip");
  if (chip) {
    const active = state.dotaRunning || state.coachState !== "idle";
    chip.classList.toggle("hidden", !active);
    chip.classList.toggle("idle", state.coachState === "idle");
    chip.innerHTML = `<span class="chip-dot"></span>${cmeta(state.coachState).chip}`;
  }
  // live coach panel (library while a match is running)
  const panel = $("coachPanel");
  if (panel) {
    const orb = $("coachOrb");
    if (orb) { orb.className = "orb " + state.coachState; orb.innerHTML = orbInner(state.coachState); }
    const m = cmeta(state.coachState);
    const txt = el(".c-text", panel);
    if (txt) txt.innerHTML = `<div class="c-name">${m.name}</div><div class="c-desc">${m.desc}</div><div class="c-hint">${m.hint}</div>`;
  }
}

// inner decorations of the coach orb for a given state
function orbInner(s) {
  if (s === "idle") {
    return droneSVG({ size: 66, uid: "orb", accent: "#5A6473", glow: false }) +
      `<div class="micoff">${micOffIcon()}</div>`;
  }
  if (s === "listen") {
    return `<div class="ring" style="width:104px;height:104px;opacity:.16"></div><div class="ring" style="width:86px;height:86px;opacity:.36"></div>` +
      droneSVG({ size: 66, uid: "orb" });
  }
  if (s === "think") {
    return droneSVG({ size: 66, uid: "orb" }) + `<div class="dots"><span></span><span></span><span></span></div>`;
  }
  // answer
  const bars = [10, 21, 30, 16, 25, 12].map((h) => `<span style="height:${h}px"></span>`).join("");
  return droneSVG({ size: 66, uid: "orb" }) + `<div class="bars">${bars}</div>`;
}

// ============================================================
// websocket (carried over, unchanged transport)
// ============================================================
function connectWs() {
  if (!state.token) return;
  if (state.ws && (state.ws.readyState === WebSocket.OPEN || state.ws.readyState === WebSocket.CONNECTING)) return;
  const ws = new WebSocket(wsUrlFrom(state.serverBase));
  ws.binaryType = "arraybuffer";
  state.ws = ws;
  // Advertise vision capability honestly: only when the user actually turned the
  // screen-reading toggle ON (off by default). Claiming vision unconditionally made
  // the server enter the scoreboard flow and beg the player to open Tab for a board
  // this client never captures — an endless "open the scoreboard" loop. When the flag
  // is off the server falls back to asking the lineup by voice (no nagging).
  ws.onopen = () => ws.send(JSON.stringify({ type: "auth", token: state.token, caps: { vision: !!state.settings.vision } }));
  ws.onclose = () => handleDisconnect();
  ws.onerror = () => handleDisconnect();
  ws.onmessage = (ev) => {
    if (typeof ev.data === "string") handleControl(JSON.parse(ev.data));
    else onAudioChunk(ev.data);
  };
}

// A dropped/closed socket must never leave the indicator stuck on listen/think.
function handleDisconnect() {
  state.connected = false;
  state.recording = false;
  state.serverTurnId = null;
  if (state.coachState !== "idle") setCoachState("idle");
  refreshFlags();
  scheduleReconnect();
}

// Keep the coach socket alive: after any drop (network blip, cloud redeploy, or a
// failed initial connect) retry every 3s while the user stays logged in, so the
// "Нет связи с сервером" alert clears by itself instead of needing a re-login.
function scheduleReconnect() {
  if (!state.token || state.reconnectTimer) return;
  state.reconnectTimer = setTimeout(() => {
    state.reconnectTimer = null;
    if (state.token && (!state.ws || state.ws.readyState === WebSocket.CLOSED)) connectWs();
  }, 3000);
}

// True if this control frame belongs to a request we've already superseded.
function isStale(msg) {
  return !!(msg.requestId && state.reqId && msg.requestId !== state.reqId);
}

// Should this answer/tts frame drive the coach UI?
// - PTT turn: yes unless it's been superseded (stale requestId).
// - Server-initiated turn (composition follow-up): no matching PTT requestId, so
//   adopt it only when the client is idle; ignore it if a PTT turn is in flight.
//   Once adopted (tts_start), keep accepting that turn's later frames (tts_end) by
//   its server requestId, since the coach is no longer idle while it plays.
function adoptable(msg) {
  if (msg.serverInitiated === true) {
    if (msg.requestId && state.serverTurnId && msg.requestId === state.serverTurnId) return true;
    return state.coachState === "idle" && !state.recording;
  }
  return !isStale(msg);
}

function closeWs() {
  if (state.reconnectTimer) { clearTimeout(state.reconnectTimer); state.reconnectTimer = null; }
  if (state.ws) { try { state.ws.close(); } catch {} state.ws = null; }
  handleDisconnect();
}

function handleControl(msg) {
  switch (msg.type) {
    case "auth_ok":
      state.connected = true;
      // Server includes a subscription snapshot; refresh the full picture async,
      // then re-render so the launch button / usage reflect it.
      if (msg.subscription && !msg.subscription.active) {
        fetchBilling().then(() => {
          if (state.screen === "library" || state.screen === "settings") render();
        });
      }
      refreshFlags();
      break;
    case "auth_error": console.warn("auth_error", msg.message); break;
    case "transcript": break;
    case "answer_text": break;
    // Server asks the desktop to fast-poll the scoreboard for a few seconds so it
    // can read the lineup the instant the player opens Tab. Internal only (no note).
    case "vision_watch": invoke("vision_watch", { forSec: msg.forSec }).catch(() => {}); break;
    // Server has recognized the full roster (both teams) — stop probing the top bar.
    case "roster_status": if (msg.complete === true) { state.rosterComplete = true; stopRosterLoop(); } break;
    // Ignore completions for a superseded request (barge-in / stale answer).
    // A server-initiated turn (composition follow-up) carries no PTT requestId we
    // own, so it would look stale — adopt it as the current turn when idle, and
    // drop it if a PTT turn is in flight (the next PTT already has the composition).
    case "tts_start":
      if (adoptable(msg)) {
        state.serverTurnId = msg.serverInitiated === true ? msg.requestId : null;
        setCoachState("answer");
        startPlayback(msg.mime);
      }
      break;
    case "tts_end":
      if (adoptable(msg)) { state.serverTurnId = null; endPlayback(); setCoachState("idle"); }
      break;
    // Items the coach recommended: show their icons next to the overlay.
    case "items_panel": if (isStale(msg)) break; invoke("show_items", { items: msg.items, ttlSec: msg.ttlSec }).catch(() => {}); break;
    case "error":
      // A server-initiated (composition follow-up) turn that errors mid-stream
      // carries its srv- requestId and sends no tts_end, so it would look stale.
      // Recover the adopted turn explicitly (hard-stop playback, return to idle)
      // instead of hanging in "answer" until the next PTT. No coach note (the
      // composition flow is voice-only); the player can simply ask again.
      if (msg.requestId && state.serverTurnId && msg.requestId === state.serverTurnId) {
        state.serverTurnId = null;
        stopPlayback();
        setCoachState("idle");
        console.warn("server-initiated turn error", msg.message);
        break;
      }
      if (isStale(msg)) break;
      setCoachState("idle");
      // The server refused paid work mid-match: no popup — show a short coach
      // note, refresh billing, and re-render so Settings → Usage / the launch
      // button reflect it (limit → red bar + upgrade; expired → "Choose a plan").
      if (msg.code === "no_subscription" || msg.code === "limit_exhausted") {
        showCoachNote(msg.code === "limit_exhausted" ? t("note_limit") : t("note_nosub"));
        fetchBilling().then(() => {
          if (state.screen === "library" || state.screen === "settings") render();
        });
      } else {
        showCoachNote(noteForServerError(msg.message));
      }
      console.warn("server error", msg.message);
      break;
  }
}

function refreshFlags() {
  if (state.screen === "settings") render();
  else updateCoachUI();
}

// ============================================================
// microphone (push-to-talk uplink) — carried over + device select
// ============================================================
async function ensureMic() {
  if (state.mic) return;
  try {
    const audio = { channelCount: 1, echoCancellation: true, noiseSuppression: true };
    if (state.micDeviceId) audio.deviceId = { exact: state.micDeviceId };
    state.mic = await navigator.mediaDevices.getUserMedia({ audio });
    state.micPermission = true;
    state.audioCtx = new (window.AudioContext || window.webkitAudioContext)();
    const source = state.audioCtx.createMediaStreamSource(state.mic);
    const node = state.audioCtx.createScriptProcessor(4096, 1, 1);
    const mute = state.audioCtx.createGain();
    mute.gain.value = 0;
    node.onaudioprocess = (e) => {
      if (!state.recording || !state.ws || state.ws.readyState !== WebSocket.OPEN) return;
      const input = e.inputBuffer.getChannelData(0);
      const pcm = downsampleTo16k(input, state.audioCtx.sampleRate);
      state.ws.send(pcm.buffer);
    };
    source.connect(node);
    node.connect(mute);
    mute.connect(state.audioCtx.destination);
    state.node = node;
  } catch (e) {
    state.micPermission = false;
    refreshFlags();
    console.warn("mic", e.message);
  }
}

async function rebuildMic() {
  releaseMic();
  await ensureMic();
}

// Fully release the mic (stop the OS capture) — used when Dota closes / device change.
function releaseMic() {
  try { if (state.mic) state.mic.getTracks().forEach((t) => t.stop()); } catch {}
  try { if (state.audioCtx) state.audioCtx.close(); } catch {}
  state.mic = null; state.audioCtx = null; state.node = null;
  state.recording = false;
}

function startRecording() {
  // Guard on the live socket, not just our `connected` flag: the socket can be
  // CLOSING/CLOSED before onclose fires, and send() would throw.
  if (!state.ws || state.ws.readyState !== WebSocket.OPEN) return;
  ensureMic();
  if (state.audioCtx && state.audioCtx.state === "suspended") state.audioCtx.resume();
  stopPlayback();
  // A new PTT supersedes any in-flight server-initiated (composition) turn so a
  // late tts_end for it can't clobber this turn's state.
  state.serverTurnId = null;
  state.reqId = crypto.randomUUID();
  state.recording = true;
  setCoachState("listen");
  state.ws.send(JSON.stringify({ type: "ptt_start", requestId: state.reqId }));
}

function stopRecording() {
  if (!state.recording) return;
  state.recording = false;
  setCoachState("think");
  if (state.ws && state.ws.readyState === WebSocket.OPEN) {
    state.ws.send(JSON.stringify({ type: "ptt_end", requestId: state.reqId }));
  }
}

function downsampleTo16k(float32, inRate) {
  if (inRate === 16000) return floatToInt16(float32);
  const ratio = inRate / 16000;
  const outLen = Math.floor(float32.length / ratio);
  const out = new Int16Array(outLen);
  let oi = 0, ii = 0;
  while (oi < outLen) {
    const nextI = Math.floor((oi + 1) * ratio);
    let sum = 0, count = 0;
    for (let i = Math.floor(ii); i < nextI && i < float32.length; i++) { sum += float32[i]; count++; }
    const s = count ? sum / count : float32[Math.floor(ii)] || 0;
    out[oi] = Math.max(-1, Math.min(1, s)) * 0x7fff;
    oi++; ii = nextI;
  }
  return out;
}
function floatToInt16(float32) {
  const out = new Int16Array(float32.length);
  for (let i = 0; i < float32.length; i++) out[i] = Math.max(-1, Math.min(1, float32[i])) * 0x7fff;
  return out;
}

// ============================================================
// playback (streamed MP3 via MediaSource) — carried over + output device
// ============================================================
function startPlayback(mime) {
  stopPlayback();
  const ms = new MediaSource();
  const audio = new Audio();
  audio.src = URL.createObjectURL(ms);
  if (state.outDeviceId && typeof audio.setSinkId === "function") {
    audio.setSinkId(state.outDeviceId).catch(() => {});
  }
  const p = { ms, audio, sb: null, queue: [], ended: false, mime: mime || "audio/mpeg" };
  ms.addEventListener("sourceopen", () => {
    try {
      p.sb = ms.addSourceBuffer(p.mime);
      p.sb.addEventListener("updateend", () => pump(p));
      pump(p);
    } catch (e) { console.error("sourcebuffer", e); }
  });
  audio.play().catch(() => {});
  state.player = p;
}

function onAudioChunk(arrayBuffer) {
  const p = state.player;
  if (!p) return;
  p.queue.push(new Uint8Array(arrayBuffer));
  pump(p);
}

function pump(p) {
  if (!p.sb || p.sb.updating) return;
  if (p.queue.length) {
    try { p.sb.appendBuffer(p.queue.shift()); } catch (e) { /* retry on updateend */ }
  } else if (p.ended) {
    try { if (p.ms.readyState === "open") p.ms.endOfStream(); } catch {}
  }
}

function endPlayback() {
  const p = state.player;
  if (!p) return;
  p.ended = true;
  pump(p);
}

function stopPlayback() {
  const p = state.player;
  if (!p) return;
  try { p.audio.pause(); } catch {}
  try { if (p.ms.readyState === "open") p.ms.endOfStream(); } catch {}
  state.player = null;
}

function fmtTokens(n) {
  if (n >= 1e6) return Math.round(n / 1e5) / 10 + "M";
  if (n >= 1e3) return Math.round(n / 1e3) + "K";
  return String(n);
}

// ============================================================
// small inline icons + helpers
// ============================================================
function esc(s) { return String(s == null ? "" : s).replace(/[&<>"']/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" }[c])); }

function gridIcon(c = "#8A95A6") {
  return `<svg width="14" height="14" viewBox="0 0 24 24" fill="none"><rect x="4" y="4" width="6" height="6" rx="1.4" fill="${c}"/><rect x="14" y="4" width="6" height="6" rx="1.4" fill="${c}"/><rect x="4" y="14" width="6" height="6" rx="1.4" fill="${c}"/><rect x="14" y="14" width="6" height="6" rx="1.4" fill="${c}"/></svg>`;
}
function gearIcon(c = "#8A95A6") {
  let lines = "";
  [0, 45, 90, 135].forEach((a) => {
    const r = a * Math.PI / 180;
    lines += `<line x1="${12 + 8.5 * Math.cos(r)}" y1="${12 + 8.5 * Math.sin(r)}" x2="${12 - 8.5 * Math.cos(r)}" y2="${12 - 8.5 * Math.sin(r)}" stroke="${c}" stroke-width="1.6" stroke-linecap="round"/>`;
  });
  return `<svg width="15" height="15" viewBox="0 0 24 24" fill="none"><circle cx="12" cy="12" r="4.2" stroke="${c}" stroke-width="1.6"/>${lines}</svg>`;
}
function micOffIcon(c = "#5A6473") {
  return `<svg width="13" height="13" viewBox="0 0 24 24" fill="none"><rect x="9" y="3.5" width="6" height="11" rx="3" stroke="${c}" stroke-width="1.6"/><path d="M6 11a6 6 0 0 0 12 0M12 17v3" stroke="${c}" stroke-width="1.6" stroke-linecap="round"/><line x1="4" y1="4" x2="20" y2="20" stroke="${c}" stroke-width="1.7" stroke-linecap="round"/></svg>`;
}
