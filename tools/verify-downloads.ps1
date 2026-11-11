param([string]$Executable,[string]$DataDirectory)
$ErrorActionPreference='Stop'
. (Join-Path $PSScriptRoot 'cdp.ps1')
New-Item -ItemType Directory -Path $DataDirectory -Force|Out-Null
$settings=Join-Path $DataDirectory 'settings.json'
if(Test-Path -LiteralPath $settings){throw 'Use a fresh isolated test directory'}
[IO.File]::WriteAllText($settings,'{"adblock":false,"startup":"new"}')
$portFile=Join-Path $DataDirectory 'server-port'
$node=(Get-Command node).Source;$scriptPath=Join-Path $PSScriptRoot 'download-fixture-server.cjs'
$server=Start-Process -FilePath $node -ArgumentList ('"'+$scriptPath+'" "'+$portFile+'"') -WindowStyle Hidden -PassThru
$script:app=$null
function Start-Browser([bool]$Private=$false){
 $env:LUMEN_DATA_DIR=$DataDirectory;$env:LUMEN_DOWNLOAD_DIR=Join-Path $DataDirectory 'files';$env:LUMEN_TEST_HIDDEN='1';$env:LUMEN_ENABLE_CDP='1'
 $arguments=@('--secondary');if($Private){$arguments+='--incognito'}
 $script:app=Start-Process -FilePath $Executable -ArgumentList $arguments -WindowStyle Hidden -RedirectStandardError (Join-Path $DataDirectory ('stderr-'+[Guid]::NewGuid().ToString()+'.log')) -PassThru
 $script:ui=$null
 for($n=0;$n -lt 70 -and -not $script:ui;$n++){
  Start-Sleep -Milliseconds 120
  try{
   $profile=if($Private){Join-Path ([IO.Path]::GetTempPath()) ('Lumen-incognito-'+$script:app.Id)}else{Join-Path $DataDirectory 'profile'}
   $script:port=[int]([IO.File]::ReadAllText((Join-Path $profile 'lumen-debug-port')))
   foreach($target in (Invoke-RestMethod "http://127.0.0.1:$port/json/list")){
    if($target.type -ne 'page'){continue};$r=Invoke-Cdp $target 'Runtime.evaluate' @{expression="typeof state!=='undefined'&&!!document.getElementById('downloadsbtn')";returnByValue=$true};if($r.result.result.value){$script:ui=$target;break}
   }
  }catch{}
 }
 if(-not $script:ui){throw 'Browser UI missing'}
 Invoke-Cdp $script:ui 'Runtime.evaluate' @{expression="document.getElementById('downloadsbtn').click()"}|Out-Null
}
function Stop-Browser{
 if($script:app -and -not $script:app.HasExited){Send @{cmd='win_close'};if(-not $script:app.WaitForExit(5000)){throw 'Exit timeout'}}
 $script:app=$null;Start-Sleep -Milliseconds 300
}
function Send($cmd){Invoke-Cdp $script:ui 'Runtime.evaluate' @{expression=('window.ipc.postMessage(JSON.stringify('+($cmd|ConvertTo-Json -Compress)+'))')}|Out-Null}
function Read-Rows{(Invoke-Cdp $script:ui 'Runtime.evaluate' @{expression='JSON.stringify(downloadRows)';returnByValue=$true}).result.result.value|ConvertFrom-Json}
function Wait-Row($name,$status){
 for($n=0;$n -lt 100;$n++){if($script:app.HasExited){throw 'Browser exited during download'};$rows=@(Read-Rows);$row=$rows|Where-Object {$_.name -eq $name -and $_.status -eq $status}|Select-Object -First 1;if($row){return $row};Start-Sleep -Milliseconds 100}
 throw "Missing $status download $name; rows: $($rows|ConvertTo-Json -Compress -Depth 4)"
}
function Download($route){Send @{cmd='go';text=($base+$route)};Send @{cmd='open_panel';name='downloads'}}
try{
 for($n=0;$n -lt 50 -and -not(Test-Path -LiteralPath $portFile);$n++){Start-Sleep -Milliseconds 100}
 $base='http://127.0.0.1:'+([IO.File]::ReadAllText($portFile))
 Start-Browser;Download '/file';$running=Wait-Row 'sample.txt' 'running'
 if($running.total -ne 1048576){throw 'Content length missing'}
 $done=Wait-Row 'sample.txt' 'completed'
 if((Get-Item -LiteralPath $done.path).Length -ne 1048576 -or $done.received -ne 1048576){throw 'Downloaded file size mismatch'}
 Download '/file';$duplicate=Wait-Row 'sample (1).txt' 'completed'
 if($duplicate.path -eq $done.path){throw 'Existing download overwritten'}
 Download '/file';$null=Wait-Row 'sample (2).txt' 'running'
 Download '/file';$null=Wait-Row 'sample (3).txt' 'completed';$null=Wait-Row 'sample (2).txt' 'completed'
 Send @{cmd='go';text=($base+'/')};$page=$null
 for($n=0;$n -lt 50 -and -not $page;$n++){
  foreach($target in (Invoke-RestMethod "http://127.0.0.1:$port/json/list")){
   if($target.type -ne 'page'){continue};$r=Invoke-Cdp $target 'Runtime.evaluate' @{expression="document.title==='Download test'&&!!document.querySelector('a')";returnByValue=$true};if($r.result.result.value){$page=$target;break}
  };if(-not $page){Start-Sleep -Milliseconds 100}
 }
 if(-not $page){throw 'Download link fixture missing'}
 Invoke-Cdp $page 'Runtime.evaluate' @{expression="document.querySelector('a').click()"}|Out-Null
 Send @{cmd='open_panel';name='downloads'};$null=Wait-Row 'sample (4).txt' 'completed'
 Download '/slow';$slow=Wait-Row 'slow.bin' 'running'
 Invoke-Cdp $script:ui 'Runtime.evaluate' @{expression="window.testDownloadNode=document.querySelector('.dl-row')"}|Out-Null
 for($n=0;$n -lt 100;$n++){$slow=Wait-Row 'slow.bin' 'running';if($slow.received -gt 0){break};Start-Sleep -Milliseconds 100}
 if($slow.received -le 0 -or $slow.received -ge $slow.total){throw 'Intermediate byte progress missing'}
 $stable=(Invoke-Cdp $script:ui 'Runtime.evaluate' @{expression="window.testDownloadNode===document.querySelector('.dl-row')&&Number(document.getElementById('dlbadge').textContent)===1";returnByValue=$true}).result.result.value
 if(-not $stable){throw 'Progress replaced DOM row or active badge is wrong'}
 Send @{cmd='clear_downloads'};Start-Sleep -Milliseconds 250
 if(@(Read-Rows).Count -ne 1 -or -not(Test-Path -LiteralPath $done.path)){throw 'Clear removed active row or downloaded file'}
 $r=Invoke-Cdp $script:ui 'Runtime.evaluate' @{expression="[...document.querySelectorAll('.dl-actions button')].find(b=>b.textContent==='Отмена'&&!b.hidden)?.click()"}
 $cancelled=Wait-Row 'slow.bin' 'cancelled'
 Download '/fail';$failed=Wait-Row 'failed.bin' 'failed';if(-not $failed.error){throw 'Download failure has no explanation'}
 if(@(Read-Rows).Count -ne 2){throw 'Automatic retry duplicated the download'}
 & $node (Join-Path $PSScriptRoot 'verify-download-layout.cjs') $DataDirectory
 if($LASTEXITCODE -ne 0){throw 'Download responsive layout failed'}
 Invoke-Cdp $script:ui 'Runtime.evaluate' @{expression="const q=document.getElementById('dlq');q.value='slow';q.dispatchEvent(new Event('input'))"}|Out-Null
 $found=(Invoke-Cdp $script:ui 'Runtime.evaluate' @{expression="document.querySelectorAll('.dl-row').length===1&&document.querySelector('.dl-name').textContent==='slow.bin'";returnByValue=$true}).result.result.value
 if(-not $found){throw 'Download history search failed'}
 Invoke-Cdp $script:ui 'Runtime.evaluate' @{expression="document.getElementById('dlq').value='';document.getElementById('dlq').dispatchEvent(new Event('input'))"}|Out-Null
 Write-Output 'Live intermediate byte progress, stable DOM, badge, search and 360/800 px layout: OK'
 Write-Output 'Native download, progress, completion, unique filenames, cancel, failure and non-destructive clear: OK'
 Stop-Browser;Start-Browser
 $null=Wait-Row 'slow.bin' 'cancelled'
 $historyFile=Join-Path $DataDirectory 'downloads.json';$before=[IO.File]::ReadAllText($historyFile)
 Stop-Browser;Start-Browser $true;Download '/file';$private=Wait-Row 'sample (5).txt' 'completed';Stop-Browser
 if([IO.File]::ReadAllText($historyFile) -ne $before){throw 'Private download wrote regular history'}
 Write-Output 'Download history survives restart; incognito does not modify persistent history: OK'
}catch{
 if($script:app){Write-Output "Browser exited: $($script:app.HasExited); exit code: $($script:app.ExitCode)"}
 throw
}finally{
 if($script:app){Stop-Process -Id $script:app.Id -ErrorAction SilentlyContinue}
 Stop-Process -Id $server.Id -ErrorAction SilentlyContinue
}
