# Security

ControlFreak is security-sensitive because it observes and controls the Windows desktop.

The MCP can capture displays, regions, and windows; run local Windows OCR; retain up to sixteen
short-lived visual baselines in memory; move/click/drag/scroll the pointer; focus existing windows;
and inject keyboard input. Mouse clicks and drags are bounded atomic operations: the server
does not expose raw button-down/button-up tools and attempts release cleanup after partial failures.
The semantic text-click tool rejects zero or ambiguous OCR matches before injecting input.
These are powerful local operations. Run it only for MCP clients you trust, and
report suspected security problems privately through GitHub's security advisory interface instead
of a public issue.

ControlFreak deliberately exposes no application-launch, shell, filesystem, clipboard, registry,
network, or elevation tool. That is a tool-surface boundary rather than a security sandbox: keyboard
input can still reach functionality exposed by an already-running desktop application.

## Process integrity and elevation

ControlFreak inherits the Windows access token of the MCP client that starts it. It never requests
elevation itself, but an elevated client would otherwise make ControlFreak elevated too. The server
therefore inspects its token before opening the MCP transport and refuses an elevated token by
default. The refusal is a JSON error on stderr with code `elevated_operation_requires_opt_in`.

Only start an elevated server when the task genuinely requires it:

```powershell
controlfreak --allow-elevated
```

This option is a deliberate trust decision for the whole server lifetime, not permission to cross
Windows integrity boundaries silently. Before every state-changing action, and again immediately
before each input batch, ControlFreak verifies the unlocked interactive `Default` input desktop and
the current target process integrity. A target above the server's integrity is refused with
`higher_integrity_target`; an unverifiable target is refused with `target_integrity_unavailable`.
UAC secure desktop, the lock screen, disconnected sessions, and non-default input desktops remain
blocked even when `--allow-elevated` is present.

`get_server_status` and `--print-capabilities` report `server_elevated`, the Windows integrity level,
and whether elevated operation was explicitly allowed. Elevated mutation sessions replace the
standard blue particles and their glow with red counterparts; all other indicator behavior and
styling remains unchanged. The capture-excluded indicator runs as a native Rust helper
mode of the same executable, without launching PowerShell or materializing executable scripts. A
per-helper Windows job object contains it when one can be created; if that fails the helper still
starts with cooperative cleanup only, and `get_server_status` reports whether containment is active.

Future platform capabilities must declare their permission requirements and preserve these explicit
boundaries.

For transport diagnosis, the server emits process lifecycle plus tool name, operation ID, status,
and duration to stderr. These events deliberately exclude tool arguments, typed text, OCR contents,
window titles, screenshots, and image data. ControlFreak writes no diagnostics to disk by default.
Setting `CONTROLFREAK_GLOW_ERROR_LOG` opts into an append-only log at the given path, recording
indicator helper lifecycle, helper stderr, and error text. That log carries no desktop content.
