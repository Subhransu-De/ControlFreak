# Privacy

This page describes what ControlFreak does with information on your computer. It covers the
`controlfreak` MCP server and the optional Windows setup program. It is based on the source code
in this repository.

## Summary

- ControlFreak runs on your computer. It has no account, no telemetry, no analytics, no update
  check and no network code.
- It sends screenshots, text read by OCR, the names of controls and window information to the MCP
  client that started it, such as Claude Code or Codex. What that client does next, including sending it to an AI
  model provider, is decided by the client and its provider, not by ControlFreak.
- It keeps short-lived data in memory only, and writes no files unless you turn on its error log.
- The optional setup program can register ControlFreak with Claude Code or Codex by running their official CLI commands (`claude mcp add` / `codex mcp add`). It never reads, edits or backs up third-party configuration files directly.

## What the server reads and why

The server reads desktop information only when the connected client calls a tool:

- **Screenshots** of a display, a region or a window, for the capture, wait and action tools.
  Actions return a screenshot by default so the client can see the result.
- **Text on the screen**, read by the Windows OCR engine on your computer, for
  `read_text_in_region`, `find_text_on_screen`, `wait_for_text` and `click_text`.
- **Controls of one window**, read through Windows UI Automation for `find_elements`: each
  control's role, accessible name, enabled and focus state, supported patterns and position.
  Only the window the client names is read, and only controls of that window's own process. The
  name of a password field is withheld, and no control's value or text content is read. The
  names are what the application exposes to screen readers, so they can include document
  titles, list entries or other visible text.
- **Window information**: titles, class names, process IDs, positions and integrity levels of
  top-level windows, for `list_windows`, `wait_for_window` and the safety checks before each input
  action. Each action result also names the window in front afterwards. When that window has no
  target reference, for example an untitled popup menu, the result gives only its class name and
  process ID, never its title.
- **Display and virtual desktop layout**, to map coordinates and to check the target window.
- **Action arguments** from the client, such as the text to type or the keys to press. These are
  used to send the input and are not stored.

The server also installs a keyboard hook that looks only for Esc, so that you can stop the agent
during a control session. It does not record or store any keys.

## What leaves the server

Tool results go to the connected MCP client over STDIO, the standard input and output of the
process. ControlFreak does not open network connections.

The client receives screenshots, OCR text and window details, and usually passes them to an AI
model. If that model runs on a remote service, the data leaves your computer under the client's
and the provider's terms. Check those terms before you let an agent see sensitive content. Close
or hide windows you do not want the agent to see. ControlFreak cannot hide content inside an
approved window.

## What the server keeps

All of this is in memory and is gone when the server exits:

- Up to 16 visual baselines, for the visual-change waits. Each holds screen pixels and is removed from memory 5 minutes after it was captured, even if the server is idle. A `wait_for_change_since` call that started before then keeps the baseline until it ends, at most 30 seconds later. The IDs of the last 64 expired or evicted baselines are kept with the reason they went, and no pixels, so a late request can be told why it was refused.
- The geometry of up to 64 recent screenshots (position and size only, no pixels), so the client can click a point in a screenshot. Each one cannot be used after 5 minutes and is removed at the next screenshot.
- Element references from `find_elements`: up to 4,096, each the window's reference, its process
  identity and the control's UI Automation ID, without its name. Each one cannot be used 2 minutes
  after it was issued.
- Target references for recently listed windows. Each one cannot be used 5 minutes after the window was last observed and is removed the next time the server observes windows.
- The timings and status of the last 32 tool calls, for `get_server_status`.

The build identity in `get_server_status`, `--version`, `--print-capabilities` and the `server_started` diagnostic line is fixed at compile time: the version, the Git commit, a dirty marker and the target triple. It contains no build time, path, user name or host name.

The capability report in `get_server_status` and `--print-capabilities` lists the installed OCR language tags, readiness states and request limits. It contains no path, user name, host name or environment value. To check OCR, the server asks the OCR helper for its languages; that check reads no pixels.

The server writes diagnostics to standard error: startup errors and one line per finished tool
call with the tool name, status and timings. These lines contain no tool arguments, screenshots,
OCR text, control names, typed text or window titles. Your MCP client decides whether standard error is shown or
saved to a log.

The server writes no files by default. If you set the `CONTROLFREAK_GLOW_ERROR_LOG` environment
variable, it appends glow indicator errors to the file you name. That log contains no desktop
content.

## What the setup program changes

The setup program installs to `%LOCALAPPDATA%\Programs\ControlFreak` for the current user, without administrator rights. When you opt in to connecting a supported client (Claude Code or Codex), it:

- Runs the client's official CLI (`claude mcp add --scope user controlfreak -- '<path>'` or `codex mcp add controlfreak -- '<path>'`) instead of reading, editing or backing up third-party configuration files directly.
- Preserves any existing `controlfreak` entry rather than overwriting it (for Codex, setup only adds after `codex mcp get --json controlfreak` confirms the server is absent).
- Writes a marker file (`claude-code.managed` or `codex.managed`) in the `installer-state` folder inside the install folder recording the installed path and active profile environment (`USERPROFILE` and `CLAUDE_CONFIG_DIR` or `CODEX_HOME`).

Setup error messages use fixed strings and never repeat CLI output or configuration contents.

When you uninstall, you can choose to remove the `controlfreak` entries that setup registered when the active profile environment matches the marker. Codex verifies the exact unmodified command before calling `codex mcp remove`; Claude Code runs `claude mcp remove --scope user controlfreak` in the matching profile upon confirmation. Pre-existing entries and any `*.controlfreak-backup-*.bak` files created by v0.3.0 or earlier (which may contain credentials) are left untouched; remove old backup files manually if you no longer need them.

## Your controls

- Press **Esc** while the "An agent is using this PC" tab is on screen to stop the agent's control session.
- Remove the `controlfreak` entry from your MCP client, or uninstall ControlFreak, to stop all access.

## Contact

Report privacy or security problems privately through
[GitHub security advisories](https://github.com/Subhransu-De/ControlFreak/security/advisories/new).
For other questions, open an issue.
