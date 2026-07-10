![Windows](https://img.shields.io/badge/platform-Windows-blue)
[![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg)](https://opensource.org/licenses/MIT)

🇬🇧 English | 🇪🇸 [Español](README.es.md)

# Claude Counter

A lightweight Windows taskbar widget that shows how much of your Claude Code, Codex, and/or Google Antigravity usage window you have left, without opening a terminal or the provider's site.

## What You Get

- A **5h** bar for your current 5-hour Claude usage window
- A **7d** bar for your current 7-day window
- A live countdown next to each bar showing exactly when the limit resets (e.g. `1h 19m`, `2d`)
- A small animated mascot: it walks faster the more quota you have left, slows down as you approach the limit, and falls asleep once a window hits 100%
- A small native widget that lives directly in the Windows taskbar, automatically matching the taskbar's own light or dark tone
- System tray icon badge with a progress border showing your usage
- Optional Codex usage bars alongside Claude Code
- Optional Antigravity usage bars for Google's 5-hour and weekly quota windows
- Right-click options for refresh, displayed models, update frequency, language, startup, and widget visibility
- Multi-monitor taskbar placement, so the widget can live on the taskbar for the screen you prefer

## Who This Is For

This app is for Windows users who already have **Claude Code (CLI or App) installed and signed in**.

Codex support is optional. To show Codex usage, install and sign in to the Codex CLI, then enable Codex from the right-click **Models** menu.

Antigravity support is optional too. To show Antigravity usage, install and sign in to Google Antigravity, then enable the **Antigravity** model from the right-click **Models** menu.

It works best if you want a simple "how close am I to the limit?" display that is always visible.

## Requirements

- Windows 10 or Windows 11
- Claude Code (CLI or App) installed and authenticated
- Optional: Codex CLI installed and authenticated, if you want Codex usage
- Optional: Google Antigravity installed and authenticated, if you want Antigravity usage

If you use Claude Code through WSL, that is supported too. The app can read your Claude Code credentials from Windows or from your WSL environment.

## Install

1. Download the latest `ClaudeCounter-Setup.exe` from the [Releases](../../releases) page.
2. Run it. No administrator rights are required.
3. If this is the first time you're using Claude Code on this PC, the installer walks you through getting the CLI installed and signed in before you can finish.

Don't have the Claude Code CLI yet? The installer's setup step covers it, but you can also do it yourself at any time:

```powershell
irm https://claude.ai/install.ps1 | iex
claude
```

Then follow the sign-in prompts (use `/login` if it doesn't ask automatically).

### A note about the Windows warning

When you run the installer, Windows may show a "Windows protected your PC" screen. This isn't a virus warning — it just means the installer isn't digitally signed yet, so Windows can't vouch for the publisher.

To continue:

1. Click **More info**
2. Click **Run anyway**

## Use

Once running, the widget appears in your taskbar and as one or more tray icons in the notification area.

- Drag the mascot icon to move the taskbar widget
- On multi-monitor setups, drag the widget onto another Windows taskbar to move it to that screen
- Right-click the taskbar widget or tray icon for refresh, displayed models, update frequency, Start with Windows, reset position, language, and exit
- Left-click the tray icon to toggle the taskbar widget on or off
- Enable `Start with Windows` from the right-click menu if you want it to launch automatically when you sign in

### Models

Use the right-click **Models** menu to choose what the widget displays:

- **Claude Code** is enabled by default
- **Codex** can be enabled alongside Claude Code or shown by itself
- **Antigravity** can be enabled alongside the other providers or shown by itself as its own model column

When multiple models are shown, each model has its own usage bar and matching usage text color.

### System Tray Icon

The tray icon shows your current 5-hour usage as a percentage badge, with a white border that traces around the icon's perimeter proportionally to your usage.

If multiple providers are enabled, the app shows one tray icon per provider. If only one model is enabled, it shows one tray icon.

## Diagnostics

If you need to troubleshoot startup or visibility issues, run the installed executable with:

```powershell
claude-code-usage-monitor.exe --diagnose
```

This writes a log file to:

```text
%TEMP%\claude-code-usage-monitor.log
```

Settings are saved to:

```text
%APPDATA%\ClaudeCodeUsageMonitor\settings.json
```

## Privacy And Security

This project is **open source**, so you can inspect exactly what it does.

What the app reads:

- Your local Claude Code OAuth credentials from `~/.claude/.credentials.json`
- If needed, the same credentials file inside an installed WSL distro
- If Codex is enabled, your local Codex credentials from `$CODEX_HOME/auth.json` or `~/.codex/auth.json`
- If Antigravity is enabled, your local Antigravity OAuth token from Windows Credential Manager target `gemini:antigravity`

What the app sends over the network:

- Requests to Anthropic's Claude endpoints to read your usage and rate-limit information
- Requests to ChatGPT's Codex usage endpoint to read your Codex usage and rate-limit information, if Codex is enabled
- Requests to Google's Cloud Code / Antigravity endpoints to read your Antigravity quota information, if Antigravity is enabled
- If proxy environment variables such as `HTTPS_PROXY`, `HTTP_PROXY`, or `ALL_PROXY` are set, those outbound requests may use that proxy

What the app stores locally:

- Widget position
- Selected taskbar / screen
- Widget visibility
- Polling frequency
- Language preference
- Displayed model preferences

What it does **not** do:

- It does not send your credentials to any other server
- It does not use a separate backend service
- It does not collect analytics or telemetry
- It does not upload your project files
- It does not directly edit your Codex credentials file

Notes:

- If your Claude Code token is expired, the app may ask the local Claude CLI to refresh it in the background
- If your Codex token is expired, the app may ask the local Codex CLI to refresh it in the background. The app does not write `auth.json` itself; any credential update is handled by the Codex CLI.
- If your Antigravity token is expired, open Antigravity and sign in again. The app does not write Windows Credential Manager entries itself.
- Proxies should be trusted because proxied usage requests include your OAuth bearer token inside the TLS connection

## How It Works

The app:

1. Finds your enabled model login credentials
2. Reads your current usage from Anthropic, ChatGPT, and/or Google's Antigravity endpoints
3. Shows the result directly in the Windows taskbar
4. Keeps the widget aligned with the selected taskbar and tray area
5. Refreshes periodically in the background

If the newer usage endpoint is unavailable, it can fall back to reading the rate-limit headers returned by Claude's Messages API.

## Open Source

This project is licensed under MIT and based on the open-source [Claude-Code-Usage-Monitor](https://github.com/CodeZeno/Claude-Code-Usage-Monitor) project by Craig Constable.

If you want to inspect the behavior or audit the code, everything is in this repository.
