# Otherhand

Otherhand lets an AI agent work the apps on a Windows PC without taking the computer away from the person sitting at it. It reads and drives windows through Windows UI Automation, the same accessibility layer screen readers use, so it can click buttons, type into fields and read what is on screen in a window that stays in the background. You keep your mouse, your keyboard and whatever window you were using. The command is called otherhand.

Headless is the default. Every command runs behind a focus guard that checks the foreground window before and after it acts, and if anything would have stolen focus the command stops and says so rather than pushing ahead. Things that can only happen on the active window, such as opening a classic Win32 menu bar, are refused in headless mode instead of quietly yanking the window forward. You can still ask for the old behaviour with --headed when you really want a window brought up front.

There is also an exclusion fence. It is a plain file in your profile listing the programs, folders and windows the agent must never touch or read, and it fails closed: if the fence file cannot be read, or your profile folder cannot be worked out, Otherhand refuses to act at all rather than guessing. Screenshots are checked against it too, and anything fenced that shows on screen is blacked out of the picture.

For passwords and other secrets there is the Dark Room. The agent never sees the secret itself. It only holds a handle, a short dr_sec_ name, and asks Otherhand to fill that handle into a field. Otherhand looks the secret up in a local encrypted vault, checks that the window or site is one the secret is allowed to go to, types it in and forgets it. The value never shows up in the agent's output, logs or notes.

While the agent is acting, a thin glow runs around the edge of your screens so you can tell at a glance that it has control. It lets every click through and stays out of screen captures. You can change how that glow looks by telling the agent. It runs `otherhand glow` and the settings are saved in the `glow` block of `%USERPROFILE%\.otherhand\config.json`, the same file that already stores thickness. The next command that draws the frame uses them. You do not restart Otherhand.

    otherhand glow show
    otherhand glow set --thickness 18 --color #FF8800 --opacity 70
    otherhand glow set --colors #00D4FF,#7C5CFF
    otherhand glow set --off
    otherhand glow set --countdown-ms 5000
    otherhand glow reset

`glow show` prints the current settings as one JSON object. `glow set` changes any of the flags you pass. `glow reset` puts the glow block back to the defaults. Thickness is an integer from 2 to 24 and defaults to 12. A color is `#RGB` or `#RRGGBB`, and `--colors` takes one to four of them, blended from the outer edge to the inner edge. `--opacity` and `--brightness` are the same setting, an integer from 0 to 100, where 100 is the usual brightness. `--countdown-ms` only saves a millisecond timing for a planned focus-warning countdown. The countdown is not drawn. A value outside those ranges, or a color that is not hex, comes back as a JSON error and the file is left unchanged. "Turn the edge glow off" is `glow set --off`. "Make the glow thicker" is `glow set --thickness 18`. "Make it orange and a bit dimmer" is `glow set --color #FF8800 --opacity 60`. "Use cyan and purple" is `glow set --colors #00D4FF,#7C5CFF`. "Reset the glow" is `glow reset`.

The agent can also keep a small folder of notes about your machine, called the jetpack, so it does not have to relearn your apps every session, and every note is screened so secrets and fenced things cannot be written into it.

To install on Windows, download otherhand.exe from the latest release and put it in a folder together with scripts\windows\install otherhand.ps1 from this repository. Then run this from that folder in PowerShell:

    powershell -ExecutionPolicy Bypass -File ".\install otherhand.ps1" -AddToPath -AgentSnippets

No admin rights are needed and nothing outside your own user profile changes. The program goes to your local app data folder, and the installer sets up your fence, the Dark Room folder and the jetpack folder so only you and Windows can open them. Running the script again updates the program and keeps your fence and notes. The longer write-up, including where everything lives and how the glow and jetpack behave, is in docs/otherhand.md.

To build it yourself you need the Rust toolchain pinned in rust-toolchain.toml. Running cargo build --release on Windows produces agent-desktop.exe in target\release. The installer accepts that file too and installs it as otherhand.exe, and it takes every command the original agent-desktop does.

Windows is the only platform with the headless work so far. Android and Linux come next.

Otherhand is a fork of agent-desktop by Vercel Labs (github.com/vercel-labs/agent-desktop). All of the original work belongs to its authors, and the project keeps their Apache 2.0 license, which is in the LICENSE file. The NOTICE file says what changed in this fork, and the original README is kept in docs/upstream agent-desktop readme.md. The command line still exposes 61 command names, 57 operational commands. The C library ABI major is 6.
