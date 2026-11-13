// Runs in an isolated content-script world; communicate only via a DOM title
// on the dedicated local fixture, never through the production Lumen bridge.
chrome.runtime.sendMessage({probe:true}).then(report=>{
  document.title='LUMEN_PROBE:'+JSON.stringify(report);
}).catch(error=>{document.title='LUMEN_PROBE:'+JSON.stringify({error:String(error)});});
