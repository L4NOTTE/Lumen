param([string]$DataDirectory,[string]$BaselineRuntime,[string]$OutputFile)
. (Join-Path $PSScriptRoot 'cdp.ps1')
$port=[int]([IO.File]::ReadAllText((Join-Path $DataDirectory 'profile/lumen-debug-port')))
$targets=Invoke-RestMethod "http://127.0.0.1:$port/json/list"
foreach($target in $targets){if($target.type -ne 'page'){continue};$r=Invoke-Cdp $target 'Runtime.evaluate' @{expression="!!document.getElementById('tiles')";returnByValue=$true};if($r.result.result.value){$page=$target;break}}
if(-not $page){throw 'Fixture page missing'}
$before=[IO.File]::ReadAllText($BaselineRuntime)
$after=[IO.File]::ReadAllText((Join-Path $PSScriptRoot '../src/runtime.js'))
$results=@()
foreach($variant in @(@{name='before';script=$before},@{name='after';script=$after})){
 $js=@'
(async()=>{
 document.getElementById('perf-fixture')?.remove();const frame=document.createElement('iframe');frame.id='perf-fixture';document.body.append(frame);
 const w=frame.contentWindow,d=w.document;
 d.body.innerHTML=Array.from({length:6000},(_,i)=>'<article><span>Обычная новость '+i+'</span><span>Текст</span><span>Данные</span></article>').join('');
 w.__walked=0;const create=w.Document.prototype.createTreeWalker;
 w.Document.prototype.createTreeWalker=function(...args){const walker=create.apply(this,args),next=walker.nextNode.bind(walker);walker.nextNode=()=>{w.__walked++;return next()};return walker};
 w.eval(SCRIPT);
 await new Promise(r=>setTimeout(r,1400));w.__walked=0;
 const times=[];for(let n=0;n<5;n++){d.querySelector('article span').firstChild.nodeValue='Обновление '+n;await new Promise(r=>setTimeout(r,1150));times.push(w.__walked)}
 const result={variant:NAME,walkedNodes:w.__walked,perStep:times};frame.remove();return JSON.stringify(result);
})()
'@
 $js=$js.Replace('SCRIPT',($variant.script|ConvertTo-Json -Compress)).Replace('NAME',($variant.name|ConvertTo-Json -Compress))
 $r=Invoke-Cdp $page 'Runtime.evaluate' @{expression=$js;returnByValue=$true;awaitPromise=$true}
 if($r.result.exceptionDetails){throw ($r.result.exceptionDetails|ConvertTo-Json -Compress -Depth 6)}
 $results+=($r.result.result.value|ConvertFrom-Json)
}
$json=$results|ConvertTo-Json -Depth 5;[IO.File]::WriteAllText($OutputFile,$json);Write-Output $json
if($results[1].walkedNodes -ge $results[0].walkedNodes/10){throw 'Incremental scan did not reduce traversal sufficiently'}
