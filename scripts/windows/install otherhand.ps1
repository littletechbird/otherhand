# Installs Otherhand for the person signed in. No admin rights needed, and nothing outside your own
# profile is changed. Safe to run again: it updates the program and keeps your fence and notes.
#
#   powershell -ExecutionPolicy Bypass -File ".\install otherhand.ps1"
#   powershell -ExecutionPolicy Bypass -File ".\install otherhand.ps1" -AddToPath -AgentSnippets
#
# What it sets up:
#   %LOCALAPPDATA%\Otherhand\otherhand.exe     the program
#   %USERPROFILE%\.agent-desktop\fence.json    your fence: programs and folders the agent must never touch
#                                              (left alone if you already have one)
#   %USERPROFILE%\.darkroom\                   the Dark Room folder, for secrets the agent fills but never reads
#   %USERPROFILE%\.otherhand\jetpack\          local notes the agent keeps about this computer
#   %USERPROFILE%\.otherhand\config.json        settings, such as the screen-border glow (kept if it exists)
# The three folders are set so only you (and Windows itself) can open them.
# Plain ASCII on purpose (Windows PowerShell 5 reads BOM-less files as ANSI).
param(
  [string]$Source = '',
  [switch]$AddToPath,
  [switch]$AgentSnippets,
  # For tests only: stand-ins for your profile folder and %LOCALAPPDATA%.
  [string]$ProfileRoot = '',
  [string]$AppDataRoot = '',
  [switch]$SkipAcl
)
$ErrorActionPreference = 'Stop'
$onWindows = ($env:OS -eq 'Windows_NT')

function Say([string]$m) { Write-Host $m }

if (-not $Source) {
  foreach ($n in 'otherhand.exe', 'agent-desktop.exe') { $p = Join-Path $PSScriptRoot $n; if (Test-Path $p) { $Source = $p; break } }
}
if (-not $Source -or -not (Test-Path $Source)) { throw 'otherhand.exe was not found next to this script. Pass -Source with its path.' }

$profileDir = if ($ProfileRoot) { $ProfileRoot } else { [Environment]::GetFolderPath('UserProfile') }
$appData = if ($AppDataRoot) { $AppDataRoot } else { $env:LOCALAPPDATA }
if (-not $profileDir -or -not $appData) { throw 'Your profile folder or LOCALAPPDATA could not be found. Run this from your own Windows account.' }

if ($onWindows -and -not $ProfileRoot) {
  $me = [Security.Principal.WindowsPrincipal][Security.Principal.WindowsIdentity]::GetCurrent()
  if ($me.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)) {
    Say 'Note: this window is running as administrator. Otherhand installs for this account only either way.'
  }
}

# Only you and SYSTEM can open the folder; inherited permissions are removed.
function Lock-Down([string]$path) {
  if ($SkipAcl -or -not $onWindows) { return }
  $sid = [Security.Principal.WindowsIdentity]::GetCurrent().User.Value
  $out = & icacls.exe $path /inheritance:r /grant:r "*${sid}:(OI)(CI)F" '*S-1-5-18:(OI)(CI)F' 2>&1
  if ($LASTEXITCODE -ne 0) { Say "Could not restrict $path ($out). It still works, but check its permissions." }
}

function New-Folder([string]$path) {
  if (-not (Test-Path $path)) { New-Item -ItemType Directory -Force -Path $path | Out-Null; $made = $true } else { $made = $false }
  Lock-Down $path
  return $made
}

# 1. The program.
$installDir = Join-Path $appData 'Otherhand'
New-Item -ItemType Directory -Force -Path $installDir | Out-Null
$exe = Join-Path $installDir 'otherhand.exe'
try { Copy-Item -LiteralPath $Source -Destination $exe -Force }
catch { throw "Could not replace $exe. If Otherhand is running, close what uses it and try again. ($($_.Exception.Message))" }
$hash = (Get-FileHash -Algorithm SHA256 -LiteralPath $exe).Hash.ToLower()
Say "Installed $exe (SHA-256 $($hash.Substring(0, 12))...)"

# 2. The fence. The built-in floor (password managers, sign-in and elevation prompts, Windows Security)
# always applies whatever this file says; the list below just makes it visible and gives you a place to add.
$fenceDir = Join-Path $profileDir '.agent-desktop'
[void](New-Folder $fenceDir)
$fence = Join-Path $fenceDir 'fence.json'
if (Test-Path $fence) {
  Say "Kept your fence: $fence"
} else {
  $floor = @('keepass.exe', 'keepassxc.exe', '1password.exe', 'bitwarden.exe', 'dashlane.exe', 'lastpass.exe',
    'nordpass.exe', 'roboform.exe', 'enpass.exe', 'proton pass.exe', 'keeper.exe', 'consent.exe',
    'credentialuibroker.exe', 'logonui.exe', 'lockapp.exe', 'sechealthui.exe')
  $doc = [ordered]@{
    version = 1
    programs = [ordered]@{ process_names = $floor; exe_paths = @(); window_classes = @(); title_patterns = @() }
    paths = @()
  }
  $doc | ConvertTo-Json -Depth 4 | Set-Content -Encoding ASCII -Path $fence
  Say "Wrote a starter fence: $fence"
  Say '  Add your banking app under programs.process_names (for example "mybank.exe") and private folders under paths.'
}

# 3. Dark Room and the jetpack.
$darkroom = Join-Path $profileDir '.darkroom'
if (New-Folder $darkroom) { Say "Made the Dark Room folder: $darkroom" } else { Say "Dark Room folder is there: $darkroom" }
$jetRoot = Join-Path $profileDir '.otherhand'
[void](New-Folder $jetRoot)
$jetpack = Join-Path $jetRoot 'jetpack'
if (New-Folder $jetpack) { Say "Made the jetpack folder: $jetpack" } else { Say "Jetpack folder is there (your notes are kept): $jetpack" }

# 3b. Settings. The glow is a thin colored frame around each screen while Otherhand is acting, so you can see
# when the agent has control. It never takes clicks or focus and never shows up in screenshots.
$config = Join-Path $jetRoot 'config.json'
if (Test-Path $config) {
  Say "Kept your settings: $config"
} else {
  [ordered]@{ glow = [ordered]@{ enabled = $true; color = '#00D4FF'; thickness = 6; linger_ms = 1500 } } |
    ConvertTo-Json -Depth 3 | Set-Content -Encoding ASCII -Path $config
  Say "Wrote your settings: $config"
}
Say '  While Otherhand acts, a soft cyan glow frames your screens. Ask the agent to change it, or run'
Say '  otherhand glow show / glow set / glow reset. --no-glow turns it off for one command.'

# 4. Optional: PATH for this account only.
if ($AddToPath) {
  $userPath = [Environment]::GetEnvironmentVariable('Path', 'User')
  $parts = @("$userPath" -split ';' | Where-Object { $_ })
  if ($parts -notcontains $installDir) {
    [Environment]::SetEnvironmentVariable('Path', (($parts + $installDir) -join ';'), 'User')
    Say "Added $installDir to your PATH. Open a new terminal to use 'otherhand'."
  } else { Say 'Already on your PATH.' }
}

# 5. Optional: a block to paste into an agent's instructions file (AGENTS.md, CLAUDE.md, Cursor rules).
if ($AgentSnippets) {
  $snipDir = Join-Path $installDir 'agent snippets'
  New-Item -ItemType Directory -Force -Path $snipDir | Out-Null
  @"
## Desktop control with Otherhand

Use Otherhand to read and operate apps on this Windows computer: ``$exe``.
Every command prints one JSON object; check ``ok`` and ``error.code``.

- Start with ``otherhand skills get desktop`` and ``otherhand skills get windows`` for the full guide.
- At the start of a task run ``otherhand jetpack read`` and ``otherhand jetpack recipe find <app or task>``.
- When something works, save it: ``otherhand jetpack recipe add --app notepad.exe --role textfield "what worked"``.
  Write how to find things (refs, roles, names), never what a window or field contains.
- Commands run headless by default and must not take the screen from the person. Only use ``--headed`` when
  the person says so.
- ``FENCED`` means the person keeps that app, window or folder private. Do not try another way in.
- Fill secrets only with ``otherhand fill-secret <ref> <dr_sec_ handle>``. Never ask for, type or store the secret itself.
- When the person talks about the screen-edge glow, change it with ``otherhand glow`` and do not edit the config file by hand.
  ``otherhand glow show`` prints the saved settings. ``otherhand glow set`` writes one or more of
  ``--on``, ``--off``, ``--thickness N`` (2-24, default 12), ``--color #RRGGBB``, ``--colors #A,#B`` (up to four),
  ``--opacity N`` or ``--brightness N`` (0-100), and ``--countdown-ms N`` (stored only; there is no countdown yet).
  ``otherhand glow reset`` restores the defaults. The next command that draws the glow uses the saved values.
  "Turn the edge glow off" -> ``otherhand glow set --off``. "Make the glow thicker" -> ``otherhand glow set --thickness 18``.
  "Make it orange and a bit dimmer" -> ``otherhand glow set --color #FF8800 --opacity 60``.
  "Use cyan and purple" -> ``otherhand glow set --colors #00D4FF,#7C5CFF``. "Reset the glow" -> ``otherhand glow reset``.
"@ | Set-Content -Encoding ASCII -Path (Join-Path $snipDir 'agent instructions.md')
  # No MCP config is written: this build is a command line tool and has no MCP server mode. Agents
  # with a shell tool use it directly with the instructions above.
  Say "Agent snippets: $snipDir"
}

# 6. Smoke test: the program starts and reads the fence.
$prev = $ErrorActionPreference; $ErrorActionPreference = 'Continue'
$v = & $exe version 2>$null
$ErrorActionPreference = $prev
if ("$v" -match '"ok":\s*true') { Say 'Otherhand runs.' } else { Say "Otherhand did not answer as expected: $v" }
Say 'Done.'
