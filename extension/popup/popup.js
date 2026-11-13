const statusDot = document.getElementById('statusDot');
const statusText = document.getElementById('statusText');
const messageEl = document.getElementById('message');
const quickForm = document.getElementById('quickDownloadForm');
const quickUrl = document.getElementById('quickUrl');
const quickDownloadBtn = document.getElementById('quickDownloadBtn');
const scanPageBtn = document.getElementById('scanPageBtn');
const openAppBtn = document.getElementById('openAppBtn');

const settingInputs = {
  enabled: document.getElementById('setEnabled'),
  interceptDownloads: document.getElementById('setIntercept'),
  showFloatingButton: document.getElementById('setFloating'),
  altClickIntercept: document.getElementById('setAltClick')
};

let statusTimer = null;

function sendMessage(type, payload = {}) {
  return new Promise((resolve) => {
    chrome.runtime.sendMessage({ type, ...payload }, (response) => {
      const error = chrome.runtime.lastError;
      resolve(error ? { ok: false, error: error.message } : (response || {}));
    });
  });
}

function sendTabMessage(tabId, type, payload = {}) {
  return new Promise((resolve) => {
    chrome.tabs.sendMessage(tabId, { type, ...payload }, (response) => {
      const error = chrome.runtime.lastError;
      resolve(error ? { ok: false, error: error.message } : (response || {}));
    });
  });
}

function showMessage(text, kind = '') {
  messageEl.textContent = text || '';
  messageEl.className = `message${kind ? ` ${kind}` : ''}`;
}

function setConnected(connected, activeDownloads = 0) {
  statusDot.classList.toggle('connected', connected);
  statusDot.classList.toggle('disconnected', !connected);
  if (!connected) {
    statusText.textContent = 'Not connected';
    return;
  }
  statusText.textContent = activeDownloads > 0 ? `Active ${activeDownloads}` : 'Connected';
}

async function refreshStatus() {
  const response = await sendMessage('GET_STATUS');
  const status = response.status || {};
  setConnected(!!response.connected, Number(status.activeDownloads) || 0);
}

async function loadSettings() {
  const response = await sendMessage('GET_SETTINGS');
  const settings = response.settings || {};
  for (const [key, input] of Object.entries(settingInputs)) {
    if (input) input.checked = settings[key] !== false;
  }
}

async function updateSetting(key, value) {
  const response = await sendMessage('UPDATE_SETTINGS', { settings: { [key]: value } });
  if (response.error) {
    showMessage(response.error, 'error');
    await loadSettings();
  }
}

function filenameFromUrl(url) {
  try {
    const parsed = new URL(url);
    const raw = parsed.pathname.split('/').filter(Boolean).pop() || 'download';
    try {
      return decodeURIComponent(raw);
    } catch {
      return raw;
    }
  } catch {
    return 'download';
  }
}

function isValidHttpUrl(value) {
  try {
    return ['http:', 'https:'].includes(new URL(value).protocol);
  } catch {
    return false;
  }
}

quickForm.addEventListener('submit', async (event) => {
  event.preventDefault();
  const url = quickUrl.value.trim();
  if (!isValidHttpUrl(url)) {
    showMessage('Enter a valid http:// or https:// URL.', 'error');
    return;
  }

  quickDownloadBtn.disabled = true;
  showMessage('Sending to Syntax Downloader...');
  try {
    const response = await sendMessage('QUICK_DOWNLOAD', {
      url,
      filename: filenameFromUrl(url)
    });
    if (response.ok) {
      showMessage('Added to the download queue.', 'success');
      quickUrl.value = '';
      await refreshStatus();
    } else {
      showMessage(response.error || 'Could not add the download.', 'error');
    }
  } finally {
    quickDownloadBtn.disabled = false;
  }
});

scanPageBtn.addEventListener('click', async () => {
  scanPageBtn.disabled = true;
  showMessage('Scanning this page...');
  try {
    const tabs = await chrome.tabs.query({ active: true, currentWindow: true });
    const tab = tabs[0];
    if (!tab?.id || !/^https?:\/\//i.test(tab.url || '')) {
      showMessage('Open a normal web page before scanning.', 'error');
      return;
    }

    const scan = await sendTabMessage(tab.id, 'SCAN_PAGE_RESOURCES');
    if (scan.error) {
      showMessage('Reload the page so the extension can scan it, then try again.', 'error');
      return;
    }

    const items = scan.items || [];
    if (items.length === 0) {
      showMessage('No downloadable files were found on this page.', 'error');
      return;
    }

    const result = await sendMessage('BATCH_DOWNLOAD', {
      items,
      referrer: tab.url,
      pageUrl: tab.url
    });
    if (result.ok) {
      const queued = Number(result.data?.queued) || items.length;
      showMessage(`Queued ${queued} file${queued === 1 ? '' : 's'}.`, 'success');
      await refreshStatus();
    } else {
      showMessage(result.error || 'Could not queue the page files.', 'error');
    }
  } finally {
    scanPageBtn.disabled = false;
  }
});

openAppBtn.addEventListener('click', async () => {
  openAppBtn.disabled = true;
  showMessage('Opening Syntax Downloader...');
  try {
    const response = await sendMessage('OPEN_APP');
    if (response.ok) {
      showMessage('Syntax Downloader is in the foreground.', 'success');
      await refreshStatus();
    } else {
      showMessage('Could not launch the desktop app. Is it installed?', 'error');
    }
  } finally {
    openAppBtn.disabled = false;
  }
});

for (const [key, input] of Object.entries(settingInputs)) {
  input?.addEventListener('change', () => updateSetting(key, input.checked));
}

document.addEventListener('DOMContentLoaded', async () => {
  await Promise.allSettled([refreshStatus(), loadSettings()]);
  statusTimer = setInterval(refreshStatus, 3000);
});

window.addEventListener('unload', () => {
  if (statusTimer) clearInterval(statusTimer);
});
