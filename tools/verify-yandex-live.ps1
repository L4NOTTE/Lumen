param([string]$DataDirectory)
$port=[int]([IO.File]::ReadAllText((Join-Path $DataDirectory 'profile\lumen-debug-port')))
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
$targets=Invoke-RestMethod "http://127.0.0.1:$port/json/list"
foreach($target in $targets){if($target.type -ne 'page'){continue};$p=Invoke-Cdp $target 'Runtime.evaluate' @{expression="!!document.getElementById('devbtn')";returnByValue=$true};if($p.result.result.value){$ui=$target;break}}
Invoke-Cdp $ui 'Runtime.evaluate' @{expression="window.ipc.postMessage(JSON.stringify({cmd:'dev_close'}));window.ipc.postMessage(JSON.stringify({cmd:'go',text:'https://yandex.ru/pogoda/ru/izhevsk'}))"}|Out-Null
Start-Sleep -Milliseconds 9000
$targets=Invoke-RestMethod "http://127.0.0.1:$port/json/list"
$page=$targets|Where-Object {$_.url -like 'https://yandex.ru/pogoda/*'}|Select-Object -First 1
if(-not $page){throw 'Weather did not load'}
$r=Invoke-Cdp $page 'DOM.getDocument' @{depth=-1;pierce=$true}
$script:ads=0;$script:hidden=0
function Walk($n){
 foreach($root in $n.shadowRoots){if($root.shadowRootType -eq 'closed'){
   $flat=$root|ConvertTo-Json -Compress -Depth 100
   if($flat -match 'Реклама' -and $flat -match 'IMG|VIDEO'){
     $script:ads++
     $box=Invoke-Cdp $page 'DOM.getBoxModel' @{backendNodeId=$n.backendNodeId}
     if($box.error){$script:hidden++}
   }
 }}
 foreach($c in $n.children){Walk $c};foreach($c in $n.shadowRoots){Walk $c}
}
Walk $r.result.root
$state=Invoke-Cdp $page 'Runtime.evaluate' @{expression="JSON.stringify({runtime:window.__lumenRt,labels:window.__lumenCfg?.labels,forecast:document.body.innerText.includes('Прогноз')||document.body.innerText.includes('Сегодня')})";returnByValue=$true}
Write-Output $state.result.result.value
Write-Output "Closed ad roots: $script:ads; without visible layout: $script:hidden"
if($script:hidden -ne $script:ads){throw 'Live advertising blocks remain visible'}
if($script:ads -eq 0){Write-Output 'No live creatives loaded; closed-root behavior covered separately by fixture, not exercised on this response.'}
