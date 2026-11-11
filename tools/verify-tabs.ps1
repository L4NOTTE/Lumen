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
if(-not $ui){throw 'UI missing'}
Invoke-Cdp $ui 'Runtime.evaluate' @{expression="document.getElementById('plus').click();document.getElementById('plus').click()"}|Out-Null
Start-Sleep -Milliseconds 400
$r=Invoke-Cdp $ui 'Runtime.evaluate' @{expression="JSON.stringify(state.tabs.map(t=>t.id))";returnByValue=$true}
$before=@($r.result.result.value|ConvertFrom-Json);$id=$before[0]
Invoke-Cdp $ui 'Runtime.evaluate' @{expression="window.ipc.postMessage(JSON.stringify({cmd:'drop_tab',id:$id,index:2,detach:false}))"}|Out-Null
Start-Sleep -Milliseconds 500
$r=Invoke-Cdp $ui 'Runtime.evaluate' @{expression="JSON.stringify({order:state.tabs.map(t=>t.id),dom:[...document.querySelectorAll('#tabs .tab:not(.out)')].map(el=>+el.dataset.id)})";returnByValue=$true}
$data=$r.result.result.value|ConvertFrom-Json;$data|ConvertTo-Json -Depth 4
if($data.order[2] -ne $id -or ($data.order -join ',') -ne ($data.dom -join ',')){throw 'Tab order differs between Rust and DOM'}
Write-Output 'Rust reorder and DOM order: OK'
$r=Invoke-Cdp $ui 'Runtime.evaluate' @{expression="JSON.stringify((()=>{const tabs=[...document.querySelectorAll('#tabs .tab:not(.out)')],a=tabs[0].getBoundingClientRect(),b=tabs.at(-1).getBoundingClientRect();return {id:+tabs[0].dataset.id,x:a.left+a.width/2,y:a.top+a.height/2,end:b.right-4}})())";returnByValue=$true}
$drag=$r.result.result.value|ConvertFrom-Json
Invoke-Cdp $ui 'Runtime.evaluate' @{expression="window.__dragNodes=[...document.querySelectorAll('#tabs .tab:not(.out)')];window.__dragEntries=0;window.__dragAnimationListener=e=>{if(e.animationName==='tabIn'&&e.target.classList.contains('tab'))window.__dragEntries++};document.addEventListener('animationstart',window.__dragAnimationListener)"}|Out-Null
foreach($target in $targets){if($target.type -ne 'page'){continue};$p=Invoke-Cdp $target 'Runtime.evaluate' @{expression="!!document.getElementById('tiles')";returnByValue=$true};if($p.result.result.value){$page=$target;break}}
if(-not $page){throw 'Page fixture missing'}
$token=Invoke-Cdp $page 'Runtime.evaluate' @{expression="window.__dragPageToken=String(Math.random());JSON.stringify({token:window.__dragPageToken,origin:performance.timeOrigin})";returnByValue=$true}
Invoke-Cdp $ui 'Input.dispatchMouseEvent' @{type='mousePressed';x=$drag.x;y=$drag.y;button='left';buttons=1;clickCount=1}|Out-Null
Invoke-Cdp $ui 'Input.dispatchMouseEvent' @{type='mouseMoved';x=$drag.end;y=$drag.y;button='left';buttons=1}|Out-Null
Start-Sleep -Milliseconds 400
$preview=Invoke-Cdp $ui 'Runtime.evaluate' @{expression="JSON.stringify({drag:!!document.querySelector('.tab.drag'),shifted:document.querySelectorAll('.tab.shift').length,transitions:[...document.querySelectorAll('.tab.shift')].map(el=>getComputedStyle(el).transitionDuration)})";returnByValue=$true}
$visual=$preview.result.result.value|ConvertFrom-Json
if(-not $visual.drag -or $visual.shifted -lt 1){throw 'Live neighbour preview missing'}
Write-Output ('Live preview: '+$preview.result.result.value)
Invoke-Cdp $ui 'Input.dispatchMouseEvent' @{type='mouseReleased';x=$drag.end;y=$drag.y;button='left';buttons=0;clickCount=1}|Out-Null
Start-Sleep -Milliseconds 500
$r=Invoke-Cdp $ui 'Runtime.evaluate' @{expression="JSON.stringify({last:state.tabs.at(-1).id,dragging:!!document.querySelector('.tab.drag')})";returnByValue=$true}
$data=$r.result.result.value|ConvertFrom-Json
if($data.last -ne $drag.id -or $data.dragging){throw 'Pointer drag did not reorder the tab'}
Write-Output 'Pointer drag through toolbar handler to Rust: OK'
$r=Invoke-Cdp $ui 'Runtime.evaluate' @{expression="JSON.stringify({sameNodes:window.__dragNodes.every(el=>el.isConnected&&[...document.querySelectorAll('.tab')].find(n=>n.dataset.id===el.dataset.id)===el),entries:window.__dragEntries,styles:[...document.querySelectorAll('.tab')].every(el=>!el.style.transform&&!el.classList.contains('shift'))})";returnByValue=$true}
$visual=$r.result.result.value|ConvertFrom-Json
if(-not $visual.sameNodes -or $visual.entries -ne 0 -or -not $visual.styles){throw 'Tab recreation, repeated entrance or stale drag style'}
$after=Invoke-Cdp $page 'Runtime.evaluate' @{expression="JSON.stringify({token:window.__dragPageToken,origin:performance.timeOrigin})";returnByValue=$true}
if($after.result.result.value -ne $token.result.result.value){throw 'Page reloaded during same-window reorder'}
Write-Output 'DOM nodes retained; no repeated entrance; page state and timeOrigin retained: OK'
