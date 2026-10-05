# BDOMon

A compact, Black Desert Online–styled performance HUD (FPS / GPU / CPU / Ping) drawn **in-game through RivaTuner Statistics Server (RTSS)**.

BDOMon does not inject anything into the game. It talks to RTSS through its official shared-memory interface, and RTSS — which is already hooking the game — renders the HUD. That keeps it compatible with anti-cheat systems, since the only code running inside the game process is RTSS's own.

[![Release Build](https://github.com/kadirgun/bdomon/actions/workflows/release.yml/badge.svg)](https://github.com/kadirgun/bdomon/actions/workflows/release.yml)

## Features

- **Pixel-perfect HUD panel** (141×52, icons included) rendered from an embedded PNG — no external files to carry around.
- **Live stats**, refreshed once per second:
  - **FPS** — read straight from RTSS's application entry for `BlackDesert64.exe`.
  - **GPU usage** — Windows PDH `GPU Engine` utilization counters.
  - **CPU usage** — `GetSystemTimes` deltas.
  - **Ping** — real RTT of the game's TCP connection (remote port **8889**) via `GetPerTcpConnectionEStats`. When you switch servers the old connection dies and a new one appears; BDOMon detects the new connection tuple and re-enables RTT collection automatically.
- **Positioning**: drag the HUD inside the 16:9 preview, or type X/Y coordinates. The position is applied to the OSD instantly and persisted between runs.
- **Single portable executable** — the HUD artwork and the _Black Desert_ font are embedded in the binary and extracted/installed on every launch, so you always run the assets matching the code.
- **System tray friendly** — closing the window minimizes to the tray while the overlay keeps running. Quitting from the tray clears the OSD.
- **Live profile reload** — applies RTSS profile changes without restarting the game.

## Requirements

- Windows 10 or 11 (x64)
- [RivaTuner Statistics Server](https://www.guru3d.com/download/rtss-rivatuner-statistics-server-download/) **7.3.4 or newer** (uses shared memory v2.20+)
- Black Desert Online

No administrator rights are needed; everything is written under your user profile.

## Installation

1. Download `bdomon.exe` from the [latest release](https://github.com/kadirgun/bdomon/releases/latest). Releases ship a single portable executable — no installer.
2. Start **RTSS**, then start the game (RTSS hooks the game when it launches), then run `bdomon.exe`.
   On first launch it extracts `hud.png` to `%APPDATA%\bdomon\resources` and installs the _Black Desert_ font for your user (`%LOCALAPPDATA%\Microsoft\Windows\Fonts` + registry). This is repeated on every launch, overwriting whatever is there.
3. Configure the RTSS profile (one-time, see below).
4. Press **Start** in the BDOMon window.

## RTSS profile setup

The OSD is drawn using RTSS's profile for `BlackDesert64.exe`, so a few profile settings must match what BDOMon expects. In RTSS, open **Profiles**, select the game (or _Global_), and set:

| Setting                                    | Value                                           |
| ------------------------------------------ | ----------------------------------------------- |
| On-screen display rendering implementation | **Raster 3D**                                   |
| Zoom                                       | **100 %**                                       |
| Text position X / Y                        | **1 / 1** (absolute)                            |
| OSD font                                   | **Black Desert** (installed by BDOMon), size 12 |

Notes:

- BDOMon positions everything relative to the profile origin (1, 1); a different zoom or position will misplace the HUD.
- After changing profile settings, click **Reload RTSS profile** in BDOMon — RTSS applies the profile to the running game without a restart.

## Usage

- **Start / Stop** — enables or disables the OSD (the OSD slots are cleared on stop/exit).
- **Preview** — the HUD element inside the 16:9 preview is draggable; dropping it applies the new position in-game immediately. X/Y fields accept exact coordinates.
- **Reload RTSS profile** — asks RTSS to re-read the game's profile file.
- **Tray** — closing the window hides it to the tray (overlay keeps running). Left-click the tray icon to show the window; the menu offers _Show_ and _Quit_.
- Position is saved to `%APPDATA%\bdomon\position.json`.

## Troubleshooting

| Symptom                         | Likely cause / fix                                                                                                                                             |
| ------------------------------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| "RTSS is not running"           | Start RivaTuner Statistics Server first.                                                                                                                       |
| No HUD in game                  | The game must be hooked by RTSS — start RTSS _before_ the game (or restart the game with RTSS running). Check that OSD is enabled in the game's profile.       |
| HUD appears but is misplaced    | Profile zoom must be 100 %, text position 1/1, implementation Raster 3D (see setup above).                                                                     |
| Ping stays 0                    | BDOMon only reports while `BlackDesert64.exe` has an established connection to remote port 8889 (the game world server). Log into a character and check again. |
| GPU stays 0                     | Some driver/counter combinations report no `GPU Engine` instances. Update your GPU drivers.                                                                    |
| Text is tiny/huge or wrong font | Set the profile font to _Black Desert_, size 12, then click **Reload RTSS profile**.                                                                           |

## How it works

- BDOMon connects to the `RTSSSharedMemoryV2` file mapping, verifies version ≥ v2.20 and claims **five OSD slots**: one for the background image, one per value. Values go in separate slots because RTSS's hypertext parser treats `%…%` pairs as data-source macros — a literal `%` in a shared string would corrupt the whole OSD.
- The image slot uses RTSS hypertext `<LI>` / `<I>` tags pointing at a content-hashed copy of `hud.png`, so RTSS refreshes its texture cache whenever the artwork changes.
- Each second BDOMon writes fresh hypertext into the slots and bumps the OSD frame counter; RTSS redraws on the next frame.
- Ping: the game's TCP row is found via `GetExtendedTcpTable` (PID + remote port 8889), RTT collection is enabled per connection with `SetPerTcpConnectionEStats`, and the smoothed RTT (`SumRtt`) is read each tick.

## Building from source

Prerequisites: Node.js 22 + pnpm 9, Rust stable (MSVC toolchain), and the [Tauri 2 prerequisites](https://tauri.app/start/prerequisites/) (WebView2 on Windows).

```sh
pnpm install
pnpm tauri dev            # development
pnpm tauri build --no-bundle   # portable exe only
```

The built binary lands at `src-tauri/target/release/bdomon.exe`.

CI: pushing a `v*` tag runs [`.github/workflows/release.yml`](.github/workflows/release.yml), builds with `--no-bundle` and attaches the single `bdomon.exe` to a GitHub Release.

## Project layout

```
src/                     React UI (control panel, preview, drag positioning)
src-tauri/
  src/rtss.rs            RTSS shared-memory client (slots, hypertext, profile reload)
  src/hud.rs             Hypertext builders + live data source
  src/sensors.rs         CPU / GPU / ping monitors
  src/resources.rs       Embedded hud.png + font, per-user extraction/install
  src/lib.rs             Tauri app: commands, 1 s ticker, tray, persistence
  resources/             hud.png, black_desert.ttf (embedded via include_bytes!)
```

## Disclaimer

BDOMon is an unofficial fan tool and is not affiliated with or endorsed by Pearl Abyss. "Black Desert" and related marks belong to their respective owners.
