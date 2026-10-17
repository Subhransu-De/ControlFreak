# Privacy

This page describes what ControlFreak does with information on your computer. It covers the
`controlfreak` MCP server and the optional Windows setup program. It is based on the source code
in this repository.

## Summary

- ControlFreak runs on your computer. It has no account, no telemetry, no analytics, no update
  check and no network code.
- It sends screenshots, text read by OCR and window information to the MCP client that started
  it, such as Claude Code or Codex. What that client does next, including sending it to an AI
  model provider, is decided by the client and its provider, not by ControlFreak.
- It keeps short-lived data in memory only, and writes no files unless you turn on its error log.
- The setup program changes the MCP configuration of the clients you select, and keeps backups of
  those files.

## What the server reads and why

The server reads desktop information only when the connected client calls a tool:

- **Screenshots** of a display, a region or a window, for the capture, wait and action tools.
  Actions return a screenshot by default so the client can see the result.
- **Text on the screen**, read by the Windows OCR engine on your computer, for
  `read_text_in_region`, `find_text_on_screen`, `wait_for_text` and `click_text`.
- **Window information**: titles, process IDs, positions and integrity levels of top-level
  windows, for `list_windows`, `wait_for_window` and the safety checks before each input action.
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

- Up to 16 visual baselines, each kept for at most 5 minutes, for the visual-change waits.
- The geometry of up to 64 recent screenshots (position and size only, no pixels), each kept for
  at most 5 minutes, so the client can click a point in a screenshot.
- Target references for recently listed windows, each kept for at most 5 minutes.
- The timings and status of the last 32 tool calls, for `get_server_status`.

The server writes diagnostics to standard error: startup errors and one line per finished tool
call with the tool name, status and timings. These lines contain no tool arguments, screenshots,
OCR text, typed text or window titles. Your MCP client decides whether standard error is shown or
saved to a log.

The server writes no files by default. If you set the `CONTROLFREAK_GLOW_ERROR_LOG` environment
variable, it appends glow indicator errors to the file you name. That log contains no desktop
content.

## What the setup program changes

The setup program installs to `%LOCALAPPDATA%\Programs\ControlFreak` for the current user, without
administrator rights. When you select an MCP client, it:

- Adds a `controlfreak` entry to that client's MCP configuration file.
- Saves a private backup of the original file next to it, named
  `<file>.controlfreak-backup-*.bak`. Configuration files can contain credentials, such as API
  keys for other MCP servers, so the backup can contain them too. The backup keeps the original
  file's access permissions.
- Records which file it changed in the `installer-state` folder inside the install folder.

Setup error messages never include configuration contents.

When you uninstall, you can choose to remove the `controlfreak` entries it added. The backups stay
on disk on purpose, so that you can restore a configuration if needed. You can delete them
yourself once you no longer need them.

## Your controls

- Press **Esc** while the blue glow is visible to stop the agent's control session.
- Remove the `controlfreak` entry from a client's MCP configuration, or uninstall ControlFreak, to
  stop all access.
- Delete `*.controlfreak-backup-*.bak` files next to your client configurations to remove the
  backups.

## Contact

Report privacy or security problems privately through
[GitHub security advisories](https://github.com/Subhransu-De/ControlFreak/security/advisories/new).
For other questions, open an issue.
