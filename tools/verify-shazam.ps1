param([string]$DataDirectory,[string]$ExtensionDirectory)
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
$ui=$null
foreach($target in $targets){if($target.type -ne 'page'){continue};$probe=Invoke-Cdp $target 'Runtime.evaluate' @{expression="!!document.getElementById('devbtn')";returnByValue=$true};if($probe.result.result.value){$ui=$target;break}}
if(-not $ui){throw 'UI target missing'}
$message=@{cmd='ext_install';input=$ExtensionDirectory}|ConvertTo-Json -Compress
Invoke-Cdp $ui 'Runtime.evaluate' @{expression="window.ipc.postMessage(JSON.stringify($message))";returnByValue=$true}|Out-Null
$installed=$false
for($i=0;$i -lt 30;$i++){Start-Sleep -Milliseconds 200;$map=Join-Path $DataDirectory 'ext_map.json';if(Test-Path -LiteralPath $map){$data=Get-Content -LiteralPath $map -Raw|ConvertFrom-Json;if($data.'mmioliijnhnoblpgimnlajmefafdfilb'){$installed=$true;break}}}
if(-not $installed){$errorResult=Invoke-Cdp $ui 'Runtime.evaluate' @{expression="document.getElementById('extmsg').textContent";returnByValue=$true};throw ('Install failed: '+$errorResult.result.result.value)}
Invoke-Cdp $ui 'Runtime.evaluate' @{expression="window.ipc.postMessage(JSON.stringify({cmd:'ext_open',id:'mmioliijnhnoblpgimnlajmefafdfilb'}))";returnByValue=$true}|Out-Null
Start-Sleep -Milliseconds 1000
$targets=Invoke-RestMethod "http://127.0.0.1:$port/json/list"
$shazam=$targets|Where-Object {$_.type -eq 'page' -and $_.url.StartsWith('chrome-extension://mmioliijnhnoblpgimnlajmefafdfilb/')}|Select-Object -First 1
if(-not $shazam){throw 'Shazam tab missing'}
$probe=Invoke-Cdp $shazam 'Runtime.evaluate' @{expression="JSON.stringify({id:chrome.runtime.id,adapter:chrome.tabCapture.capture.toString().includes('capture_start'),nativeBridge:typeof window.ipc?.postMessage,secure:isSecureContext,loaded:document.body.innerText.length>0})";returnByValue=$true}
$result=$probe.result.result.value|ConvertFrom-Json;$result|ConvertTo-Json
if(-not $result.adapter -or $result.nativeBridge -ne 'function'){throw 'Capture adapter unavailable in real extension context'}