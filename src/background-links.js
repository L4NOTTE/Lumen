// Record only the real pointer gesture; WebView2 resolves the link and opens it.
(() => {
  addEventListener('pointerdown', event => {
    if (!event.isTrusted || !window.chrome?.webview) return;
    chrome.webview.postMessage(JSON.stringify({kind:'lumen-link-gesture',middle:event.button===1}));
  }, true);
})();
