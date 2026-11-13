/* ============================================================
   Syntax Downloader — Background Service Worker
   Manifest V3, resilient reconnection, smart interception
   ============================================================ */

let activeAppBase = 'http://127.0.0.1:36855';

const DEFAULT_SETTINGS = {
  enabled: true,
  interceptDownloads: true,
  showFloatingButton: true,
  altClickIntercept: true,
  speedProfile: 'high',
  excludedDomains: [],
  catchFileTypes: [
    '.zip', '.rar', '.7z', '.tar', '.gz', '.bz2', '.xz', '.iso',
    '.exe', '.msi', '.dmg', '.deb', '.rpm', '.apk', '.bin',
    '.mp4', '.mkv', '.avi', '.mov', '.flv', '.webm', '.m4v',
    '.mp3', '.flac', '.wav', '.aac', '.ogg', '.m4a',
    '.pdf', '.doc', '.docx', '.xls', '.xlsx', '.ppt', '.pptx'
  ]
};

const PROFILE_SEGMENTS = { normal: 8, high: 16, turbo: 32 };
const HEARTBEAT_ALARM = 'syntax-downloader-heartbeat';
const RECENT_SEND_WINDOW_MS = 3000;
const INTERCEPT_ID_TTL_MS = 60000;

let appConnected = false;
let activeDownloadCount = 0;
let launchPromise = null;

const interceptedIds = new Map();
const recentSends = new Map();
const pendingSends = new Map();

/* ---------- Settings Management ---------- */

function normalizeSettings(raw) {
  const settings = { ...DEFAULT_SETTINGS, ...(raw || {}) };
  settings.enabled = settings.enabled !== false;
  settings.interceptDownloads = settings.interceptDownloads !== false;
  settings.showFloatingButton = settings.showFloatingButton !== false;
  settings.altClickIntercept = settings.altClickIntercept !== false;
  settings.speedProfile = ['normal', 'high', 'turbo'].includes(settings.speedProfile)
    ? settings.speedProfile
    : 'high';
  settings.excludedDomains = Array.isArray(settings.excludedDomains)
    ? settings.excludedDomains.map((value) => String(value).trim().toLowerCase()).filter(Boolean)
    : [];
  settings.catchFileTypes = Array.isArray(settings.catchFileTypes)
    ? settings.catchFileTypes
        .map((value) => String(value).trim().toLowerCase())
        .filter((value) => value.startsWith('.') || value === '*')
    : [];
  return settings;
}

async function getSettings() {
  try {
    const data = await chrome.storage.local.get('settings');
    return normalizeSettings(data.settings);
  } catch {
    return normalizeSettings(null);
  }
}

async function saveSettings(settings) {
  await chrome.storage.local.set({ settings: normalizeSettings(settings) });
}

/* ---------- App Reachability & Connection ---------- */

async function pingApp() {
  try {
    const res = await fetch(`${activeAppBase}/api/status`, {
      signal: AbortSignal.timeout(1500)
    });
    if (res.ok) {
      const data = await res.json().catch(() => ({}));
      activeAppBase = 'http://127.0.0.1:36855';
      appConnected = true;
      activeDownloadCount = Number(data.activeDownloads) || 0;
      updateBadge();
      return data;
    }
  } catch {
    // App is not reachable.
  }
  appConnected = false;
  activeDownloadCount = 0;
  updateBadge();
  return null;
}

async function ensureAppConnected() {
  if (await pingApp()) return true;

  if (!launchPromise) {
    launchPromise = new Promise((resolve) => {
      try {
        chrome.runtime.sendNativeMessage(
          'com.syntax.downloader',
          { command: 'launch' },
          (response) => {
            const error = chrome.runtime.lastError;
            if (error) {
              console.error('Could not launch Syntax Downloader:', error.message);
              resolve(false);
              return;
            }
            if (!response?.ok) {
              console.error(
                'Could not launch Syntax Downloader:',
                response?.error || 'Native host failed'
              );
              resolve(false);
              return;
            }
            resolve(true);
          }
        );
      } catch (error) {
        console.error('Native messaging is unavailable:', error);
        resolve(false);
      }
    }).finally(() => {
      launchPromise = null;
    });
  }

  if (!(await launchPromise)) return false;
  for (let attempt = 0; attempt < 20; attempt++) {
    if (await pingApp()) return true;
    await new Promise((resolve) => setTimeout(resolve, 250));
  }
  return false;
}

async function focusAppWindow() {
  try {
    const response = await fetch(`${activeAppBase}/api/focus`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: '{}',
      signal: AbortSignal.timeout(2000)
    });
    if (!response.ok) throw new Error(`HTTP ${response.status}`);
    return true;
  } catch (error) {
    console.error('Could not bring Syntax Downloader to the foreground:', error);
    showNotification(
      'Syntax Downloader',
      'The download was sent, but the app window could not be brought to the foreground.'
    );
    return false;
  }
}

/* ---------- Cookie Extraction ---------- */

async function getCookiesForUrl(downloadUrl) {
  try {
    const cookies = await chrome.cookies.getAll({ url: downloadUrl });
    if (!cookies || cookies.length === 0) return '';
    return cookies.map((cookie) => `${cookie.name}=${cookie.value}`).join('; ');
  } catch {
    return '';
  }
}

/* ---------- File Matcher ---------- */

function safeDecode(value) {
  try {
    return decodeURIComponent(value);
  } catch {
    return value;
  }
}

function filenameFromUrl(url) {
  try {
    const parsed = new URL(url);
    const name = safeDecode(parsed.pathname.split('/').filter(Boolean).pop() || '');
    return name || 'download';
  } catch {
    return 'download';
  }
}

function basename(value) {
  return String(value || '').split(/[/\\]/).filter(Boolean).pop() || '';
}

function urlExtension(url, filename) {
  const fromName = basename(filename).toLowerCase();
  if (fromName.includes('.')) {
    return fromName.slice(fromName.lastIndexOf('.'));
  }
  try {
    const path = new URL(url).pathname.toLowerCase();
    const last = path.split('/').filter(Boolean).pop() || '';
    if (last.includes('.')) return last.slice(last.lastIndexOf('.'));
  } catch {
    // Invalid URL; no extension.
  }
  return '';
}

function isDomainExcluded(hostname, excludedDomains) {
  const host = String(hostname || '').toLowerCase();
  return excludedDomains.some((entry) => {
    const domain = entry.replace(/^\*\./, '').replace(/^\./, '');
    return host === domain || host.endsWith(`.${domain}`);
  });
}

function isMimeAllowed(mime, catchFileTypes) {
  if (!mime || catchFileTypes.includes('*')) return catchFileTypes.includes('*');
  const value = String(mime).toLowerCase();
  const checks = [
    [value.startsWith('video/'), ['.mp4', '.mkv', '.avi', '.mov', '.flv', '.webm', '.m4v']],
    [value.startsWith('audio/'), ['.mp3', '.flac', '.wav', '.aac', '.ogg', '.m4a']],
    [value === 'application/pdf', ['.pdf']],
    [value.includes('zip'), ['.zip']],
    [value.includes('rar'), ['.rar']],
    [value.includes('7z'), ['.7z']],
    [value.includes('msdownload') || value.includes('x-msdownload'), ['.exe', '.msi', '.bin']]
  ];
  return checks.some(([matches, extensions]) =>
    matches && extensions.some((extension) => catchFileTypes.includes(extension))
  );
}

function shouldIntercept(url, filename, mime, settings) {
  if (!settings.enabled || !settings.interceptDownloads) return false;

  let parsed;
  try {
    parsed = new URL(url);
  } catch {
    return false;
  }
  if (!['http:', 'https:'].includes(parsed.protocol)) return false;
  if (isDomainExcluded(parsed.hostname, settings.excludedDomains)) return false;

  const allowed = settings.catchFileTypes;
  if (allowed.length === 0) return true;
  if (allowed.includes('*')) return true;

  const extension = urlExtension(url, filename);
  if (extension && allowed.includes(extension)) return true;
  return isMimeAllowed(mime, allowed);
}

/* ---------- Forward Downloads to Desktop App ---------- */

function pruneRecentSends(now) {
  for (const [url, timestamp] of recentSends) {
    if (now - timestamp > RECENT_SEND_WINDOW_MS * 10) recentSends.delete(url);
  }
  if (recentSends.size > 200) {
    const oldest = [...recentSends.entries()]
      .sort((a, b) => a[1] - b[1])
      .slice(0, recentSends.size - 100);
    for (const [url] of oldest) recentSends.delete(url);
  }
}

async function sendToApp(url, filename, referrer, options = {}) {
  const pageUrl = options.pageUrl || referrer || '';

  if (typeof url !== 'string' || !/^https?:\/\//i.test(url)) {
    if (options.isStream && /^https?:\/\//i.test(pageUrl)) {
      url = pageUrl;
    } else {
      return { error: 'Only HTTP and HTTPS URLs can be downloaded.' };
    }
  }

  const now = Date.now();
  pruneRecentSends(now);
  const lastTime = recentSends.get(url);
  if (lastTime && now - lastTime < RECENT_SEND_WINDOW_MS) {
    return { success: true, duplicate: true };
  }
  if (pendingSends.has(url)) {
    return pendingSends.get(url);
  }

  const task = performSend(url, filename, referrer, options).finally(() => {
    pendingSends.delete(url);
  });
  pendingSends.set(url, task);
  return task;
}

async function performSend(url, filename, referrer, options) {
  const connected = await ensureAppConnected();
  if (!connected) {
    notifyActiveTabAppNotRunning();
    showNotification(
      'Syntax Downloader App Not Running',
      'Launch the Windows desktop app to enable accelerated downloads.'
    );
    return { error: 'Syntax Downloader is not running.' };
  }

  const settings = await getSettings();
  const cookies = await getCookiesForUrl(url);
  const requested = Number(options.maxSegments);
  const profileLimit = PROFILE_SEGMENTS[settings.speedProfile] || 16;
  const maxSegments = Math.max(
    1,
    Math.min(32, Number.isFinite(requested) && requested > 0 ? requested : profileLimit)
  );

  try {
    const payload = {
      url,
      filename: filename || '',
      referrer: referrer || '',
      cookies: cookies || '',
      userAgent: navigator.userAgent,
      maxSegments,
      isStream: !!options.isStream
    };

    const res = await fetch(`${activeAppBase}/api/download`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify(payload)
    });

    if (!res.ok) {
      const error = await res.json().catch(() => ({}));
      throw new Error(error.error || `HTTP error ${res.status}`);
    }
    const data = await res.json();
    recentSends.set(url, Date.now());

    void focusAppWindow();
    setTimeout(pingApp, 500);
    return data;
  } catch (error) {
    console.error('Failed to send download to desktop app:', error);
    showNotification(
      'Syntax Downloader',
      error.message || 'Could not forward the download to the desktop app.'
    );
    return { error: error.message || 'Could not forward the download to the desktop app.' };
  }
}

async function sendBatchToApp(items, referrer = '', pageUrl = '') {
  const connected = await ensureAppConnected();
  if (!connected) {
    notifyActiveTabAppNotRunning();
    showNotification(
      'Syntax Downloader App Not Running',
      'Launch the Windows desktop app to enable accelerated downloads.'
    );
    return { ok: false, error: 'Syntax Downloader is not running.' };
  }

  const settings = await getSettings();
  const maxSegments = PROFILE_SEGMENTS[settings.speedProfile] || 16;
  const seen = new Set();
  const enriched = [];

  for (const item of (Array.isArray(items) ? items : []).slice(0, 100)) {
    const url = typeof item?.url === 'string' ? item.url : '';
    if (!/^https?:\/\//i.test(url) || seen.has(url)) continue;
    seen.add(url);
    enriched.push({
      url,
      filename: item.filename || filenameFromUrl(url),
      referrer: item.referrer || referrer || '',
      cookies: await getCookiesForUrl(url),
      userAgent: navigator.userAgent,
      isStream: !!item.isStream
    });
  }

  if (enriched.length === 0) {
    return { ok: false, error: 'No downloadable links were found on this page.' };
  }

  try {
    const res = await fetch(`${activeAppBase}/api/batch-download`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ items: enriched, maxSegments, pageUrl })
    });
    const data = await res.json().catch(() => ({}));
    if (!res.ok) throw new Error(data.error || `HTTP error ${res.status}`);

    const failed = Array.isArray(data.failed) ? data.failed.length : 0;
    const queued = Number(data.queued) || 0;
    showNotification(
      'Batch Download',
      failed
        ? `${queued} queued, ${failed} could not be added.`
        : `Sent ${queued} file${queued === 1 ? '' : 's'} to Syntax Downloader.`
    );
    setTimeout(pingApp, 500);
    return {
      ok: failed === 0,
      data,
      error: failed ? `${failed} item(s) could not be added.` : undefined
    };
  } catch (error) {
    showNotification('Batch Download', error.message || 'Could not queue the page files.');
    return { ok: false, error: error.message || 'Could not queue the page files.' };
  }
}

/* ---------- Chrome Downloads Interception ---------- */

function pruneInterceptedIds(now) {
  for (const [id, timestamp] of interceptedIds) {
    if (now - timestamp > INTERCEPT_ID_TTL_MS) interceptedIds.delete(id);
  }
  if (interceptedIds.size > 250) {
    const oldest = [...interceptedIds.entries()]
      .sort((a, b) => a[1] - b[1])
      .slice(0, interceptedIds.size - 100);
    for (const [id] of oldest) interceptedIds.delete(id);
  }
}

chrome.downloads.onDeterminingFilename.addListener((downloadItem, suggest) => {
  const now = Date.now();
  pruneInterceptedIds(now);
  if (interceptedIds.has(downloadItem.id)) {
    suggest();
    return;
  }
  interceptedIds.set(downloadItem.id, now);

  let finished = false;
  const finish = () => {
    if (finished) return;
    finished = true;
    suggest();
  };

  (async () => {
    try {
      const settings = await getSettings();
      const url = downloadItem.finalUrl || downloadItem.url;
      if (!url || url.startsWith('blob:') || url.startsWith('data:')) {
        interceptedIds.delete(downloadItem.id);
        finish();
        return;
      }

      const filename = downloadItem.filename ? basename(downloadItem.filename) : '';
      if (!shouldIntercept(url, filename, downloadItem.mime, settings)) {
        interceptedIds.delete(downloadItem.id);
        finish();
        return;
      }

      const connected = await ensureAppConnected();
      if (!connected) {
        interceptedIds.delete(downloadItem.id);
        notifyActiveTabAppNotRunning();
        showNotification(
          'Syntax Downloader App Not Running',
          'Launch the Windows desktop app to enable accelerated downloads.'
        );
        finish();
        return;
      }

      const queued = await sendToApp(url, filename, downloadItem.referrer || '');
      if (!queued || queued.error) {
        interceptedIds.delete(downloadItem.id);
        finish();
        return;
      }

      finish();
      chrome.downloads.cancel(downloadItem.id, () => {
        void chrome.runtime.lastError;
        chrome.downloads.erase({ id: downloadItem.id }, () => {
          void chrome.runtime.lastError;
        });
      });
    } catch (error) {
      interceptedIds.delete(downloadItem.id);
      console.error('Chrome download interception failed:', error);
      finish();
    }
  })();

  return true;
});

/* ---------- Notifications & Badge ---------- */

function updateBadge() {
  if (!appConnected) {
    chrome.action.setBadgeBackgroundColor({ color: '#ef4444' });
    chrome.action.setBadgeText({ text: '!' });
    return;
  }
  if (activeDownloadCount > 0) {
    chrome.action.setBadgeBackgroundColor({ color: '#00d4ff' });
    chrome.action.setBadgeText({
      text: activeDownloadCount > 999 ? '999+' : String(activeDownloadCount)
    });
  } else {
    chrome.action.setBadgeText({ text: '' });
  }
}

function showNotification(title, message) {
  try {
    chrome.notifications.create({
      type: 'basic',
      iconUrl: chrome.runtime.getURL('icons/icon128.png'),
      title,
      message
    });
  } catch (error) {
    console.error('Could not show notification:', error);
  }
}

function notifyActiveTabAppNotRunning() {
  chrome.tabs.query({ active: true, currentWindow: true }, (tabs) => {
    const tabId = tabs?.[0]?.id;
    if (!tabId) return;
    chrome.tabs.sendMessage(tabId, { type: 'SHOW_APP_NOT_RUNNING_POPUP' }, () => {
      void chrome.runtime.lastError;
    });
  });
}

/* ---------- Context Menus ---------- */

function setupContextMenus() {
  chrome.contextMenus.removeAll(() => {
    chrome.contextMenus.create({
      id: 'sdl-download-link',
      title: 'Download with Syntax Downloader',
      contexts: ['link']
    });
    chrome.contextMenus.create({
      id: 'sdl-download-all-links',
      title: 'Download all links with Syntax Downloader',
      contexts: ['page', 'selection']
    });
    chrome.contextMenus.create({
      id: 'sdl-download-media',
      title: 'Download media with Syntax Downloader',
      contexts: ['video', 'audio', 'image']
    });
  });
}

function ensureHeartbeat() {
  chrome.alarms.create(HEARTBEAT_ALARM, { periodInMinutes: 1 });
}

chrome.runtime.onInstalled.addListener(async () => {
  const data = await chrome.storage.local.get('settings');
  await saveSettings(normalizeSettings(data.settings));
  setupContextMenus();
  ensureHeartbeat();
  void pingApp();
});

chrome.runtime.onStartup.addListener(() => {
  setupContextMenus();
  ensureHeartbeat();
  void pingApp();
});

chrome.alarms.onAlarm.addListener((alarm) => {
  if (alarm.name === HEARTBEAT_ALARM) void pingApp();
});

chrome.contextMenus.onClicked.addListener(async (info, tab) => {
  const settings = await getSettings();
  if (!settings.enabled) return;

  const referrer = tab?.url || '';

  if (info.menuItemId === 'sdl-download-link' && info.linkUrl) {
    const filename = filenameFromUrl(info.linkUrl);
    await sendToApp(info.linkUrl, filename, referrer);
    return;
  }

  if (info.menuItemId === 'sdl-download-media' && info.srcUrl) {
    const filename = filenameFromUrl(info.srcUrl);
    const isStream = !/^https?:\/\//i.test(info.srcUrl);
    await sendToApp(info.srcUrl, filename, referrer, {
      isStream,
      pageUrl: tab?.url || referrer
    });
    return;
  }

  if (info.menuItemId === 'sdl-download-all-links' && tab?.id) {
    chrome.tabs.sendMessage(tab.id, { type: 'SCAN_PAGE_RESOURCES' }, async (res) => {
      if (chrome.runtime.lastError) {
        showNotification(
          'Syntax Downloader',
          'Could not scan this page. Reload the tab and try again.'
        );
        return;
      }
      const items = res?.items || [];
      await sendBatchToApp(items, referrer, tab.url || referrer);
    });
  }
});

/* ---------- Message Handling ---------- */

chrome.runtime.onMessage.addListener((message, sender, sendResponse) => {
  (async () => {
    try {
      switch (message?.type) {
        case 'DOWNLOAD_LINK': {
          const result = await sendToApp(message.url, message.filename, message.referrer, {
            isStream: message.isStream,
            pageUrl: message.pageUrl
          });
          sendResponse({ ok: !!result && !result.error, error: result?.error });
          break;
        }

        case 'QUICK_DOWNLOAD': {
          const result = await sendToApp(
            message.url,
            message.filename || '',
            message.referrer || '',
            {
              isStream: !!message.isStream,
              pageUrl: message.pageUrl || message.url
            }
          );
          sendResponse({
            ok: !!result && !result.error,
            error: result?.error,
            download: result?.download
          });
          break;
        }

        case 'OPEN_APP': {
          const connected = await ensureAppConnected();
          sendResponse({ ok: connected ? await focusAppWindow() : false });
          break;
        }

        case 'GET_STATUS': {
          const status = await pingApp();
          sendResponse({ connected: appConnected, status });
          break;
        }

        case 'GET_DOWNLOADS': {
          try {
            const res = await fetch(`${activeAppBase}/api/downloads`, {
              signal: AbortSignal.timeout(2500)
            });
            const data = await res.json();
            sendResponse({ downloads: data.downloads || [] });
          } catch {
            sendResponse({ downloads: [] });
          }
          break;
        }

        case 'GET_SETTINGS': {
          sendResponse({ settings: await getSettings() });
          break;
        }

        case 'UPDATE_SETTINGS': {
          const current = await getSettings();
          const updated = normalizeSettings({ ...current, ...(message.settings || {}) });
          await saveSettings(updated);

          const tabs = await chrome.tabs.query({});
          await Promise.allSettled(
            tabs
              .filter((tab) => typeof tab.id === 'number')
              .map((tab) =>
                chrome.tabs.sendMessage(tab.id, { type: 'SETTINGS_UPDATED', settings: updated })
              )
          );
          sendResponse({ settings: updated });
          break;
        }

        case 'BATCH_DOWNLOAD': {
          const result = await sendBatchToApp(
            message.items || [],
            message.referrer || sender.tab?.url || '',
            message.pageUrl || sender.tab?.url || ''
          );
          sendResponse(result);
          break;
        }

        case 'PAUSE_DOWNLOAD':
        case 'RESUME_DOWNLOAD':
        case 'CANCEL_DOWNLOAD': {
          const endpoint = message.type.toLowerCase().replace('_download', '');
          const res = await fetch(`${activeAppBase}/api/${endpoint}/${message.id}`, {
            method: 'POST'
          });
          const data = await res.json().catch(() => ({}));
          if (!res.ok) throw new Error(data.error || `HTTP error ${res.status}`);
          void pingApp();
          sendResponse({ ok: true, data });
          break;
        }

        default:
          sendResponse({ error: 'Unknown message type' });
      }
    } catch (error) {
      console.error('Extension message failed:', error);
      sendResponse({ ok: false, error: error.message || String(error) });
    }
  })();

  return true;
});

/* ---------- Startup ---------- */

ensureHeartbeat();
void pingApp();
