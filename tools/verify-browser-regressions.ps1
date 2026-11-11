param([string]$Executable,[string]$DataDirectory)
$ErrorActionPreference='Stop'
. (Join-Path $PSScriptRoot 'cdp.ps1')
New-Item -ItemType Directory -Path $DataDirectory -Force|Out-Null
$settings=Join-Path $DataDirectory 'settings.json'
if(Test-Path -LiteralPath $settings){throw 'Use a fresh isolated profile'}
[IO.File]::WriteAllText($settings,'{"adblock":false,"startup":"new"}')
$env:LUMEN_DATA_DIR=$DataDirectory;$env:LUMEN_TEST_HIDDEN='1';$env:LUMEN_ENABLE_CDP='1'
$app=Start-Process -FilePath $Executable -ArgumentList '--secondary' -WindowStyle Hidden -PassThru
try{
 $ui=$null
 for($n=0;$n -lt 80 -and -not $ui;$n++){
  Start-Sleep -Milliseconds 100
  try{$port=[int]([IO.File]::ReadAllText((Join-Path $DataDirectory 'profile/lumen-debug-port')));foreach($target in (Invoke-RestMethod "http://127.0.0.1:$port/json/list")){if($target.type -ne 'page'){continue};$r=Invoke-Cdp $target 'Runtime.evaluate' @{expression="!!document.getElementById('devbtn')";returnByValue=$true};if($r.result.result.value){$ui=$target;break}}}catch{}
 }
 if(-not $ui){throw 'UI missing'}
 foreach($test in @('verify-tabs.ps1','verify-yandex.ps1','verify-shazam-layout.ps1','verify-devtools.ps1')){Write-Output "Running $test";& (Join-Path $PSScriptRoot $test) -DataDirectory $DataDirectory}
 foreach($theme in @('dark','light')){& (Join-Path $PSScriptRoot 'verify-console-style.ps1') -DataDirectory $DataDirectory -Theme $theme}
 Invoke-Cdp $ui 'Runtime.evaluate' @{expression="window.ipc.postMessage(JSON.stringify({cmd:'win_close'}))"}|Out-Null
 if(-not $app.WaitForExit(5000)){throw 'Browser did not exit'}
 Write-Output 'Browser UI regression suite: OK'
}finally{if(-not $app.HasExited){Stop-Process -Id $app.Id -ErrorAction SilentlyContinue}}
