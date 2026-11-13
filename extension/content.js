/* ============================================================
   Syntax Downloader — Content Script
   Non-intrusive link/media interception and page scanning
   ============================================================ */

(() => {
  'use strict';

  let settings = {
    enabled: true,
    showFloatingButton: true,
    altClickIntercept: true,
    speedProfile: 'high'
  };

  let initialized = false;
  let videoGrabberEl = null;
  let activeMediaElement = null;
  let grabberCleanup = null;
  let grabberTimer = null;

  const DOWNLOADABLE_EXTS = [
    '.zip', '.exe', '.msi', '.dmg', '.iso', '.rar', '.7z', '.tar', '.gz', '.bz2', '.xz',
    '.mp4', '.mkv', '.avi', '.mp3', '.flac', '.webm', '.mov', '.m4a', '.wav', '.m4v',
    '.pdf', '.doc', '.docx', '.xls', '.xlsx', '.ppt', '.pptx', '.apk', '.bin', '.deb', '.rpm'
  ];

  /* ---------- Settings Sync ---------- */

  chrome.runtime.sendMessage({ type: 'GET_SETTINGS' }, (res) => {
    void chrome.runtime.lastError;
    if (res?.settings) settings = { ...settings, ...res.settings };
    syncState();
  });

  chrome.runtime.onMessage.addListener((msg, sender, sendResponse) => {
    if (msg?.type === 'SHOW_APP_NOT_RUNNING_POPUP') {
      showAppNotRunningPopup();
      sendResponse({ ok: true });
      return true;
    }

    if (msg?.type === 'SETTINGS_UPDATED') {
      settings = { ...settings, ...(msg.settings || {}) };
      syncState();
    }

    if (msg?.type === 'SCAN_PAGE_RESOURCES') {
      sendResponse({ items: scanPageResources(), pageTitle: document.title, url: location.href });
      return true;
    }

    return false;
  });

  function syncState() {
    if (!settings.enabled || !settings.showFloatingButton) {
      removeVideoGrabber();
    }
    if (settings.enabled && !initialized) {
      init();
    }
  }

  /* ---------- Initialization ---------- */

  function init() {
    if (initialized) return;
    initialized = true;

    attachVideoGrabberListeners();
    attachAltClickListener();

    // SPA navigation (YouTube, Twitter, TikTok, etc.)
    window.addEventListener('yt-navigate-start', removeVideoGrabber, true);
    window.addEventListener('yt-navigate-finish', removeVideoGrabber, true);
    window.addEventListener('yt-page-data-updated', removeVideoGrabber, true);
    window.addEventListener('popstate', removeVideoGrabber, true);

    window.addEventListener('scroll', repositionGrabber, { passive: true });
    window.addEventListener('resize', repositionGrabber, { passive: true });
  }

  /* ---------- Alt + Click Intercept ---------- */

  function attachAltClickListener() {
    document.addEventListener(
      'click',
      (event) => {
        if (!settings.enabled || !settings.altClickIntercept || !event.altKey) return;
        const link = closestElement(event.target, 'a[href]');
        if (!link) return;

        const href = link.href;
        if (!/^https?:\/\//i.test(href)) return;

        event.preventDefault();
        event.stopPropagation();
        const filename = link.download || filenameFromUrl(href);
        sendDownload(href, filename);
      },
      true
    );
  }

  function closestElement(target, selector) {
    const element = target instanceof Element ? target : target?.parentElement;
    return element ? element.closest(selector) : null;
  }

  /* ---------- Video & Audio Grabber ---------- */

  function isOpenedVideo(mediaEl) {
    if (!mediaEl) return false;
    const host = location.hostname.toLowerCase();

    if (host.includes('youtube.com') || host.includes('youtu.be')) {
      const path = location.pathname.toLowerCase();
      const isWatchPage =
        path.startsWith('/watch') || path.startsWith('/shorts') || path.startsWith('/live');
      if (!isWatchPage) return false;

      if (
        mediaEl.closest(
          'ytd-thumbnail, #inline-preview-player, ytd-video-preview, ytd-compact-video-renderer, #thumbnail, .ytd-thumbnail'
        )
      ) {
        return false;
      }

      const isMainPlayer =
        mediaEl.classList.contains('html5-main-video') ||
        mediaEl.closest('#movie_player, #ytd-player, .html5-video-player, ytd-watch-flexy, ytd-shorts');
      if (!isMainPlayer) return false;
    }

    const isAudio = mediaEl.tagName === 'AUDIO';
    if (!isAudio) {
      const rect = mediaEl.getBoundingClientRect();
      if (rect.width < 280 || rect.height < 160) return false;
      if (
        mediaEl.closest(
          '.inline-preview, [data-preview], .thumbnail-preview, [aria-label*="preview" i], .feed-video-preview'
        )
      ) {
        return false;
      }
    }

    return true;
  }

  function attachVideoGrabberListeners() {
    document.addEventListener(
      'mouseover',
      (event) => {
        if (!settings.enabled || !settings.showFloatingButton) return;
        const mediaEl = closestElement(event.target, 'video, audio');
        if (mediaEl && isOpenedVideo(mediaEl)) {
          activeMediaElement = mediaEl;
          showVideoGrabber(mediaEl);
        }
      },
      true
    );

    document.addEventListener(
      'play',
      (event) => {
        if (!settings.enabled || !settings.showFloatingButton) return;
        const target = event.target;
        if (target && (target.tagName === 'VIDEO' || target.tagName === 'AUDIO') && isOpenedVideo(target)) {
          activeMediaElement = target;
          showVideoGrabber(target);
        }
      },
      true
    );

    document.addEventListener(
      'loadeddata',
      (event) => {
        if (!settings.enabled || !settings.showFloatingButton) return;
        const target = event.target;
        if (
          target &&
          (target.tagName === 'VIDEO' || target.tagName === 'AUDIO') &&
          !target.paused &&
          isOpenedVideo(target)
        ) {
          activeMediaElement = target;
          showVideoGrabber(target);
        }
      },
      true
    );
  }

  function resolveMediaSource(el) {
    const host = location.hostname.toLowerCase();

    if (host.includes('youtube.com') || host.includes('youtu.be')) {
      return location.href;
    }

    if (el.currentSrc && !el.currentSrc.startsWith('blob:') && !el.currentSrc.startsWith('data:')) {
      return el.currentSrc;
    }
    if (el.src && !el.src.startsWith('blob:') && !el.src.startsWith('data:')) {
      return el.src;
    }

    const sourceEl = el.querySelector('source[src]');
    if (
      sourceEl &&
      sourceEl.src &&
      !sourceEl.src.startsWith('blob:') &&
      !sourceEl.src.startsWith('data:')
    ) {
      return sourceEl.src;
    }

    const streamingPlatforms = [
      'vimeo.com', 'tiktok.com', 'twitter.com', 'x.com', 'instagram.com',
      'facebook.com', 'fb.watch', 'reddit.com', 'twitch.tv', 'dailymotion.com',
      'rumble.com', 'soundcloud.com', 'bilibili.com'
    ];
    if (streamingPlatforms.some((domain) => host.includes(domain))) {
      return location.href;
    }

    return location.href;
  }

  function showVideoGrabber(mediaEl) {
    if (!isOpenedVideo(mediaEl)) return;
    const src = resolveMediaSource(mediaEl);
    if (!src) return;

    const rect = mediaEl.getBoundingClientRect();
    const isAudio = mediaEl.tagName === 'AUDIO';
    if (!isAudio && (rect.width < 160 || rect.height < 90)) return;

    if (videoGrabberEl && videoGrabberEl.dataset.targetVideo === src) {
      updateGrabberPosition(mediaEl);
      return;
    }

    removeVideoGrabber();

    const label = isAudio ? 'Download Audio' : 'Download Video';
    const icon = isAudio ? '🎵' : '🎬';

    videoGrabberEl = document.createElement('div');
    videoGrabberEl.className = 'sdl-video-grabber';
    videoGrabberEl.dataset.targetVideo = src;
    videoGrabberEl.innerHTML = `
      <div class="sdl-video-grabber-content">
        <span class="sdl-video-grabber-icon">${icon}</span>
        <span class="sdl-video-grabber-text">${label}</span>
      </div>
    `;

    updateGrabberPosition(mediaEl);

    videoGrabberEl.addEventListener('click', (event) => {
      event.preventDefault();
      event.stopPropagation();

      let title = (document.title || 'media').replace(/[/\\?%*:|"<>]/g, '_').trim();
      title = title.replace(/\s*-\s*YouTube$/i, '').trim();
      const extension = isAudio ? '.mp3' : '.mp4';
      const cleanFilename = title.endsWith(extension) ? title : title + extension;

      const host = location.hostname.toLowerCase();
      const isStream =
        host.includes('youtube.com') || host.includes('youtu.be') || src === location.href;

      sendDownload(src, cleanFilename, { isStream, pageUrl: location.href });
      removeVideoGrabber();
    });

    const container = document.fullscreenElement || document.body || document.documentElement;
    container.appendChild(videoGrabberEl);

    const onMediaLeave = (event) => {
      if (
        videoGrabberEl &&
        !mediaEl.contains(event.relatedTarget) &&
        !videoGrabberEl.contains(event.relatedTarget)
      ) {
        scheduleGrabberRemoval(900);
      }
    };
    mediaEl.addEventListener('mouseleave', onMediaLeave);

    const onGrabberLeave = () => {
      if (!mediaEl.matches(':hover')) scheduleGrabberRemoval(700);
    };
    videoGrabberEl.addEventListener('mouseleave', onGrabberLeave);

    grabberCleanup = () => {
      mediaEl.removeEventListener('mouseleave', onMediaLeave);
    };
  }

  function scheduleGrabberRemoval(delay) {
    clearTimeout(grabberTimer);
    grabberTimer = setTimeout(() => {
      if (videoGrabberEl && (!activeMediaElement || !activeMediaElement.matches(':hover'))) {
        removeVideoGrabber();
      }
    }, delay);
  }

  function updateGrabberPosition(mediaEl) {
    if (!videoGrabberEl || !mediaEl) return;
    const rect = mediaEl.getBoundingClientRect();
    const scrollX = window.scrollX || window.pageXOffset || 0;
    const scrollY = window.scrollY || window.pageYOffset || 0;

    const top = Math.max(10, scrollY + rect.top + 10);
    const left = Math.max(10, scrollX + rect.right - 180);

    videoGrabberEl.style.top = `${top}px`;
    videoGrabberEl.style.left = `${left}px`;
  }

  function repositionGrabber() {
    if (videoGrabberEl && activeMediaElement) {
      updateGrabberPosition(activeMediaElement);
    }
  }

  function removeVideoGrabber() {
    clearTimeout(grabberTimer);
    grabberTimer = null;
    if (grabberCleanup) {
      grabberCleanup();
      grabberCleanup = null;
    }
    if (videoGrabberEl) {
      videoGrabberEl.remove();
      videoGrabberEl = null;
    }
    activeMediaElement = null;
  }

  /* ---------- Page Resource Scanner ---------- */

  function filenameFromUrl(url) {
    try {
      const parsed = new URL(url, location.href);
      const raw = parsed.pathname.split('/').filter(Boolean).pop() || '';
      return safeDecode(raw) || 'download';
    } catch {
      return 'download';
    }
  }

  function safeDecode(value) {
    try {
      return decodeURIComponent(value);
    } catch {
      return value;
    }
  }

  function isHttpUrl(value) {
    try {
      return ['http:', 'https:'].includes(new URL(value, location.href).protocol);
    } catch {
      return false;
    }
  }

  function isDownloadableUrl(url) {
    try {
      const path = new URL(url, location.href).pathname.toLowerCase();
      const last = path.split('/').filter(Boolean).pop() || '';
      return DOWNLOADABLE_EXTS.some((extension) => last.endsWith(extension));
    } catch {
      return false;
    }
  }

  function scanPageResources() {
    const items = [];
    const seen = new Set();

    const add = (url, filename, type, isStream = false) => {
      if (!url || !isHttpUrl(url)) return;
      const absolute = new URL(url, location.href).href;
      if (seen.has(absolute) || absolute === location.href) return;
      seen.add(absolute);
      items.push({
        url: absolute,
        filename: filename || filenameFromUrl(absolute),
        type,
        isStream
      });
    };

    document.querySelectorAll('video, audio, video source, audio source').forEach((element) => {
      if (items.length >= 200) return;
      const isAudio = element.tagName.toLowerCase() === 'audio';
      const source = element.currentSrc || element.src || element.getAttribute('src');
      if (!source || source.startsWith('blob:') || source.startsWith('data:')) return;
      add(
        source,
        filenameFromUrl(source),
        isAudio ? 'Audio' : 'Video'
      );
    });

    document.querySelectorAll('a[href]').forEach((link) => {
      if (items.length >= 200) return;
      const href = link.href;
      const hasDownloadAttribute = link.hasAttribute('download');
      if (!hasDownloadAttribute && !isDownloadableUrl(href)) return;
      const name = link.download || link.getAttribute('download') || filenameFromUrl(href);
      add(href, name, hasDownloadAttribute ? 'File' : 'Archive / File');
    });

    return items;
  }

  /* ---------- App Offline Notification Popup ---------- */

  function showAppNotRunningPopup(message) {
    const existing = document.getElementById('sdlAppOfflinePopup');
    if (existing) existing.remove();

    const popup = document.createElement('div');
    popup.className = 'sdl-app-offline-popup';
    popup.id = 'sdlAppOfflinePopup';
    popup.innerHTML = `
      <div class="sdl-app-offline-icon">
        <svg width="22" height="22" viewBox="0 0 24 24" fill="none">
          <path d="M12 9v4m0 4h.01M21 12a9 9 0 11-18 0 9 9 0 0118 0z" stroke="#ef4444" stroke-width="2" stroke-linecap="round"/>
        </svg>
      </div>
      <div class="sdl-app-offline-body">
        <p class="sdl-app-offline-title">Syntax Downloader App Not Running</p>
        <p class="sdl-app-offline-subtitle">Launch the Windows desktop app to enable 32x acceleration</p>
      </div>
      <button class="sdl-app-offline-close" title="Close">&times;</button>
    `;
    if (message) {
      popup.querySelector('.sdl-app-offline-subtitle').textContent = message;
    }

    popup.querySelector('.sdl-app-offline-close').onclick = (event) => {
      event.stopPropagation();
      popup.classList.add('closing');
      setTimeout(() => popup.remove(), 250);
    };

    (document.body || document.documentElement).appendChild(popup);

    setTimeout(() => {
      if (popup.parentElement) {
        popup.classList.add('closing');
        setTimeout(() => popup.remove(), 250);
      }
    }, 6000);
  }

  /* ---------- Send to Background Service Worker ---------- */

  function sendDownload(url, filename, options = {}) {
    chrome.runtime.sendMessage(
      {
        type: 'DOWNLOAD_LINK',
        url,
        filename: filename || filenameFromUrl(url),
        referrer: location.href,
        isStream: !!options.isStream,
        pageUrl: options.pageUrl || location.href
      },
      (res) => {
        const error = chrome.runtime.lastError;
        if (error) {
          showAppNotRunningPopup('The extension needs to be reloaded. Open chrome://extensions and reload it.');
          return;
        }
        if (!res || !res.ok) {
          showAppNotRunningPopup(res?.error || 'Could not add this download to Syntax Downloader.');
        }
      }
    );
  }
})();
