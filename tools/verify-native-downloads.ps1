param([Parameter(Mandatory)][string]$Executable)

$ErrorActionPreference = 'Stop'
$exe = (Resolve-Path -LiteralPath $Executable).Path
$profile = Join-Path ([IO.Path]::GetTempPath()) ('lumen-download-check-' + [guid]::NewGuid())
New-Item -ItemType Directory -Path $profile | Out-Null
$portFile = Join-Path $profile 'port'
$history = Join-Path $profile 'downloads.json'
$env:LUMEN_DATA_DIR = $profile
$env:LUMEN_DOWNLOAD_DIR = Join-Path $profile 'files'
Remove-Item Env:LUMEN_TEST_HIDDEN -ErrorAction SilentlyContinue
$fixture = Join-Path $PSScriptRoot 'download-fixture-server.cjs'
$server = Start-Process (Get-Command node).Source -ArgumentList ('"' + $fixture + '" "' + $portFile + '"') -WindowStyle Hidden -PassThru

try {
    for ($i = 0; $i -lt 70 -and !(Test-Path -LiteralPath $portFile); $i++) {
        Start-Sleep -Milliseconds 100
    }
    $origin = 'http://127.0.0.1:' + [IO.File]::ReadAllText($portFile)
    foreach ($scenario in @(
        @{ route = '/file'; name = 'sample.txt'; status = 'completed' }
        @{ route = '/file'; name = 'sample (1).txt'; status = 'completed' }
        @{ route = '/fail'; name = 'failed.bin'; status = 'failed' }
    )) {
        $browser = Start-Process $exe -ArgumentList '--secondary','--url',($origin + $scenario.route) -PassThru
        try {
            $record = $null
            for ($i = 0; $i -lt 250; $i++) {
                if ($browser.HasExited) { throw 'Browser exited during download' }
                if (Test-Path -LiteralPath $history) {
                    $record = (Get-Content -LiteralPath $history -Raw | ConvertFrom-Json) |
                        Where-Object { $_.name -eq $scenario.name -and $_.status -ne 'running' } |
                        Select-Object -First 1
                    if ($record) { break }
                }
                Start-Sleep -Milliseconds 100
            }
            if (!$record -or $record.status -ne $scenario.status) {
                throw "Unexpected download status for $($scenario.name)"
            }
            if ($scenario.status -eq 'completed' -and
                ((Get-Item -LiteralPath $record.path).Length -ne 1MB -or $record.received -ne 1MB)) {
                throw "Incorrect download size for $($scenario.name)"
            }
            Write-Output "$($scenario.name) $($record.status)"
        } finally {
            if (!$browser.HasExited) {
                $browser.Refresh()
                if (!$browser.CloseMainWindow() -or !$browser.WaitForExit(5000)) {
                    Stop-Process -Id $browser.Id
                    $browser.WaitForExit()
                }
            }
        }
    }
} finally {
    if (!$server.HasExited) { Stop-Process -Id $server.Id }
}
