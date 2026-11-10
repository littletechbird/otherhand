# Security Policy

## Reporting a Vulnerability

Please report security vulnerabilities through [GitHub private vulnerability reporting](https://github.com/vercel-labs/agent-desktop/security/advisories/new) for this repository.

Do not open a public issue for suspected vulnerabilities. Include the affected version or commit, reproduction steps, expected impact, and any relevant logs or proof-of-concept details.

## Scope

Security reports are in scope for the CLI, npm installer, FFI boundary, release artifacts, GitHub Actions workflows, and platform adapters.

## Screen glow

Commands that take focus or send input wait until the on-screen glow is visible and a countdown of at least one second has elapsed. They return `GLOW_UNAVAILABLE` when the helper cannot show that frame. The helper ignores a control window whose process image is not this executable, and names its mutex with the current user's SID when that SID can be read. A same-user process can still create windows and hold mutexes. That does not let it turn the warning off: a missing or untrusted acknowledgement refuses the command.

`--no-glow` is not a flag. `config.json` cannot set the warning off. `otherhand glow set --off` writes the person switch only after the confirmation phrase is typed on an interactive console. While that switch is set, focus-taking commands refuse. A same-user process that writes the switch file directly gets the same refusal, not a silent command.

## Dark Room vault key

`machine.key` sits next to `vault.enc.json` and is the Fernet key Dark Room uses to decrypt the vault. Otherhand reads that file as Dark Room wrote it. Wrapping it with DPAPI (`CryptProtectData`, user scope) or moving it to Credential Manager would not change the boundary this fork can hold: both APIs decrypt for any process running as the same user, and changing the file would stop Dark Room's own reader from opening the vault. The handles are opaque to an agent that stays inside the CLI. An agent with a shell as the same user can read the key. The exclusion fence still refuses Otherhand's own file and launch tools when they name the vault directory.

## Response

Maintainers will review private vulnerability reports, coordinate fixes, and publish security advisories when a vulnerability affects released versions.
