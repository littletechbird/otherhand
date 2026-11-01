<#
A/B harness: stock agent-desktop against Otherhand on the same typing tasks.

Each task runs -Runs times per tool. Every run is judged by this script's own
UI Automation reads, never by the tool under test, and logged as PASS or FAIL
with a reason. Results go to <OutDir>\runs.csv, runs.jsonl and summary.txt.

Meant for Windows Sandbox (see ab-sandbox.wsb and README.md). It opens and
closes Edge and Notepad and moves focus around, so do not run it on a machine
someone is using.
#>
param(
    [Parameter(Mandatory = $true)][string]$Stock,
    [Parameter(Mandatory = $true)][string]$Otherhand,
    [int]$Runs = 5,
    [string]$OutDir = (Join-Path $PSScriptRoot 'results'),
    [string[]]$Tasks = @('short_notepad', 'short_web', 'long_web', 'focus_theft', 'foreground_theft', 'laggy_page'),
    [int]$LateWatchSeconds = 10,
    [int]$TypeTimeoutMs = 300000
)
$ErrorActionPreference = 'Stop'
Add-Type -AssemblyName UIAutomationClient, UIAutomationTypes, System.Windows.Forms
Add-Type @'
using System;
using System.Runtime.InteropServices;
public static class AbWin32 {
    [DllImport("user32.dll")] public static extern IntPtr GetForegroundWindow();
}
'@
New-Item -ItemType Directory -Force -Path $OutDir | Out-Null
$csv = Join-Path $OutDir 'runs.csv'
$jsonl = Join-Path $OutDir 'runs.jsonl'
$editor = Join-Path $PSScriptRoot 'fixtures\editor.html'
$edge = @("${env:ProgramFiles(x86)}\Microsoft\Edge\Application\msedge.exe", "$env:ProgramFiles\Microsoft\Edge\Application\msedge.exe") |
    Where-Object { Test-Path $_ } | Select-Object -First 1
if (-not $edge) { throw 'Microsoft Edge was not found.' }

$words = 'the quiet helper types for you while you keep your own mouse and window and nothing else moves'.Split(' ')
function New-Text([int]$count) {
    $out = New-Object System.Text.StringBuilder
    for ($i = 0; $i -lt $count; $i++) {
        [void]$out.Append($words[$i % $words.Length])
        if ($i % 17 -eq 16) { [void]$out.Append('. ') } else { [void]$out.Append(' ') }
    }
    $out.ToString().Trim()
}
$shortText = 'Hello from the sandbox, twelve words typed in one go here.'
$longText = New-Text 850
$lagText = New-Text 100

function Normalize([string]$text) {
    if ($null -eq $text) { return '' }
    (($text -replace [char]0x00A0, ' ') -replace '[\u200B\uFEFF]', '' -replace '\s+', ' ').Trim()
}

function Invoke-Tool([string]$exe, [string[]]$arguments) {
    $ErrorActionPreference = 'Continue'
    $watch = [Diagnostics.Stopwatch]::StartNew()
    $raw = & $exe @arguments 2>&1 | Out-String
    $code = $LASTEXITCODE
    $watch.Stop()
    $ok = $false
    try { $ok = [bool]((ConvertFrom-Json $raw).ok) } catch { $ok = $false }
    [pscustomobject]@{ ok = $ok; exit = $code; ms = $watch.ElapsedMilliseconds; raw = $raw.Trim() }
}

$script:findLog = ''
function Find-Ref([string]$exe, [string[]]$apps, [string]$name, [string]$role) {
    $script:findLog = ''
    foreach ($app in $apps) {
        $arguments = @('find', '--app', $app, '--first')
        if ($name) { $arguments += @('--name', $name) }
        if ($role) { $arguments += @('--role', $role) }
        $result = Invoke-Tool $exe $arguments
        $script:findLog += "find --app $app (exit $($result.exit)) => $($result.raw)`n"
        $match = [regex]::Match($result.raw, '"ref(?:_id)?"\s*:\s*"(@[^"]+)"')
        if ($match.Success) { return $match.Groups[1].Value }
    }
    $null
}

function Find-Uia([string]$windowPattern, [string]$name, [string]$processName) {
    $root = [System.Windows.Automation.AutomationElement]::RootElement
    $windows = $root.FindAll([System.Windows.Automation.TreeScope]::Children, [System.Windows.Automation.Condition]::TrueCondition)
    foreach ($window in $windows) {
        $title = $window.Current.Name
        $owner = ''
        try { $owner = (Get-Process -Id $window.Current.ProcessId).ProcessName } catch { }
        if (($windowPattern -and $title -like $windowPattern) -or ($processName -and $owner -eq $processName)) {
            if (-not $name) {
                $editCondition = New-Object System.Windows.Automation.OrCondition(
                    (New-Object System.Windows.Automation.PropertyCondition([System.Windows.Automation.AutomationElement]::ControlTypeProperty, [System.Windows.Automation.ControlType]::Document)),
                    (New-Object System.Windows.Automation.PropertyCondition([System.Windows.Automation.AutomationElement]::ControlTypeProperty, [System.Windows.Automation.ControlType]::Edit)))
                return $window.FindFirst([System.Windows.Automation.TreeScope]::Descendants, $editCondition)
            }
            $condition = New-Object System.Windows.Automation.PropertyCondition([System.Windows.Automation.AutomationElement]::NameProperty, $name)
            $found = $window.FindFirst([System.Windows.Automation.TreeScope]::Descendants, $condition)
            if ($found) { return $found }
        }
    }
    $null
}

function Read-Uia($element) {
    if (-not $element) { return $null }
    $pattern = $null
    if ($element.TryGetCurrentPattern([System.Windows.Automation.TextPattern]::Pattern, [ref]$pattern)) {
        $text = $pattern.DocumentRange.GetText(-1)
        if ($text -eq $element.Current.Name) { return '' }
        return $text
    }
    if ($element.TryGetCurrentPattern([System.Windows.Automation.ValuePattern]::Pattern, [ref]$pattern)) {
        return $pattern.Current.Value
    }
    $null
}

function Start-UserWindow {
    $process = Start-Process cmd.exe -ArgumentList '/k', 'title AB user window' -PassThru
    Start-Sleep -Milliseconds 1200
    $process
}

function Start-CursorWatch {
    $stop = Join-Path $env:TEMP ("ab-cursor-" + [guid]::NewGuid() + '.stop')
    $job = Start-Job -ArgumentList $stop -ScriptBlock {
        param($stop)
        Add-Type -AssemblyName System.Windows.Forms
        $first = [System.Windows.Forms.Cursor]::Position
        $moved = $false
        while (-not (Test-Path $stop)) {
            if ([System.Windows.Forms.Cursor]::Position -ne $first) { $moved = $true }
            Start-Sleep -Milliseconds 50
        }
        $moved
    }
    Start-Sleep -Milliseconds 800
    [pscustomobject]@{ job = $job; stop = $stop }
}

function Stop-CursorWatch($watch) {
    New-Item -ItemType File -Path $watch.stop -Force | Out-Null
    $moved = Receive-Job -Job $watch.job -Wait -AutoRemoveJob
    Remove-Item $watch.stop -Force -ErrorAction SilentlyContinue
    [bool]$moved
}

function Open-Editor([string]$query) {
    Get-Process msedge -ErrorAction SilentlyContinue | Stop-Process -Force
    Start-Sleep -Milliseconds 500
    $profileDir = Join-Path $env:TEMP ('ab-edge-' + [guid]::NewGuid())
    $url = 'file:///' + ($editor -replace '\\', '/') + $query
    Start-Process $edge -ArgumentList "--user-data-dir=$profileDir", '--no-first-run', '--no-default-browser-check', '--new-window', $url | Out-Null
    for ($i = 0; $i -lt 60; $i++) {
        Start-Sleep -Milliseconds 500
        if (Find-Uia '*AB Editor*' 'Article body' $null) { return }
    }
    throw 'The editor page did not open.'
}

function Watch-Late($element, [string]$after) {
    $deadline = (Get-Date).AddSeconds($LateWatchSeconds)
    while ((Get-Date) -lt $deadline) {
        Start-Sleep -Milliseconds 250
        if ((Normalize (Read-Uia $element)) -ne (Normalize $after)) { return $true }
    }
    $false
}

function Invoke-Run([string]$toolName, [string]$exe, [string]$task, [int]$run) {
    $text = $shortText
    $adversarial = $task -in @('focus_theft', 'foreground_theft', 'laggy_page')
    $query = ''
    if ($task -in @('long_web', 'focus_theft', 'foreground_theft')) { $text = $longText }
    if ($task -eq 'laggy_page') { $text = $lagText; $query = '?lag=400' }
    if ($task -eq 'focus_theft') { $query = '?steal=1500' }
    $appName = 'Microsoft Edge'
    $targetName = 'Article body'
    if ($task -eq 'short_web') { $targetName = 'Article title' }
    if ($task -eq 'short_notepad') {
        Get-Process notepad -ErrorAction SilentlyContinue | Stop-Process -Force
        Start-Process notepad.exe | Out-Null
        Start-Sleep -Seconds 2
        $appName = 'Notepad'
        $targetName = $null
    } else {
        Open-Editor $query
    }
    $user = Start-UserWindow
    $userWindow = [AbWin32]::GetForegroundWindow()
    if ($task -eq 'short_notepad') {
        $target = Find-Uia $null $null 'notepad'
        $ref = Find-Ref $exe @('notepad', 'Notepad') 'Text Editor' $null
        if (-not $ref) { $ref = Find-Ref $exe @('notepad', 'Notepad') $null 'textfield' }
    } else {
        $target = Find-Uia '*AB Editor*' $targetName $null
        $ref = Find-Ref $exe @('msedge', 'Microsoft Edge') $targetName $null
    }
    $decoy = Find-Uia '*AB Editor*' 'Decoy field' $null
    $before = Normalize (Read-Uia $target)
    $thief = $null
    if ($task -eq 'foreground_theft') {
        $thief = Start-Job -ScriptBlock { Start-Sleep -Milliseconds 1500; Start-Process notepad.exe }
    }
    $cursor = Start-CursorWatch
    if ($ref) {
        $result = Invoke-Tool $exe @('type', $ref, $text, '--timeout-ms', "$TypeTimeoutMs")
        $headed = $false
        if (-not $result.ok -and $result.raw -match '"code"\s*:\s*"POLICY_DENIED"') {
            # Strict headless mode refused; give both tools the same second
            # chance with physical delivery allowed, as a user would.
            $headed = $true
            $first = $result.raw
            $result = Invoke-Tool $exe @('--headed', 'type', $ref, $text, '--timeout-ms', "$TypeTimeoutMs")
            $result.raw = "headless attempt: $first`nheaded attempt: $($result.raw)"
        }
    } else {
        $headed = $false
        $result = [pscustomobject]@{ ok = $false; exit = -1; ms = 0; raw = "find returned no ref`n$script:findLog" }
    }
    $foregroundAfter = [AbWin32]::GetForegroundWindow()
    $cursorMoved = Stop-CursorWatch $cursor
    if ($thief) { Receive-Job $thief -Wait -AutoRemoveJob | Out-Null }
    $after = Read-Uia $target
    $late = Watch-Late $target $after
    $landedText = Normalize $after
    $expected = Normalize ($before + ' ' + $text)
    $landed = ($landedText -eq (Normalize $text)) -or ($landedText -eq $expected)
    $stray = $false
    $strayChars = 0
    if ($decoy) { $strayChars = (Normalize (Read-Uia $decoy)).Length; $stray = $strayChars -gt 0 }
    if ($task -eq 'foreground_theft') {
        $thiefEdit = Find-Uia $null $null 'notepad'
        if ($thiefEdit -and (Normalize (Read-Uia $thiefEdit)).Length -gt 0) { $stray = $true }
    }
    $foregroundKept = ($foregroundAfter -eq $userWindow)
    $verdict = 'PASS'
    $reason = 'typed and verified'
    if ($late) { $verdict = 'FAIL'; $reason = 'text changed after the command returned (late keys)' }
    elseif ($stray) { $verdict = 'FAIL'; $reason = 'text landed outside the target' }
    elseif ($result.ok -and -not $landed) { $verdict = 'FAIL'; $reason = 'reported success but the text did not land' }
    elseif (-not $result.ok -and $landed) { $reason = 'text landed but the tool reported failure' }
    elseif (-not $result.ok -and -not $adversarial) { $verdict = 'FAIL'; $reason = 'did not type' }
    elseif (-not $result.ok) { $reason = 'refused or stopped honestly' }
    if ($verdict -eq 'PASS' -and $cursorMoved) { $verdict = 'FAIL'; $reason = 'the mouse cursor moved' }
    $record = [pscustomobject]@{
        tool = $toolName; task = $task; run = $run; verdict = $verdict; reason = $reason
        tool_ok = $result.ok; headed_retry = $headed; landed = $landed; stray = $stray; stray_chars = $strayChars; late_keys = $late
        cursor_moved = $cursorMoved; foreground_kept = $foregroundKept; ms = $result.ms
        chars_expected = (Normalize $text).Length; chars_seen = $landedText.Length
    }
    $record | Export-Csv -Path $csv -Append -NoTypeInformation
    ($record | Select-Object *, @{ n = 'output'; e = { $result.raw } } | ConvertTo-Json -Compress) | Add-Content -Path $jsonl
    Write-Host ("{0,-10} {1,-17} run {2,2}: {3} - {4} ({5} ms)" -f $toolName, $task, $run, $verdict, $reason, $result.ms)
    Stop-Process -Id $user.Id -Force -ErrorAction SilentlyContinue
    Get-Process notepad -ErrorAction SilentlyContinue | Stop-Process -Force
}

$tools = [ordered]@{ stock = $Stock; otherhand = $Otherhand }
foreach ($task in $Tasks) {
    for ($run = 1; $run -le $Runs; $run++) {
        foreach ($name in $tools.Keys) {
            try { Invoke-Run $name $tools[$name] $task $run }
            catch { Write-Host ("{0,-10} {1,-17} run {2,2}: ERROR - {3}" -f $name, $task, $run, $_) }
        }
    }
}
Get-Process msedge -ErrorAction SilentlyContinue | Stop-Process -Force

$rows = Import-Csv $csv
$summary = $rows | Group-Object tool, task | ForEach-Object {
    $pass = @($_.Group | Where-Object verdict -eq 'PASS').Count
    "{0,-30} {1}/{2} passed" -f $_.Name, $pass, $_.Count
}
$summary | Set-Content (Join-Path $OutDir 'summary.txt')
$summary | Write-Host
