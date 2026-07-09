# Vael — desktop client

The open-source desktop client for **Vael**, an AI voice coach for Dota 2. This
repository contains the full source of the app that runs on **your** machine, so
anyone can read exactly what it does before installing it. The AI/coaching backend
is a separate hosted service and is not part of this repo.

- Website & download: https://vaelcopilot.tractioneye.xyz
- Built with [Tauri v2](https://v2.tauri.app/) (Rust core + a WebView UI). Windows only.

---

## Is it safe? What the app actually does

Everything below is verifiable in this source tree — the relevant files are named.

**How it reads the game**
- **Game State Integration (GSI).** Dota 2's own, Valve-sanctioned feature: the game
  POSTs match state to a **localhost-only** server the app runs on `127.0.0.1:53210`,
  protected by a per-install token. Nothing here leaves your machine except what the
  app forwards to the Vael backend. See `src-tauri/src/gsi.rs`.
- **Optional screen reading (OFF by default).** GSI never reveals the *enemy* draft, so —
  only if you turn it on — the app captures the **Dota game window** (via Windows
  Graphics Capture) to recognize enemy heroes from the top bar. Only the Dota window is
  captured, only while enabled. See `src-tauri/src/vision/`.

**What it sends**
- Your own hero/state and (if enabled) recognized enemy heroes, plus your push-to-talk
  voice, go over an encrypted connection to the Vael backend, which returns the coach's
  reply (text + speech). You can see exactly what is sent in `ui/app.js` and
  `src-tauri/src/`.

**What it does NOT do**
- It does **not** send input to Dota, automate play, or read the game's memory — it is
  read-only and never interferes with the match.
- It does **not** log your keystrokes. It listens for the **single push-to-talk hotkey**
  you configure, nothing else. See the PTT handling in `src-tauri/src/main.rs`.
- It does **not** capture your whole screen or other apps — only the Dota window, and
  only when screen-reading is switched on.
- The microphone is held open **only while Dota is running**.

**Updates are verifiable.** Releases are built by this repo's public CI and signed with
a minisign key. The app only accepts an update whose signature matches the public key
baked into `src-tauri/tauri.conf.json`. Each GitHub Release also publishes SHA-256
checksums, so the installer you download can be checked against the one CI produced.

---

## Кратко по-русски (безопасность)

Это открытый исходник **клиента** Vael — приложения, которое работает у вас на ПК. Сервер
(ИИ-тренер) — отдельный, в этот репозиторий не входит. Приложение читает состояние матча
через штатную функцию Dota (**GSI**, только localhost) и — **опционально, по умолчанию
выключено** — распознаёт состав врага, снимая **только окно Dota**. Оно **не** вводит
команды в игру, **не** логирует нажатия клавиш (слушает одну вашу клавишу рации), **не**
снимает весь экран и другие программы. Обновления подписаны ключом и проверяются
приложением; в каждом релизе есть контрольные суммы — можно сверить скачанный файл.

---

## Build from source

Prerequisites: [Rust](https://rustup.rs/) (stable) and [Node.js](https://nodejs.org/) 20+,
plus the Tauri prerequisites for Windows (WebView2 is present on Windows 10/11 by default).

```bash
npm ci
npm run build      # = tauri build -> installer in src-tauri/target/release/bundle/nsis
```

For a dev run: `npm run dev`.

A build from source runs the same code as the official release. Auto-update points at the
same feed, so a self-built copy will update to the next official signed release unless you
change the endpoint in `src-tauri/tauri.conf.json`.

## Releases

Tagging `desktop-v<version>` triggers `.github/workflows/release.yml`, which builds, signs,
and publishes the installer, its signature, `latest.json`, and `SHA256SUMS.txt` to GitHub
Releases. Nothing but public source and public CI is involved.

## License

[MIT](./LICENSE) © 2026 Vasiliy Andronov (Vael).

Security reports: see [SECURITY.md](./SECURITY.md).
