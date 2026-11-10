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
Start-Sleep -Milliseconds 1200
$ui=$null;$dev=$null
for($attempt=0;$attempt -lt 8;$attempt++) {
  $targets=Invoke-RestMethod "http://127.0.0.1:$port/json/list"
  foreach($target in $targets) {
    if($target.type -ne 'page'){continue}
    try {
      $probe=Invoke-Cdp $target 'Runtime.evaluate' @{expression="JSON.stringify({ui:!!document.getElementById('devbtn'),dev:!!(document.getElementById('notice')||document.getElementById('lumen-devtools-host'))})";returnByValue=$true}
      if($probe.result.result.value){$meta=$probe.result.result.value|ConvertFrom-Json;if($meta.ui){$ui=$target};if($meta.dev){$dev=$target}}
    } catch { }
  }
  if($ui -and $dev){break};Start-Sleep -Milliseconds 500
}
if(-not $ui -or -not $dev){throw 'Lumen UI targets not found'}
$hints=Invoke-Cdp $ui 'Runtime.evaluate' @{expression="(async()=>{const b=document.getElementById('devbtn');b.title='Test label';await new Promise(r=>setTimeout(r,40));const s=getComputedStyle(b);return JSON.stringify({titles:document.querySelectorAll('[title]').length,label:b.getAttribute('aria-label'),shadow:s.boxShadow,transform:s.transform})})()";returnByValue=$true;awaitPromise=$true}
$hintState=$hints.result.result.value|ConvertFrom-Json
if($hintState.titles -ne 0 -or $hintState.label -ne 'Test label' -or $hintState.shadow -ne 'none' -or $hintState.transform -ne 'none'){throw 'Button appearance regression'}
Write-Output 'Native hints removed; accessible labels retained; no button shadow or scale: OK'
$controls=Invoke-Cdp $ui 'Runtime.evaluate' @{expression="JSON.stringify({close:typeof document.getElementById('wclose').onclick,maximize:typeof document.getElementById('wmax').onclick,zoom:typeof document.getElementById('zin').onclick,drag:typeof dragZone})";returnByValue=$true}
$buttons=$controls.result.result.value|ConvertFrom-Json
foreach($property in $buttons.PSObject.Properties){if($property.Value -ne 'function'){throw ('Missing handler: '+$property.Name)}}
Write-Output 'Window controls, zoom and drag initialization: OK'
$before=Invoke-Cdp $ui 'Runtime.evaluate' @{expression='state.tabs.length';returnByValue=$true}
Invoke-Cdp $ui 'Runtime.evaluate' @{expression="document.getElementById('plus').click()";returnByValue=$true}|Out-Null
Start-Sleep -Milliseconds 400
$after=Invoke-Cdp $ui 'Runtime.evaluate' @{expression='state.tabs.length';returnByValue=$true}
if($after.result.result.value -le $before.result.result.value){throw 'New tab button did not work'}
Write-Output 'New tab through UI handler: OK'
$opened=Invoke-Cdp $ui 'Runtime.evaluate' @{expression="if(!state.dev_open)window.ipc.postMessage(JSON.stringify({cmd:'dev_tools'}))";returnByValue=$true}
if($opened.result.exceptionDetails){throw ($opened.result.exceptionDetails|ConvertTo-Json -Compress)}
Start-Sleep -Milliseconds 4000
$check=Invoke-Cdp $dev 'Runtime.evaluate' @{expression="JSON.stringify({url:location.href,title:document.title.slice(0,40),customHeader:!!document.getElementById('lumen-devtools-host'),tabs:(()=>{const labels=new Set();const visit=root=>{root.querySelectorAll('[role=tab]').forEach(e=>labels.add(e.getAttribute('aria-label')||e.textContent.trim()));root.querySelectorAll('*').forEach(e=>{if(e.shadowRoot)visit(e.shadowRoot)});};visit(document);return [...labels]})(),rootTop:document.querySelector('.root-view')?.getBoundingClientRect().top,shadows:[...document.querySelectorAll('*')].filter(e=>e.shadowRoot).length})";returnByValue=$true}
$result=$check.result.result.value|ConvertFrom-Json; $result|ConvertTo-Json -Depth 8; if(-not $result.customHeader -or $result.tabs.Count -lt 4){throw 'Complete DevTools UI did not initialize'}foreach($panel in @('console','sources','network','timeline','resources')) {
  $switch=Invoke-Cdp $dev 'Runtime.evaluate' @{expression="typeof InspectorFrontendAPI.showPanel === 'function' && (InspectorFrontendAPI.showPanel('$panel'),true)";returnByValue=$true}
  Start-Sleep -Milliseconds 400
  $selected=Invoke-Cdp $dev 'Runtime.evaluate' @{expression="JSON.stringify((()=>{const names=[];const visit=r=>{r.querySelectorAll('[role=tab][aria-selected=true]').forEach(e=>names.push(e.getAttribute('aria-label')||e.textContent.trim()));r.querySelectorAll('*').forEach(e=>{if(e.shadowRoot)visit(e.shadowRoot)});};visit(document);return names})())";returnByValue=$true}
  Write-Output ($panel+': '+$selected.result.result.value)
}