// Record only the real pointer gesture; WebView2 resolves the link and opens it.
(() => {
  addEventListener('pointerdown', event => {
    if (!event.isTrusted || !window.chrome?.webview) return;
    chrome.webview.postMessage(JSON.stringify({kind:'lumen-link-gesture',middle:event.button===1}));
  }, true);

  // Preserve the document after editing, until a new document is committed.
  // No values or form metadata cross the bridge.
  let reportedEdit = false;
  const protectDraft = event => {
    if (reportedEdit || !window.chrome?.webview ||
        !event.composedPath().some(target => target?.matches?.('input, textarea, select') ||
                                            target?.isContentEditable)) return;
    chrome.webview.postMessage('lumen-protect-draft');
    reportedEdit = true;
  };
  addEventListener('input', protectDraft, true);
  addEventListener('change', protectDraft, true);
})();
