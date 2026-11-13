chrome.runtime.onMessage.addListener((message,sender,reply)=>{
  if(!message.probe||!sender.tab?.url?.startsWith('http://127.0.0.1:'))return;
  (async()=>{
    await chrome.storage.local.set({probe:'lumen'});
    const value=await chrome.storage.local.get('probe');
    const tabs=await chrome.tabs.query({});
    reply({id:chrome.runtime.id,storage:value.probe==='lumen',serviceWorker:true,tabs:tabs.length>0,tabCapture:typeof chrome.tabCapture?.getMediaStreamId==='function',offscreen:typeof chrome.offscreen?.createDocument==='function'});
  })().catch(error=>reply({error:String(error)}));
  return true;
});
