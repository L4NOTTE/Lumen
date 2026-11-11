param([string]$Executable,[string]$DataDirectory)
$ErrorActionPreference='Stop'
. (Join-Path $PSScriptRoot 'cdp.ps1')
New-Item -ItemType Directory -Path $DataDirectory -Force|Out-Null
$settingsFile=Join-Path $DataDirectory 'settings.json'
if(Test-Path -LiteralPath $settingsFile){throw 'Use a new isolated test directory'}
[IO.File]::WriteAllText($settingsFile,'{"adblock":false,"startup":"new"}')
$portFile=Join-Path $DataDirectory 'fixture-ports.json'
$serverScript=Join-Path $PSScriptRoot 'zoom-fixture-server.cjs'
$node=(Get-Command node).Source
$server=Start-Process -FilePath $node -ArgumentList ('"'+$serverScript+'" "'+$portFile+'"') -WindowStyle Hidden -PassThru
$script:app=$null
function Start-TestBrowser([bool]$Private=$false){
 $env:LUMEN_DATA_DIR=$DataDirectory;$env:LUMEN_TEST_HIDDEN='1';$env:LUMEN_ENABLE_CDP='1'
 $arguments=@('--secondary','--url',"$site1/one");if($Private){$arguments+='--incognito'}
 $script:app=Start-Process -FilePath $Executable -ArgumentList $arguments -WindowStyle Hidden -PassThru
 $script:ui=$null
 for($n=0;$n -lt 50 -and -not $script:ui;$n++){
  Start-Sleep -Milliseconds 150
  try{
   $file=if($Private){Get-Item -LiteralPath (Join-Path ([IO.Path]::GetTempPath()) ('Lumen-incognito-'+$script:app.Id+'/lumen-debug-port'))}else{Get-Item -LiteralPath (Join-Path $DataDirectory 'profile/lumen-debug-port')}
   $script:port=[int]([IO.File]::ReadAllText($file.FullName))
   foreach($target in (Invoke-RestMethod "http://127.0.0.1:$script:port/json/list")){
    if($target.type -ne 'page'){continue};$r=Invoke-Cdp $target 'Runtime.evaluate' @{expression="typeof state!=='undefined'&&!!document.getElementById('devbtn')";returnByValue=$true}
    if($r.result.result.value){$script:ui=$target;break}
   }
  }catch{}
 }
 if(-not $script:ui){throw 'Browser UI missing'}
}
function Send-Cmd($cmd){Invoke-Cdp $script:ui 'Runtime.evaluate' @{expression=('window.ipc.postMessage(JSON.stringify('+($cmd|ConvertTo-Json -Compress)+'))')}|Out-Null}
function Wait-Zoom($expected){
 for($n=0;$n -lt 50;$n++){
  $r=Invoke-Cdp $script:ui 'Runtime.evaluate' @{expression='JSON.stringify({zoom:state.zoom,url:state.tabs.find(t=>t.id===state.active)?.url})';returnByValue=$true}
  $s=$r.result.result.value|ConvertFrom-Json
  if($s.zoom -eq $expected -and $s.url.StartsWith($script:wantedSite)){return}
  Start-Sleep -Milliseconds 100
 }
 throw "Expected zoom $expected at $script:wantedSite; got $($r.result.result.value)"
}
function Stop-TestBrowser {
 if($script:app -and -not $script:app.HasExited){Send-Cmd @{cmd='win_close'};if(-not $script:app.WaitForExit(5000)){throw 'Browser exit timeout'}}
 $script:app=$null
 Start-Sleep -Milliseconds 300
}
try{
 for($n=0;$n -lt 40 -and -not(Test-Path -LiteralPath $portFile);$n++){Start-Sleep -Milliseconds 100}
 $ports=[IO.File]::ReadAllText($portFile)|ConvertFrom-Json
 $site1='http://127.0.0.1:'+$ports[0];$site2='http://127.0.0.1:'+$ports[1]
 $script:wantedSite=$site1;Start-TestBrowser;Wait-Zoom 100
 1..3|ForEach-Object {Send-Cmd @{cmd='zoom_in'}};Wait-Zoom 130
 Send-Cmd @{cmd='go';text="$site2/two"};$script:wantedSite=$site2;Wait-Zoom 100
 Send-Cmd @{cmd='zoom_out'};Wait-Zoom 90
 Send-Cmd @{cmd='go';text="$site1/other"};$script:wantedSite=$site1;Wait-Zoom 130
 Send-Cmd @{cmd='new_tab'};Send-Cmd @{cmd='go';text="$site1/reopened"};$script:wantedSite="$site1/reopened";Wait-Zoom 130
 $targets=Invoke-RestMethod "http://127.0.0.1:$script:port/json/list"
 $page=$targets|Where-Object {$_.url -like "$site1/reopened*"}|Select-Object -First 1
 $dpr=(Invoke-Cdp $page 'Runtime.evaluate' @{expression='devicePixelRatio';returnByValue=$true}).result.result.value
 $base=(Invoke-Cdp $script:ui 'Runtime.evaluate' @{expression='devicePixelRatio';returnByValue=$true}).result.result.value
 if([math]::Abs($dpr/$base-1.3) -gt .01){throw 'Native page zoom differs from toolbar'}
 Stop-TestBrowser;$script:wantedSite=$site1;Start-TestBrowser;Wait-Zoom 130
 Write-Output 'Site zoom restored after navigation, new tab and process restart; native DPR matches: OK'
 Stop-TestBrowser
 $before=[IO.File]::ReadAllText($settingsFile)
 Start-TestBrowser $true;Wait-Zoom 130;Send-Cmd @{cmd='zoom_in'};Wait-Zoom 140;Stop-TestBrowser
 if([IO.File]::ReadAllText($settingsFile) -ne $before){throw 'Incognito changed persistent settings'}
 Start-TestBrowser;Wait-Zoom 130;Send-Cmd @{cmd='zoom_reset'};Wait-Zoom 100;Stop-TestBrowser
 Start-TestBrowser;Wait-Zoom 100;Stop-TestBrowser
 Write-Output 'Incognito does not persist changes; reset to 100% survives restart: OK'
}finally{
 if($script:app){Stop-Process -Id $script:app.Id -ErrorAction SilentlyContinue}
 Stop-Process -Id $server.Id -ErrorAction SilentlyContinue
}
