param([Parameter(Mandatory)][string]$Executable,[Parameter(Mandatory)][string]$DataDirectory)
$ErrorActionPreference='Stop'
New-Item -ItemType Directory -Path $DataDirectory | Out-Null
[IO.File]::WriteAllText((Join-Path $DataDirectory 'settings.json'),'{"startup":"new","adblock":false,"audd_token":"test-obsolete-token"}')
$env:LUMEN_DATA_DIR=$DataDirectory
$env:LUMEN_TEST_HIDDEN='1'
Remove-Item Env:LUMEN_ENABLE_CDP -ErrorAction SilentlyContinue
$app=Start-Process -FilePath $Executable -ArgumentList '--secondary' -WindowStyle Hidden -PassThru
try{
 Start-Sleep -Seconds 5
 if($app.HasExited){throw 'Application exited'}
 $processes=Get-CimInstance Win32_Process -Property ProcessId,ParentProcessId,CommandLine,Name
 $ids=[Collections.Generic.HashSet[int]]::new();[void]$ids.Add($app.Id)
 do{$count=$ids.Count;foreach($p in $processes){if($ids.Contains([int]$p.ParentProcessId)){[void]$ids.Add([int]$p.ProcessId)}}}while($count-ne $ids.Count)
 $web=@($processes|Where-Object {$ids.Contains([int]$_.ProcessId)-and $_.Name -like '*webview*'})
 if(-not $web.Count){throw 'No WebView processes observed'}
 if(@($web|Where-Object {$_.CommandLine -match '--remote-debugging-(port|pipe)'}).Count){throw 'Debug endpoint enabled by default'}
 if(Test-Path -LiteralPath (Join-Path $DataDirectory 'profile/lumen-debug-port')){throw 'Debug endpoint marker created'}
 $settings=Get-Content -LiteralPath (Join-Path $DataDirectory 'settings.json') -Raw
 if($settings -match 'audd_token|test-obsolete-token'){throw 'Obsolete token retained'}
 Write-Output 'Secure default: running WebView, no debug switches or marker; obsolete token removed: OK'
}finally{if(-not $app.HasExited){Stop-Process -Id $app.Id}}
