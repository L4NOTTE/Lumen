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
foreach($target in $targets){if($target.type -ne 'page'){continue};$p=Invoke-Cdp $target 'Runtime.evaluate' @{expression="!!document.getElementById('tiles')";returnByValue=$true};if($p.result.result.value){$page=$target;break}}
if(-not $page){throw 'Test new-tab page missing'}
$html='<style>.App{width:420px}.hqPiV58pa21f7uh828xl{position:fixed;top:165px;width:420px;display:flex;justify-content:flex-end}.RLHtwz_RlwPoJIR5A33T{background:white;max-height:250px;overflow-y:scroll}</style><div class="App"><div class="VRUhPzbkVeLTTUR5ydTO" style="height:250px;background:gray"><h2>MOSCOW</h2><div class="hqPiV58pa21f7uh828xl"><button id="done" class="ud8U9hWDQ4geAHT0H9ui">ВЫПОЛНЕНО</button></div></div><div class="RLHtwz_RlwPoJIR5A33T">Песни из Shazam</div></div>'
$theme=[IO.File]::ReadAllText((Join-Path $PSScriptRoot '..\src\shazam-theme.js')).Replace("if(location.protocol!=='chrome-extension:'||location.hostname!=='mmioliijnhnoblpgimnlajmefafdfilb')return;",'')
foreach($width in @(360,470)) {
 $expression='(async()=>{const f=document.createElement("iframe");f.style.cssText="border:0;width:'+$width+'px;height:620px";const ready=new Promise(r=>f.onload=r);f.srcdoc='+($html|ConvertTo-Json -Compress)+';document.body.replaceChildren(f);await ready;const w=f.contentWindow,d=f.contentDocument;w.__lumenPopupTheme="dark";w.eval('+($theme|ConvertTo-Json -Compress)+');return JSON.stringify({width:w.innerWidth,scroll:d.documentElement.scrollWidth,app:d.querySelector(".App").getBoundingClientRect().width,buttonRight:d.getElementById("done").getBoundingClientRect().right,whiteSpace:w.getComputedStyle(d.getElementById("done")).whiteSpace,background:w.getComputedStyle(d.body).backgroundColor})})()'
 $result=Invoke-Cdp $page 'Runtime.evaluate' @{expression=$expression;returnByValue=$true;awaitPromise=$true}
 $data=$result.result.result.value|ConvertFrom-Json;$data|ConvertTo-Json
 if($data.whiteSpace -ne 'nowrap' -or $data.width -ne $width -or $data.scroll -gt $width -or $data.buttonRight -gt $width -or $data.background -ne 'rgb(27, 28, 32)'){throw 'Popup layout regression'}
}