param([Parameter(Mandatory)][string]$Executable, [Parameter(Mandatory)][string]$DataDirectory, [switch]$OversizedBackground)

$ErrorActionPreference = 'Stop'
Add-Type -AssemblyName UIAutomationClient
Add-Type -AssemblyName UIAutomationTypes
$profile = (New-Item -ItemType Directory -Path $DataDirectory).FullName
if ($OversizedBackground) {
    $png = [Convert]::FromBase64String('iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+j/XcAAAAASUVORK5CYII=')
    $padded = [byte[]]::new(2250000)
    [Array]::Copy($png, $padded, $png.Length)
    [IO.File]::WriteAllText((Join-Path $profile 'background.txt'), 'data:image/png;base64,' + [Convert]::ToBase64String($padded))
    [IO.File]::WriteAllText((Join-Path $profile 'settings.json'), '{"bg_mode":"custom","adblock_level":"basic"}')
    [IO.File]::WriteAllText((Join-Path $profile 'links.json'), '[{"t":"Wallpaper fallback bookmark","u":"https://example.com/"}]')
}
$env:LUMEN_DATA_DIR = $profile
Remove-Item Env:LUMEN_TEST_HIDDEN -ErrorAction SilentlyContinue
$portFile = Join-Path $profile 'port'
$readyFile = Join-Path $profile 'page-loaded.jsonl'
$fixture = Join-Path $PSScriptRoot 'performance-fixture.cjs'
$server = Start-Process -FilePath (Get-Command node).Source -ArgumentList ('"'+$fixture+'" "'+$portFile+'" "'+$readyFile+'"') -WindowStyle Hidden -PassThru
$browser = $null

try {
    if ($OversizedBackground) {
        $browser = Start-Process -FilePath $Executable -ArgumentList '--secondary' -PassThru
        $scope = [Windows.Automation.TreeScope]::Descendants
        $hyperlink = [Windows.Automation.PropertyCondition]::new([Windows.Automation.AutomationElement]::ControlTypeProperty, [Windows.Automation.ControlType]::Hyperlink)
        $deadline = [DateTime]::UtcNow.AddSeconds(15)
        $link = $null
        do {
            $browser.Refresh()
            if ($browser.MainWindowHandle -ne 0) {
                $tree = [Windows.Automation.AutomationElement]::FromHandle($browser.MainWindowHandle)
                $link = $tree.FindAll($scope, $hyperlink) | Where-Object {
                    !$_.Current.IsOffscreen -and $_.Current.Name -match 'Wallpaper fallback bookmark|example\.com'
                } | Select-Object -First 1
                if ($link) { break }
            }
            Start-Sleep -Milliseconds 80
        } while ([DateTime]::UtcNow -lt $deadline)
        if (!$link) { throw 'New tab did not recover from the oversized background' }
        Write-Output 'The new tab and its bookmark remained visible with the oversized background.'
    } else {
        $deadline = [DateTime]::UtcNow.AddSeconds(10)
        while (!(Test-Path -LiteralPath $portFile) -and [DateTime]::UtcNow -lt $deadline) { Start-Sleep -Milliseconds 50 }
        $port = Get-Content -LiteralPath $portFile -Raw
        $browser = Start-Process -FilePath $Executable -ArgumentList '--secondary','--url',"http://127.0.0.1:$port/progressive" -PassThru
        $heading = [Windows.Automation.PropertyCondition]::new([Windows.Automation.AutomationElement]::NameProperty, 'Visible while loading')
        $deadline = [DateTime]::UtcNow.AddSeconds(15)
        $visibleWhileLoading = $false
        while ([DateTime]::UtcNow -lt $deadline -and !(Test-Path -LiteralPath $readyFile)) {
            $browser.Refresh()
            if ($browser.MainWindowHandle -ne 0) {
                $tree = [Windows.Automation.AutomationElement]::FromHandle($browser.MainWindowHandle)
                $node = $tree.FindFirst([Windows.Automation.TreeScope]::Descendants, $heading)
                if ($node -and !$node.Current.IsOffscreen -and !(Test-Path -LiteralPath $readyFile)) {
                    $visibleWhileLoading = $true
                    break
                }
            }
            Start-Sleep -Milliseconds 50
        }
        while (!(Test-Path -LiteralPath $readyFile) -and [DateTime]::UtcNow -lt $deadline) { Start-Sleep -Milliseconds 50 }
        if (!$visibleWhileLoading -or !(Test-Path -LiteralPath $readyFile)) { throw 'Progressive loading check failed' }
        Write-Output 'Page heading was visible before the delayed image and the load event.'
    }
} finally {
    if ($browser -and !$browser.HasExited) {
        if (!$browser.CloseMainWindow() -or !$browser.WaitForExit(3000)) {
            Stop-Process -Id $browser.Id -ErrorAction SilentlyContinue
        }
    }
    if (!$server.HasExited) { Stop-Process -Id $server.Id -ErrorAction SilentlyContinue }
}
