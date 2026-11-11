param([string]$DataDirectory,[string]$Screenshot,[ValidateSet('dark','light')][string]$Theme='dark')
$consoleScreenshot=$Screenshot
. (Join-Path $PSScriptRoot 'verify-devtools-look.ps1') -DataDirectory $DataDirectory -Theme $Theme -Width 900
$targets=Invoke-RestMethod "http://127.0.0.1:$port/json/list"
$id=[regex]::Match($dev.url,'/devtools/page/([^&]+)').Groups[1].Value
$page=$targets|Where-Object id -eq $id
$log=@'
console.clear();
console.warn('LUMEN_WARNING_FIX: warning with a link https://example.com/\nSecond line stays on the same row background.');
console.error('LUMEN_ERROR_FIX: error with a link https://example.com/\nSecond line stays on the same row background.');
'@
Invoke-Cdp $page 'Runtime.evaluate' @{expression=$log}|Out-Null
Invoke-Cdp $dev 'Runtime.evaluate' @{expression="InspectorFrontendAPI.showPanel('console')"}|Out-Null
Start-Sleep -Milliseconds 1000
$js=@'
JSON.stringify((()=>{const rows=[];function walk(root){root.querySelectorAll('.console-message-wrapper').forEach(row=>{if(!/LUMEN_(WARNING|ERROR)_FIX/.test(row.textContent))return;rows.push({text:row.textContent.slice(0,35),background:getComputedStyle(row).backgroundColor,fragments:[...row.querySelectorAll('.source-code,.console-message-text')].map(el=>({background:getComputedStyle(el).backgroundColor,color:getComputedStyle(el).color})),links:row.querySelectorAll('a,.devtools-link').length})});root.querySelectorAll('*').forEach(el=>{if(el.shadowRoot)walk(el.shadowRoot)})}walk(document);return rows})())
'@
$r=Invoke-Cdp $dev 'Runtime.evaluate' @{expression=$js;returnByValue=$true}
if($r.result.exceptionDetails){throw 'Console style evaluation failed'}
$rows=@($r.result.result.value|ConvertFrom-Json);$rows|ConvertTo-Json -Depth 6
if($rows.Count -ne 2){throw 'Console fixture rows missing'}
foreach($row in $rows){
 if(-not $row.fragments.Count){throw 'Message fragment fixture missing'}
 foreach($fragment in $row.fragments){if($fragment.background -notin @('transparent','rgba(0, 0, 0, 0)')){throw 'Opaque inline message background remains'}}
}
if($rows[0].background -eq $rows[1].background){throw 'Warning and error distinction lost'}
if($consoleScreenshot){$r=Invoke-Cdp $dev 'Page.captureScreenshot' @{format='png'};[IO.File]::WriteAllBytes($consoleScreenshot,[Convert]::FromBase64String($r.result.data))}
