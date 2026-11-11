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
$html='<main id="forecast">Прогноз погоды</main><section style="display:flex;width:600px"><div id="ad1" style="width:260px;height:250px"><span><span>Соц</span><span>реклама</span></span><a href="https://example.com"><img width="180" height="120">Рекламный товар</a></div><div id="ad2" style="width:260px;height:250px"><span>Реклама</span><a href="https://example.com"><img width="180" height="120">Ещё объявление</a></div></section><article id="normal" style="width:260px;height:250px"><a href="https://yandex.ru"><img width="180" height="120">Обычная новость</a></article><div id="yandex_rtb_test">RTB</div>'
$runtime=[IO.File]::ReadAllText((Join-Path $PSScriptRoot '..\src\runtime.js')).Replace('location.hostname',"'yandex.ru'")
$expression='document.body.innerHTML='+($html|ConvertTo-Json -Compress)+';window.__lumenRt=0;window.__lumenCfg={labels:true};eval('+($runtime|ConvertTo-Json -Compress)+');'
Invoke-Cdp $page 'Runtime.evaluate' @{expression=$expression;returnByValue=$true}|Out-Null
$result=Invoke-Cdp $page 'Runtime.evaluate' @{expression="new Promise(r=>setTimeout(()=>r(JSON.stringify(Object.fromEntries(['ad1','ad2','normal','forecast','yandex_rtb_test'].map(id=>[id,getComputedStyle(document.getElementById(id)).display])))),1600))";awaitPromise=$true;returnByValue=$true}
$data=$result.result.result.value|ConvertFrom-Json;$data|ConvertTo-Json
if($data.ad1 -ne 'none' -or $data.ad2 -ne 'none' -or $data.yandex_rtb_test -ne 'none' -or $data.normal -eq 'none' -or $data.forecast -eq 'none'){throw 'Yandex fixture regression'}
$closed=@'
for(const [id,ad] of [['closedAd',true],['closedNormal',false]]){
 const host=document.createElement('div');host.id=id;document.body.append(host);
 const root=host.attachShadow({mode:'closed'});
 root.innerHTML='<style>:host{all:initial!important;display:block!important}</style><span>'+(ad?'Реклама':'Прогноз')+'</span><a href="https://example.com"><img width="180" height="120">Карточка</a>';
}
new Promise(r=>setTimeout(()=>r(JSON.stringify({ad:getComputedStyle(document.getElementById('closedAd')).display,normal:getComputedStyle(document.getElementById('closedNormal')).display})),1600))
'@
$result=Invoke-Cdp $page 'Runtime.evaluate' @{expression=$closed;awaitPromise=$true;returnByValue=$true}
$data=$result.result.result.value|ConvertFrom-Json;$data|ConvertTo-Json
if($data.ad -ne 'none' -or $data.normal -eq 'none'){throw 'Closed Shadow DOM regression'}
$dynamic=@'
(async()=>{
 const normal=document.getElementById('normal'),span=document.createElement('span');span.textContent='Обычный текст';normal.append(span);
 await new Promise(r=>setTimeout(r,1100));span.firstChild.nodeValue='Реклама';
 await new Promise(r=>setTimeout(r,1100));
 const incremental=getComputedStyle(normal).display;
 window.__lumenSetLabels(false);
 const card=document.createElement('article');card.style.cssText='width:250px;height:220px';card.innerHTML='<span>Реклама</span><a href="https://example.com"><img width="180" height="120">Объявление</a>';document.body.append(card);
 await new Promise(r=>setTimeout(r,1100));const disabled=getComputedStyle(card).display;
 window.__lumenSetLabels(true);await new Promise(r=>setTimeout(r,1100));
 return JSON.stringify({incremental,disabled,reenabled:getComputedStyle(card).display,forecast:getComputedStyle(document.getElementById('forecast')).display});
})()
'@
$r=Invoke-Cdp $page 'Runtime.evaluate' @{expression=$dynamic;awaitPromise=$true;returnByValue=$true}
if($r.result.exceptionDetails){throw 'Dynamic cosmetic test threw'}
$data=$r.result.result.value|ConvertFrom-Json;$data|ConvertTo-Json
if($data.incremental -ne 'none' -or $data.disabled -eq 'none' -or $data.reenabled -ne 'none' -or $data.forecast -eq 'none'){throw 'Incremental label update or settings toggle regression'}
