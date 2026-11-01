# Runs once when Windows Sandbox starts. Fetches the stock agent-desktop
# release if it is not already in the mapped folder, then runs the A/B
# harness and leaves the results in C:\ab\results.
$ErrorActionPreference = 'Stop'
$root = 'C:\ab'
$bin = Join-Path $root 'bin'
New-Item -ItemType Directory -Force -Path $bin | Out-Null
$stock = Join-Path $bin 'agent-desktop.exe'
if (-not (Test-Path $stock)) {
    $version = 'v1.0.2'
    $url = "https://github.com/vercel-labs/agent-desktop/releases/download/$version/agent-desktop-$version-x86_64-pc-windows-msvc.tar.gz"
    $archive = Join-Path $env:TEMP 'agent-desktop.tar.gz'
    [Net.ServicePointManager]::SecurityProtocol = [Net.SecurityProtocolType]::Tls12
    Invoke-WebRequest -UseBasicParsing -Uri $url -OutFile $archive
    $unpack = Join-Path $env:TEMP 'agent-desktop-unpack'
    New-Item -ItemType Directory -Force -Path $unpack | Out-Null
    tar -xzf $archive -C $unpack
    $found = Get-ChildItem -Path $unpack -Recurse -Filter 'agent-desktop.exe' | Select-Object -First 1
    Copy-Item $found.FullName $stock
}
$otherhand = Join-Path $bin 'otherhand.exe'
if (-not (Test-Path $otherhand)) {
    throw "Put the otherhand.exe build you want to test in $bin before starting the sandbox."
}
$runs = 5
$runsFile = Join-Path $root 'runs.txt'
if (Test-Path $runsFile) { $runs = [int](Get-Content $runsFile -TotalCount 1) }
& (Join-Path $root 'tests\ab-harness\run-ab.ps1') -Stock $stock -Otherhand $otherhand -Runs $runs -OutDir (Join-Path $root 'results')
