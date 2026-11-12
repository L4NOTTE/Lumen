param([Parameter(Mandatory)][string]$Executable,[Parameter(Mandatory)][string]$DataDirectory)
$ErrorActionPreference='Stop'
New-Item -ItemType Directory -Path $DataDirectory | Out-Null
[IO.File]::WriteAllText((Join-Path $DataDirectory 'settings.json'),'{"startup":"new","adblock":false,"obsolete_test_field":"test-obsolete-token"}')
$env:LUMEN_DATA_DIR=$DataDirectory
$env:LUMEN_TEST_HIDDEN='1'
$env:LUMEN_ENABLE_CDP='1'
$env:WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS='--remote-debugging-port=39999'
$portFile=Join-Path $DataDirectory 'fixture-port'
$resultFile=Join-Path $DataDirectory 'permission-result'
$fixture=Join-Path $PSScriptRoot 'privacy-fixture.cjs'
$server=Start-Process -FilePath (Get-Command node).Source -ArgumentList ('"'+$fixture+'" "'+$portFile+'" "'+$resultFile+'"') -WindowStyle Hidden -PassThru
$app=$null
try{
 for($i=0;$i -lt 50 -and -not (Test-Path -LiteralPath $portFile);$i++){Start-Sleep -Milliseconds 100}
 $port=[int](Get-Content -LiteralPath $portFile -Raw)
 $app=Start-Process -FilePath $Executable -ArgumentList '--secondary','--developer-mode','--url',"http://127.0.0.1:$port/" -WindowStyle Hidden -PassThru
 for($i=0;$i -lt 100 -and -not (Test-Path -LiteralPath $resultFile);$i++){Start-Sleep -Milliseconds 100}
 if($app.HasExited){throw 'Application exited'}
 if(-not (Test-Path -LiteralPath $resultFile)){throw 'Geolocation denial was not observed'}
 Start-Sleep -Milliseconds 600
 if((Get-Content -LiteralPath $resultFile -Raw) -ne 'denied'){throw 'External page invoked a new-tab command'}
 $processes=Get-CimInstance Win32_Process -Property ProcessId,ParentProcessId,CommandLine,Name
 $ids=[Collections.Generic.HashSet[int]]::new();[void]$ids.Add($app.Id)
 do{$count=$ids.Count;foreach($p in $processes){if($ids.Contains([int]$p.ParentProcessId)){[void]$ids.Add([int]$p.ProcessId)}}}while($count-ne $ids.Count)
 $web=@($processes|Where-Object {$ids.Contains([int]$_.ProcessId)-and $_.Name -like '*webview*'})
 if(-not $web.Count){throw 'No WebView processes observed'}
 if(@($web|Where-Object {$_.CommandLine -match '--remote-debugging-(port|pipe)'}).Count){throw 'Debug endpoint enabled by default'}
 if(Test-Path -LiteralPath (Join-Path $DataDirectory 'profile/lumen-debug-port')){throw 'Debug endpoint marker created'}
 $settings=Get-Content -LiteralPath (Join-Path $DataDirectory 'settings.json') -Raw
 if($settings -match 'obsolete_test_field|test-obsolete-token'){throw 'Obsolete token retained'}
 Write-Output 'Geolocation and forged new-tab command blocked, legacy CDP flags ignored: OK'
}finally{
 if($app -and -not $app.HasExited){Stop-Process -Id $app.Id}
 if(-not $server.HasExited){Stop-Process -Id $server.Id}
}
