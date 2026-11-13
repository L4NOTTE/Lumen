document.getElementById('capture').onclick=async()=>{
  try{
    const [tab]=await chrome.tabs.query({active:true,currentWindow:true});
    const id=await chrome.tabCapture.getMediaStreamId({targetTabId:tab.id});
    document.getElementById('result').textContent=JSON.stringify({streamIdIssued:!!id});
  }catch(error){document.getElementById('result').textContent=String(error);}
};
