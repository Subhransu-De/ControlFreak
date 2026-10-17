<p align="center">
  <img src="https://raw.githubusercontent.com/Subhransu-De/ControlFreak/main/assets/logo.png" alt="ControlFreak mascot: a blue octopus holding two app windows, pressing a key and clicking a mouse cursor" width="240">
</p>

<h1 align="center">ControlFreak</h1>

<p align="center">Let your agents work for you.</p>

<p align="center">
  <a href="https://github.com/Subhransu-De/ControlFreak/releases"><img src="https://img.shields.io/github/v/release/Subhransu-De/ControlFreak?include_prereleases&amp;label=Release&amp;style=flat" alt="Latest release"></a>
  <a href="LICENSE"><img src="https://img.shields.io/badge/License-MIT-blue?style=flat" alt="MIT License"></a>
  <a href="https://github.com/Subhransu-De/ControlFreak/actions/workflows/ci.yml"><img src="https://img.shields.io/github/actions/workflow/status/Subhransu-De/ControlFreak/ci.yml?branch=main&amp;label=Build&amp;style=flat" alt="Build status"></a>
</p>

ControlFreak lets an AI agent use your Windows desktop. It is a computer-use
[MCP](https://modelcontextprotocol.io) server written in Rust. Your MCP client, such as Claude
Code, Codex, Claude Desktop, OpenCode or Pi, starts it on your computer and talks to it over
STDIO. The agent can take screenshots, read text with the Windows OCR engine on your computer,
and click, scroll and type in one window that it chose.

[Install](#install) · [Documentation](https://github.com/Subhransu-De/ControlFreak/blob/main/docs/README.md) · [Privacy](PRIVACY.md) ·
[Changelog](CHANGELOG.md)

## What it can do

- Capture displays, regions, and windows.
- Move, click, drag, and scroll the mouse across multiple displays.
- Send keyboard shortcuts and type Unicode text without using the clipboard.
- List, focus, capture, and wait for application windows.
- Read, find, wait for, and click on-screen text using the local Windows OCR engine.
- Run up to 16 clicks, scrolls and keystrokes on one window in a single call.
- Inspect and switch between discoverable Windows virtual desktops.

See all 27 tools in the [tool reference](https://github.com/Subhransu-De/ControlFreak/blob/main/docs/tools.md).

## How it works

| See | Read | Act |
| --- | --- | --- |
| Screenshots of any display, region or window | Text on screen, read by Windows OCR on your computer | Clicks, drags, scrolls, keys and typing in the window it chose |
| Lists windows, displays and virtual desktops | Finds a label and clicks it by name | Up to 16 steps in one call |
| Waits for a window or a change on screen | Waits for text to appear or disappear | Works across multiple displays |

## Ask your agent to

| Task | What ControlFreak does |
| --- | --- |
| Fill in a form in a desktop app | Finds the window, clicks each field and types the values. |
| Click **Save** in any app | Reads the screen and clicks the label that matches. |
| Read the numbers off a dashboard | Captures the window and reads the text on your computer. |
| Wait for an export to finish | Watches the screen until "Done" appears. |
| Check the app it just built | Clicks through the app's window and takes a screenshot of each step. |
| Work across monitors and desktops | Finds windows on every display and switches virtual desktops. |

A blue glow around the screen shows when an agent is working.

## Install

1. Download `ControlFreak-<version>.exe` from
   [Releases](https://github.com/Subhransu-De/ControlFreak/releases).
2. Run it and tick the MCP clients you use.
3. Restart those clients.

Setup adds a `controlfreak` entry to each client you tick, and keeps a backup of the file:

| Client | File setup updates |
| --- | --- |
| Claude Code | `%USERPROFILE%\.claude.json` |
| Codex | `%USERPROFILE%\.codex\config.toml` |
| Claude Desktop | `%APPDATA%\Claude\claude_desktop_config.json` |
| OpenCode | `%USERPROFILE%\.config\opencode\opencode.json` |
| Pi | `%USERPROFILE%\.pi\agent\mcp.json` |

Needs Windows 10 (2004) or Windows 11 on x86-64. For the portable ZIP, silent install, manual
setup and uninstall, see [Install and configure](https://github.com/Subhransu-De/ControlFreak/blob/main/docs/install.md).

## Privacy

Screenshots, OCR text and window details go to your MCP client, which usually sends them to an
AI model. ControlFreak itself has no network code and keeps data only in memory. See
[PRIVACY.md](PRIVACY.md) for what it reads and keeps, and what setup changes.

## Documentation

Detailed reference for agents and client authors is in [`docs/`](https://github.com/Subhransu-De/ControlFreak/blob/main/docs/README.md):

- [Tools](https://github.com/Subhransu-De/ControlFreak/blob/main/docs/tools.md): every tool, targets, OCR matching, waits, results and errors.
- [Control sessions](https://github.com/Subhransu-De/ControlFreak/blob/main/docs/sessions.md): sessions, the indicator, stopping and elevated mode.
- [Install and configure](https://github.com/Subhransu-De/ControlFreak/blob/main/docs/install.md): everything about setup.

## License

ControlFreak is released under the [MIT License](LICENSE).
