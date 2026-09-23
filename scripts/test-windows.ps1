param(
    [Parameter(ValueFromRemainingArguments = $true)]
    [string[]]$TestArgs
)

$ErrorActionPreference = 'Stop'
$tauriRoot = Join-Path $PSScriptRoot '..\src-tauri'
$manifest = Join-Path $tauriRoot 'windows-test.manifest'
$mt = (Get-Command mt.exe -ErrorAction SilentlyContinue | Select-Object -First 1).Source
if (-not $mt) {
    $kitsBin = Join-Path ${env:ProgramFiles(x86)} 'Windows Kits\10\bin'
    $mt = Get-ChildItem -Path (Join-Path $kitsBin '*\x64\mt.exe') -ErrorAction SilentlyContinue |
        Sort-Object FullName -Descending |
        Select-Object -First 1 -ExpandProperty FullName
}
if (-not $mt) {
    throw 'Windows SDK mt.exe is required to run Handy unit tests on Windows.'
}

Push-Location $tauriRoot
try {
    & cargo test --lib --no-run
    if ($LASTEXITCODE -ne 0) { throw "Rust test build failed (exit $LASTEXITCODE)." }

    $testExe = Get-ChildItem -LiteralPath 'target\debug\deps' -Filter 'handy_app_lib-*.exe' |
        Sort-Object LastWriteTime -Descending |
        Select-Object -First 1
    if (-not $testExe) { throw 'The Handy Rust unit-test executable was not found.' }

    # Tauri embeds Common Controls v6 in Handy.exe, but Cargo's separate libtest
    # executable has no such manifest. Without it, Windows cannot import
    # TaskDialogIndirect and the tests fail before reaching the test harness.
    & $mt '-manifest' $manifest "-outputresource:$($testExe.FullName);#1"
    if ($LASTEXITCODE -ne 0) { throw "Embedding the test manifest failed (exit $LASTEXITCODE)." }

    & $testExe.FullName @TestArgs
    if ($LASTEXITCODE -ne 0) { throw "Rust tests failed (exit $LASTEXITCODE)." }
}
finally {
    Pop-Location
}
