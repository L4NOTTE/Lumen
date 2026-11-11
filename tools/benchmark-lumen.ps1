param([Parameter(Mandatory)][string]$Executable,[Parameter(Mandatory)][string]$DataDirectory,[Parameter(Mandatory)][string]$OutputFile,[int[]]$TabCounts=@(1,5,20))
$ErrorActionPreference='Stop'
function Invoke-Cdp($target,$method,$parameters) {
  $socket=[Net.WebSockets.ClientWebSocket]::new();$cancel=[Threading.CancellationTokenSource]::new(15000)
  try {
    $socket.ConnectAsync([Uri]$target.webSocketDebuggerUrl,$cancel.Token).GetAwaiter().GetResult() | Out-Null
    $request=@{id=1;method=$method;params=$parameters}|ConvertTo-Json -Compress -Depth 12
    $bytes=[Text.Encoding]::UTF8.GetBytes($request)
    $socket.SendAsync([ArraySegment[byte]]::new($bytes),[Net.WebSockets.WebSocketMessageType]::Text,$true,$cancel.Token).GetAwaiter().GetResult() | Out-Null
    while($true) {
      $stream=[IO.MemoryStream]::new();$buffer=[byte[]]::new(65536)
      do {$part=$socket.ReceiveAsync([ArraySegment[byte]]::new($buffer),$cancel.Token).GetAwaiter().GetResult();$stream.Write($buffer,0,$part.Count)}while(-not $part.EndOfMessage)
      $data=[Text.Encoding]::UTF8.GetString($stream.ToArray())|ConvertFrom-Json;$stream.Dispose()
      if($data.id -eq 1){return $data}
    }
  } finally {$socket.Abort();$socket.Dispose();$cancel.Dispose()}
}

function Measure-Family($rootPid){
 $all=Get-CimInstance Win32_Process -Property ProcessId,ParentProcessId
 $family=[Collections.Generic.HashSet[int]]::new();[void]$family.Add($rootPid)
 do{$before=$family.Count;foreach($p in $all){if($family.Contains([int]$p.ParentProcessId)){[void]$family.Add([int]$p.ProcessId)}}}while($family.Count -gt $before)
 $pids=@($family);$processes=Get-Process -Id $pids -ErrorAction SilentlyContinue
 $memory=($processes|Measure-Object WorkingSet64 -Sum).Sum/1MB
 $cpu1=($processes|ForEach-Object {$_.TotalProcessorTime.TotalSeconds}|Measure-Object -Sum).Sum
 $watch=[Diagnostics.Stopwatch]::StartNew();Start-Sleep -Milliseconds 2000
 $cpu2=(Get-Process -Id $pids -ErrorAction SilentlyContinue|ForEach-Object {$_.TotalProcessorTime.TotalSeconds}|Measure-Object -Sum).Sum
 @{processes=$processes.Count;workingSetMiB=[math]::Round($memory,1);idleCpuCorePercent=[math]::Round([math]::Max(0,$cpu2-$cpu1)/$watch.Elapsed.TotalSeconds*100,2)}
}
function Action($cmd,$condition){
 $expression="new Promise((resolve,reject)=>{const original=window.onState,start=performance.now();const timer=setTimeout(()=>{window.onState=original;reject(new Error('state timeout'))},15000);window.onState=s=>{original(s);if($condition){clearTimeout(timer);window.onState=original;resolve(performance.now()-start)}};window.ipc.postMessage(JSON.stringify($cmd))})"
 $r=Invoke-Cdp $ui 'Runtime.evaluate' @{expression=$expression;awaitPromise=$true;returnByValue=$true}
 if($r.result.exceptionDetails){throw ($r.result.exceptionDetails|ConvertTo-Json -Compress -Depth 5)}
 [double]$r.result.result.value
}
New-Item -ItemType Directory -Path $DataDirectory -Force|Out-Null
[IO.File]::WriteAllText((Join-Path $DataDirectory 'settings.json'),'{"adblock":false,"startup":"new","sleep_minutes":0}')
$env:LUMEN_DATA_DIR=$DataDirectory;$env:LUMEN_TEST_HIDDEN='1';$env:LUMEN_ENABLE_CDP='1'
$clock=[Diagnostics.Stopwatch]::StartNew();$app=Start-Process -FilePath $Executable -ArgumentList '--secondary' -WindowStyle Hidden -PassThru
try{
 $ui=$null
 for($attempt=0;$attempt -lt 100 -and -not $ui;$attempt++){
  Start-Sleep -Milliseconds 100
  try{$port=[int]([IO.File]::ReadAllText((Join-Path $DataDirectory 'profile/lumen-debug-port')));$targets=Invoke-RestMethod "http://127.0.0.1:$port/json/list";foreach($t in $targets){if($t.type -ne 'page'){continue};$probe=Invoke-Cdp $t 'Runtime.evaluate' @{expression="typeof state!=='undefined'&&state.tabs?.length===1&&!!document.getElementById('devbtn')";returnByValue=$true};if($probe.result.result.value){$ui=$t;break}}}catch{}
 }
 if(-not $ui){throw 'Startup timeout'}
 $startup=$clock.Elapsed.TotalMilliseconds
 $rows=@();$count=1;$create=@()
 foreach($goal in $TabCounts){
  while($count -lt $goal){$next=$count+1;$create+=Action '{cmd:"new_tab"}' "s.tabs.length===$next";$count=$next}
  Start-Sleep -Milliseconds 2000
  $usage=Measure-Family $app.Id
  $ids=(Invoke-Cdp $ui 'Runtime.evaluate' @{expression='JSON.stringify(state.tabs.map(t=>t.id))';returnByValue=$true}).result.result.value|ConvertFrom-Json
  $switch=@()
  if($goal -gt 1){for($i=0;$i -lt 10;$i++){$id=$ids[$i%$goal];$switch+=Action "{cmd:'switch',id:$id}" "s.active===$id"}}
  $sorted=@($switch|Sort-Object)
  $rows+=@{tabs=$goal;usage=$usage;switchMedianMs=if($sorted.Count){[math]::Round($sorted[[int]($sorted.Count/2)],2)}else{$null}}
 }
 $result=@{startupMs=[math]::Round($startup,1);newTabMeanMs=[math]::Round(($create|Measure-Object -Average).Average,2);rows=$rows;note='New-tab pages; blocker disabled; sum of process working sets includes shared pages; one-core CPU; state acknowledgement latency, not first rendered frame.'}
 $json=$result|ConvertTo-Json -Depth 8;[IO.File]::WriteAllText($OutputFile,$json);Write-Output $json
}finally{Stop-Process -Id $app.Id -ErrorAction SilentlyContinue}
