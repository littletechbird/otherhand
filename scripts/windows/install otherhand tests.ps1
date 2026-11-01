# Tests for 'install otherhand.ps1'. Runs anywhere PowerShell 7 runs (Linux included) against a scratch
# profile, so it never touches a real one. Pass -Exe with a built otherhand or agent-desktop binary.
param([Parameter(Mandatory = $true)][string]$Exe)
$ErrorActionPreference = 'Stop'
$here = $PSScriptRoot
$installer = Join-Path $here 'install otherhand.ps1'
$root = Join-Path ([IO.Path]::GetTempPath()) ("otherhand install test " + [guid]::NewGuid().ToString('N').Substring(0, 8))
$profileDir = Join-Path $root 'Users\alex'
$appData = Join-Path $root 'Users\alex\AppData\Local'
New-Item -ItemType Directory -Force -Path $profileDir, $appData | Out-Null
if (-not ($env:OS -eq 'Windows_NT')) { & chmod 700 $root $profileDir }
$src = Join-Path $root 'otherhand.exe'; Copy-Item $Exe $src
$failed = 0
function Expect([string]$name, [bool]$ok, [string]$detail = '') { if ($ok) { "pass  $name" } else { $script:failed++; "FAIL  $name $detail" } }
function Run-Exe([string[]]$a) {
  $saved = $env:HOME; $env:HOME = $profileDir
  try { $o = & (Join-Path $appData 'Otherhand/otherhand.exe') @a 2>$null } finally { $env:HOME = $saved }
  try { return ($o | ConvertFrom-Json) } catch { return $null }
}

& $installer -Source $src -ProfileRoot $profileDir -AppDataRoot $appData -SkipAcl -AgentSnippets | Out-Null
$fence = Join-Path $profileDir '.agent-desktop/fence.json'
Expect 'program copied as otherhand.exe' (Test-Path (Join-Path $appData 'Otherhand/otherhand.exe'))
Expect 'starter fence written' (Test-Path $fence)
$doc = Get-Content -Raw $fence | ConvertFrom-Json
Expect 'starter fence lists the floor' ($doc.programs.process_names -contains 'keepass.exe')
Expect 'starter fence holds only the floor' (@($doc.programs.process_names).Count -eq 16 -and @($doc.paths).Count -eq 0)
Expect 'dark room folder made' (Test-Path (Join-Path $profileDir '.darkroom'))
Expect 'jetpack folder made' (Test-Path (Join-Path $profileDir '.otherhand/jetpack'))
$cfgPath = Join-Path $profileDir '.otherhand/config.json'
Expect 'settings written' (Test-Path $cfgPath)
$cfg = Get-Content -Raw $cfgPath | ConvertFrom-Json
Expect 'glow on by default in soft cyan' ($cfg.glow.enabled -eq $true -and $cfg.glow.color -eq '#00D4FF')
Expect 'agent instructions written' (Test-Path (Join-Path $appData 'Otherhand/agent snippets/agent instructions.md'))

# The program reads the starter fence (it refuses everything if the file does not parse).
$j = Run-Exe @('jetpack', 'note', 'notepad opens untitled')
Expect 'program accepts the starter fence' ([bool]($j -and $j.ok)) ($j | ConvertTo-Json -Compress -Depth 5)
$j = Run-Exe @('jetpack', 'note', 'keepassxc.exe is where the logins are')
Expect 'jetpack refuses a fenced program' ([bool]($j -and -not $j.ok -and $j.error.code -eq 'FENCED'))

# A second run keeps the person's own fence, settings and notes.
Set-Content -Path $cfgPath -Value '{ "glow": { "enabled": false } }'
Set-Content -Path $fence -Value '{ "version": 1, "programs": { "process_names": ["mybank.exe"] }, "paths": [] }'
& $installer -Source $src -ProfileRoot $profileDir -AppDataRoot $appData -SkipAcl | Out-Null
Expect 'second run keeps the fence' ((Get-Content -Raw $fence) -match 'mybank.exe')
Expect 'second run keeps the settings' ((Get-Content -Raw $cfgPath) -match 'false')
$log = @(Get-ChildItem (Join-Path $profileDir '.otherhand/jetpack/sessions') -Filter *.md)
Expect 'second run keeps jetpack notes' ($log.Count -eq 1 -and ((Get-Content -Raw $log[0].FullName) -match 'notepad opens untitled'))
$j = Run-Exe @('jetpack', 'note', 'MyBank.exe has a login button')
Expect 'jetpack follows the edited fence' ([bool]($j -and -not $j.ok -and $j.error.code -eq 'FENCED'))

Remove-Item -Recurse -Force $root
if ($failed) { throw "$failed installer test(s) failed" } else { 'all installer tests passed' }
