param([string]$DataDirectory,[string]$Screenshot,[switch]$InspectOnly,[int]$Width=560,[ValidateSet('dark','light')][string]$Theme='dark')
. (Join-Path $PSScriptRoot 'cdp.ps1')
$port=[int]([IO.File]::ReadAllText((Join-Path $DataDirectory 'profile/lumen-debug-port')))
$targets=Invoke-RestMethod "http://127.0.0.1:$port/json/list"
foreach($target in $targets){if($target.type -ne 'page'){continue};$r=Invoke-Cdp $target 'Runtime.evaluate' @{expression="!!document.getElementById('devbtn')";returnByValue=$true};if($r.result.result.value){$ui=$target;break}}
Invoke-Cdp $ui 'Runtime.evaluate' @{expression="if(!state.dev_open)window.ipc.postMessage(JSON.stringify({cmd:'dev_tools'}))"}|Out-Null
Start-Sleep -Milliseconds 3500
$targets=Invoke-RestMethod "http://127.0.0.1:$port/json/list"
$dev=$targets|Where-Object {$_.url -match '/devtools/inspector.html'}|Select-Object -First 1
Invoke-Cdp $ui 'Runtime.evaluate' @{expression="window.ipc.postMessage(JSON.stringify({cmd:'dev_width',width:$Width}))"}|Out-Null
Start-Sleep -Milliseconds 300
$setup=@'
(async()=>{const UI=await import('./ui/legacy/legacy.js');const action=UI.ActionRegistry.ActionRegistry.instance().actions().find(a=>a.id()==='emulation.toggle-device-mode');if(action?.toggled())await action.execute();window.onState({incognito:false,set:{theme:THEME},look:{al:'#4f46e5',ad:'#8b8dff'}})})()
'@
$setup=$setup.Replace('THEME',($Theme|ConvertTo-Json -Compress))
$setupResult=Invoke-Cdp $dev 'Runtime.evaluate' @{expression=$setup;awaitPromise=$true;returnByValue=$true}
if($setupResult.result.exceptionDetails){throw ($setupResult.result.exceptionDetails|ConvertTo-Json -Compress -Depth 6)}
Start-Sleep -Milliseconds 300
$js=@'
JSON.stringify((()=>{const icons=[],codes=[];const walk=root=>{root.querySelectorAll('devtools-icon').forEach(el=>{if(icons.length<35)icons.push({name:el.name,mask:getComputedStyle(el.shadowRoot?.querySelector('span')||el).maskImage.slice(0,35)})});root.querySelectorAll('.webkit-html-tag-name,.cm-content,.source-code').forEach(el=>{if(codes.length<5)codes.push({class:el.className,font:getComputedStyle(el).fontFamily,size:getComputedStyle(el).fontSize,background:getComputedStyle(el).backgroundColor,line:getComputedStyle(el).lineHeight,color:getComputedStyle(el).color})});root.querySelectorAll('*').forEach(el=>{if(el.shadowRoot)walk(el.shadowRoot)})};walk(document);return {icons,codes,nativeTheme:document.documentElement.className,version:document.getElementById('lumen-devtools-host')?.dataset.design,top:document.querySelector('.root-view')?.getBoundingClientRect().top,headerFits:(()=>{const h=document.getElementById('lumen-devtools-host')?.shadowRoot.querySelector('header');return h&&h.scrollWidth<=h.clientWidth})()}})())
'@
$r=Invoke-Cdp $dev 'Runtime.evaluate' @{expression=$js;returnByValue=$true};$r.result.result.value
if(-not $InspectOnly){$data=$r.result.result.value|ConvertFrom-Json;if($data.version -ne '2' -or -not $data.codes.Count -or $data.codes[0].size -ne '13px' -or $data.top -ne 44 -or -not $data.headerFits){throw 'Lumen code theme or geometry missing'};if(@($data.icons|Where-Object {$_.mask -like '*data:image/svg*'}).Count -lt 4){throw 'Custom SVG icons not applied'}}
if(-not $InspectOnly -and $Theme -eq 'light' -and @($data.codes|Where-Object {$_.class -like '*elements-tree-outline*' -and $_.background -ne 'rgb(247, 248, 252)'}).Count -gt 0){throw 'Light code background has wrong contrast'}
if($Screenshot){$r=Invoke-Cdp $dev 'Page.captureScreenshot' @{format='png'};[IO.File]::WriteAllBytes($Screenshot,[Convert]::FromBase64String($r.result.data))}
