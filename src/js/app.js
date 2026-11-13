/**
 * Syntax Downloader — Main Application Logic
 * Coordinates UI, Tauri API, real-time speed graph, notifications, and context menus.
 */

import { NotificationManager } from './components/notification.js';
import { ContextMenu } from './components/context-menu.js';
import { appAPI } from './tauri-api.js';

window.appAPI = appAPI;

class SyntaxDownloaderApp {
  constructor() {
    this.downloads = new Map();
    this.currentView = 'all';
    this.currentSort = 'dateAdded';
    this.searchQuery = '';
    this.settings = {};
    this.theme = 'dark';
    this.selectedDownloadId = null;
    this.currentSpeed = 0;
    this.backendReady = false;
    this.filenameAuto = true;

    // Components
    this.notifications = null;
    this.contextMenu = null;

    // DOM Elements Cache
    this.dom = {};
  }

  /* ─────────────────────────────────────────────────────────────
     INIT & BOOTSTRAP
     ───────────────────────────────────────────────────────────── */
  async init() {
    this._cacheDom();
    this._initTheme();
    this._initComponents();
    this._bindEvents();
    this._setupTauriListeners();

    // Load initial settings and downloads
    await this._loadSettings();
    await this._loadDownloads();

    this._updateSidebarBadges();
    this._updateStatusBar();

    // Check for updates on startup if enabled (delayed so it never interferes with UI loading)
    if (this.settings?.autoCheckUpdates !== false) {
      setTimeout(() => {
        this._handleCheckUpdates(false).catch(() => {});
      }, 2000);

      // Periodically check for updates every 30 minutes while running
      setInterval(() => {
        if (this.settings?.autoCheckUpdates !== false) {
          this._handleCheckUpdates(false).catch(() => {});
        }
      }, 30 * 60 * 1000);
    }
  }

  _cacheDom() {
    this.dom = {
      app: document.getElementById('app'),
      themeOverlay: document.getElementById('themeOverlay'),
      themeIcon: document.getElementById('themeIcon'),
      sidebar: document.getElementById('sidebar'),
      sidebarToggle: document.getElementById('sidebarToggle'),
      sidebarItems: document.querySelectorAll('.sidebar-item'),

      // Toolbar
      tbBtnAdd: document.getElementById('tbBtnAdd'),
      tbBtnResumeAll: document.getElementById('tbBtnResumeAll'),
      tbBtnPauseAll: document.getElementById('tbBtnPauseAll'),
      tbBtnCancelAll: document.getElementById('tbBtnCancelAll'),
      tbBtnOpenFolder: document.getElementById('tbBtnOpenFolder'),
      tbBtnExtension: document.getElementById('tbBtnExtension'),
      sidebarBtnExtension: document.getElementById('sidebarBtnExtension'),
      tbBtnTheme: document.getElementById('tbBtnTheme'),
      tbBtnSettings: document.getElementById('tbBtnSettings'),
      searchInput: document.getElementById('searchInput'),

      // View & List
      viewTitle: document.getElementById('viewTitle'),
      sortButtons: document.querySelectorAll('.sort-btn'),
      downloadList: document.getElementById('downloadList'),
      emptyState: document.getElementById('downloadEmptyState'),
      fabAdd: document.getElementById('fabAdd'),

      // Status Bar
      statusCurrentSpeed: document.getElementById('statusCurrentSpeed'),
      statusActiveCount: document.getElementById('statusActiveCount'),
      statusTodayDownloaded: document.getElementById('statusTodayDownloaded'),
      statusConnectionText: document.getElementById('statusConnectionText'),
      statusDotIndicator: document.getElementById('statusDotIndicator'),

      // Modals
      modalAdd: document.getElementById('modalAdd'),
      modalExtension: document.getElementById('modalExtension'),
      modalSettings: document.getElementById('modalSettings'),
      btnDownloadExtZip: document.getElementById('btnDownloadExtZip'),
      btnOpenExtFolder: document.getElementById('btnOpenExtFolder'),

      // Add Modal Inputs
      inputUrl: document.getElementById('inputUrl'),
      inputFilename: document.getElementById('inputFilename'),
      inputSaveDir: document.getElementById('inputSaveDir'),
      inputSegments: document.getElementById('inputSegments'),
      valSegments: document.getElementById('valSegments'),
      inputSha256: document.getElementById('inputSha256'),
      chkStartImmediate: document.getElementById('chkStartImmediate'),
      chkSchedule: document.getElementById('chkSchedule'),
      scheduleTimeWrap: document.getElementById('scheduleTimeWrap'),
      inputScheduleTime: document.getElementById('inputScheduleTime'),
      btnBrowseDir: document.getElementById('btnBrowseDir'),
      btnPasteUrl: document.getElementById('btnPasteUrl'),
      btnConfirmAdd: document.getElementById('btnConfirmAdd'),

      // Settings Modal Inputs
      setDownloadDir: document.getElementById('setDownloadDir'),
      btnBrowseSettingsDir: document.getElementById('btnBrowseSettingsDir'),
      lblDefaultDir: document.getElementById('lblDefaultDir'),
      setMaxConcurrent: document.getElementById('setMaxConcurrent'),
      setDefaultSegments: document.getElementById('setDefaultSegments'),
      valDefaultSegments: document.getElementById('valDefaultSegments'),
      setSpeedLimit: document.getElementById('setSpeedLimit'),
      setNotifications: document.getElementById('setNotifications'),
      setMinToTray: document.getElementById('setMinToTray'),
      setCloseToTray: document.getElementById('setCloseToTray'),
      setStartWithWindows: document.getElementById('setStartWithWindows'),
      setShutdown: document.getElementById('setShutdown'),
      setUpdateFeedUrl: document.getElementById('setUpdateFeedUrl'),
      setAutoCheckUpdates: document.getElementById('setAutoCheckUpdates'),
      btnCheckUpdates: document.getElementById('btnCheckUpdates'),
      updateStatusText: document.getElementById('updateStatusText'),
      modalUpdateAvailable: document.getElementById('modalUpdateAvailable'),
      updateModalTitle: document.getElementById('updateModalTitle'),
      updateModalHeading: document.getElementById('updateModalHeading'),
      updateModalVersions: document.getElementById('updateModalVersions'),
      updateModalChangelog: document.getElementById('updateModalChangelog'),
      btnDownloadUpdate: document.getElementById('btnDownloadUpdate'),
      btnExportHistory: document.getElementById('btnExportHistory'),
      btnImportHistory: document.getElementById('btnImportHistory'),
      btnSaveSettings: document.getElementById('btnSaveSettings'),

      // Badges
      badgeAll: document.getElementById('badgeAll'),
      badgeDownloading: document.getElementById('badgeDownloading'),
      badgeCompleted: document.getElementById('badgeCompleted'),
      badgeScheduled: document.getElementById('badgeScheduled'),
      badgeVideos: document.getElementById('badgeVideos'),
      badgeMusic: document.getElementById('badgeMusic'),
      badgeDocuments: document.getElementById('badgeDocuments'),
      badgePrograms: document.getElementById('badgePrograms'),
      badgeCompressed: document.getElementById('badgeCompressed'),
      badgeImages: document.getElementById('badgeImages'),
      badgeOther: document.getElementById('badgeOther')
    };
  }

  _initComponents() {
    this.notifications = new NotificationManager(document.body);
    this.contextMenu = new ContextMenu(document.body);
  }

  /* ─────────────────────────────────────────────────────────────
     THEME MANAGEMENT
     ───────────────────────────────────────────────────────────── */
  _initTheme() {
    const saved = localStorage.getItem('sdl-theme') || 'dark';
    this._setTheme(saved, false);
  }

  _setTheme(theme, animate = true) {
    this.theme = theme;
    localStorage.setItem('sdl-theme', theme);

    if (animate && this.dom.themeOverlay) {
      this.dom.themeOverlay.classList.add('active');
      setTimeout(() => {
        document.documentElement.setAttribute('data-theme', theme);
        this.dom.themeOverlay.classList.remove('active');
      }, 200);
    } else {
      document.documentElement.setAttribute('data-theme', theme);
    }

    if (this.dom.themeIcon) {
      if (theme === 'light') {
        this.dom.themeIcon.innerHTML = `
          <path d="M21 12.79A9 9 0 1111.21 3 7 7 0 0021 12.79z" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"/>
        `;
      } else {
        this.dom.themeIcon.innerHTML = `
          <path d="M12 3v1m0 16v1m9-9h-1M4 12H3m15.364 6.364l-.707-.707M6.343 6.343l-.707-.707m12.728 0l-.707.707M6.343 17.657l-.707.707M16 12a4 4 0 11-8 0 4 4 0 018 0z" stroke="currentColor" stroke-width="2" stroke-linecap="round"/>
        `;
      }
    }
  }

  _toggleTheme() {
    const theme = this.theme === 'dark' ? 'light' : 'dark';
    this._setTheme(theme, true);
    this.settings = { ...this.settings, theme };
    window.appAPI?.updateSettings({ theme }).catch((error) => {
      console.warn('Could not save the theme setting:', error);
    });
  }

  /* ─────────────────────────────────────────────────────────────
     EVENT BINDINGS
     ───────────────────────────────────────────────────────────── */
  _bindEvents() {
    // Sidebar Toggle
    this.dom.sidebarToggle?.addEventListener('click', () => {
      this.dom.app.classList.toggle('sidebar-collapsed');
    });

    // Sidebar Navigation
    this.dom.sidebarItems.forEach(btn => {
      btn.addEventListener('click', () => {
        this.dom.sidebarItems.forEach(b => b.classList.remove('active'));
        btn.classList.add('active');
        this.currentView = btn.dataset.view;
        this._updateViewTitle();
        this.renderDownloads();
      });
    });

    // Toolbar Add Download
    this.dom.tbBtnAdd?.addEventListener('click', () => this.showAddModal());
    this.dom.fabAdd?.addEventListener('click', () => this.showAddModal());

    // Toolbar Batch Operations
    this.dom.tbBtnResumeAll?.addEventListener('click', () => {
      this._runBulkAction('resumeAll', 'Resuming all downloads');
    });

    this.dom.tbBtnPauseAll?.addEventListener('click', () => {
      this._runBulkAction('pauseAll', 'Paused all downloads');
    });

    this.dom.tbBtnCancelAll?.addEventListener('click', () => {
      if (confirm('Cancel and stop all active downloads?')) {
        this._runBulkAction('cancelAll', 'Cancelled all downloads');
      }
    });

    // Toolbar Open Downloads Folder
    this.dom.tbBtnOpenFolder?.addEventListener('click', () => {
      const defaultDir = this.settings?.downloadDir || '';
      if (!defaultDir) {
        this.notifications.show('No download folder is configured yet', 'warning');
        return;
      }
      window.appAPI?.openFolder(defaultDir).catch((error) => {
        this.notifications.show(`Could not open folder: ${error.message || error}`, 'error');
      });
    });

    // Extension Modal
    this.dom.tbBtnExtension?.addEventListener('click', () => this.showExtensionModal());
    this.dom.sidebarBtnExtension?.addEventListener('click', () => this.showExtensionModal());

    // Theme Toggle
    this.dom.tbBtnTheme?.addEventListener('click', () => this._toggleTheme());

    // Settings
    this.dom.tbBtnSettings?.addEventListener('click', () => this.showSettingsModal());
    this.dom.btnSaveSettings?.addEventListener('click', () => this._saveSettings());
    this.dom.btnBrowseSettingsDir?.addEventListener('click', async () => {
      const path = await this._pickDirectory('Choose Default Download Folder');
      if (path) this.dom.setDownloadDir.value = path;
    });
    this.dom.setDefaultSegments?.addEventListener('input', (e) => {
      this.dom.valDefaultSegments.textContent = e.target.value;
    });
    this.dom.btnCheckUpdates?.addEventListener('click', () => this._handleCheckUpdates(true));
    this.dom.btnDownloadUpdate?.addEventListener('click', () => this._handleDownloadUpdate());
    document.querySelectorAll('.app-version-badge').forEach(badge => {
      badge.style.cursor = 'pointer';
      badge.setAttribute('title', 'Click to check for updates');
      badge.addEventListener('click', () => this._handleCheckUpdates(true));
    });

    this.dom.btnExportHistory?.addEventListener('click', async () => {
      try {
        const res = await window.appAPI?.showSaveFileDialog({
          title: 'Export Download History',
          defaultPath: 'syntax-downloads.json',
          filters: [{ name: 'JSON', extensions: ['json'] }]
        });
        if (!res || res.canceled || !res.filePath) return;
        const result = await window.appAPI?.exportDownloads(res.filePath);
        this.notifications.show(`Exported ${result?.count ?? 0} download(s)`, 'success');
      } catch (error) {
        this.notifications.show(`Could not export history: ${error.message || error}`, 'error');
      }
    });

    this.dom.btnImportHistory?.addEventListener('click', async () => {
      try {
        const res = await window.appAPI?.showOpenDialog({
          title: 'Import Download History',
          filters: [{ name: 'JSON', extensions: ['json'] }]
        });
        if (!res || res.canceled || !res.filePaths?.[0]) return;
        const result = await window.appAPI?.importDownloads(res.filePaths[0]);
        await this._loadDownloads();
        this.notifications.show(`Imported ${result?.count ?? 0} download(s)`, 'success');
      } catch (error) {
        this.notifications.show(`Could not import history: ${error.message || error}`, 'error');
      }
    });

    // Search input
    this.dom.searchInput?.addEventListener('input', (e) => {
      this.searchQuery = e.target.value.toLowerCase().trim();
      this.renderDownloads();
    });

    // Sort buttons
    this.dom.sortButtons.forEach(btn => {
      btn.addEventListener('click', () => {
        this.dom.sortButtons.forEach(b => b.classList.remove('active'));
        btn.classList.add('active');
        this.currentSort = btn.dataset.sort;
        this.renderDownloads();
      });
    });

    // Close Modals by [data-close-modal]
    document.querySelectorAll('[data-close-modal]').forEach(btn => {
      btn.addEventListener('click', () => {
        const modalId = btn.getAttribute('data-close-modal');
        this._closeModal(modalId);
      });
    });

    // Close modal on clicking overlay backdrop
    document.querySelectorAll('.modal-overlay').forEach(overlay => {
      overlay.addEventListener('click', (e) => {
        if (e.target === overlay) {
          overlay.classList.remove('open');
        }
      });
    });

    // Add Modal Form interactions
    this.dom.inputSegments?.addEventListener('input', (e) => {
      if (this.dom.valSegments) this.dom.valSegments.textContent = e.target.value;
    });

    this.dom.inputFilename?.addEventListener('input', () => {
      this.filenameAuto = false;
    });

    this.dom.chkSchedule?.addEventListener('change', (e) => {
      this.dom.scheduleTimeWrap.style.display = e.target.checked ? 'block' : 'none';
      if (e.target.checked && !this.dom.inputScheduleTime.value) {
        // default 1 hour from now
        const d = new Date(Date.now() + 3600000);
        d.setMinutes(d.getMinutes() - d.getTimezoneOffset());
        this.dom.inputScheduleTime.value = d.toISOString().slice(0, 16);
      }
    });

    this.dom.btnPasteUrl?.addEventListener('click', async () => {
      try {
        const text = await navigator.clipboard.readText();
        if (text) {
          this.dom.inputUrl.value = text.trim();
          this._handleUrlInput();
        }
      } catch {
        this.dom.inputUrl.focus();
      }
    });

    this.dom.inputUrl?.addEventListener('input', () => this._handleUrlInput());

    this.dom.btnBrowseDir?.addEventListener('click', async () => {
      const path = await this._pickDirectory('Choose Save Folder');
      if (path) this.dom.inputSaveDir.value = path;
    });

    this.dom.btnConfirmAdd?.addEventListener('click', () => this._submitAddDownload());

    // Extension Modal Action Handlers
    this.dom.btnDownloadExtZip?.addEventListener('click', async () => {
      const res = await window.appAPI?.downloadExtension();
      if (res && res.success) {
        this.notifications.show('Extension package saved to Downloads folder', 'success');
      } else {
        this.notifications.show(res?.error || 'Failed to download extension package', 'error');
      }
    });

    this.dom.btnOpenExtFolder?.addEventListener('click', async () => {
      await window.appAPI?.openExtensionFolder();
    });

    // Keyboard Shortcuts
    window.addEventListener('keydown', (e) => {
      if ((e.ctrlKey || e.metaKey) && e.key.toLowerCase() === 'n') {
        e.preventDefault();
        this.showAddModal();
      }
      if (e.key === 'Escape') {
        document.querySelectorAll('.modal-overlay.open').forEach(m => m.classList.remove('open'));
      }
    });

    // Global drag & drop handling for URLs
    window.addEventListener('dragover', (e) => e.preventDefault());
    window.addEventListener('drop', (e) => {
      e.preventDefault();
      const text = e.dataTransfer.getData('text/uri-list') || e.dataTransfer.getData('text/plain');
      if (text && (text.startsWith('http://') || text.startsWith('https://'))) {
        this.showAddModal(text.trim());
      }
    });
  }

  /* ─────────────────────────────────────────────────────────────
     ELECTRON IPC LISTENERS
     ───────────────────────────────────────────────────────────── */
  _setupTauriListeners() {
    if (!window.appAPI) return;

    window.appAPI.onDownloadProgress((data) => {
      const dl = this.downloads.get(data.id);
      if (!dl) return;
      const statusChanged = data.status && data.status !== dl.status;
      dl.progress = data.progress ?? dl.progress;
      dl.speed = data.speed ?? dl.speed;
      dl.filename = data.filename ?? dl.filename;
      dl.category = data.category ?? dl.category;
      dl.downloadedSize = data.downloadedSize ?? data.downloaded ?? dl.downloadedSize;
      dl.totalSize = data.totalSize ?? data.total ?? dl.totalSize;
      dl.stage = data.stage ?? dl.stage;
      dl.segments = data.segments || dl.segments;
      dl.status = data.status || dl.status;
      this._updateDownloadCard(dl);
      if (statusChanged) {
        this._updateSidebarBadges();
        this._updateStatusBar();
      }
    });

    window.appAPI.onDownloadComplete((dl) => {
      if (!dl?.id) return;
      this.downloads.set(dl.id, dl);
      this._updateDownloadCard(dl);
      this._updateSidebarBadges();
      this._updateStatusBar();
      this.notifications.show(`Downloaded: ${dl.filename || 'File'}`, 'success');
    });

    window.appAPI.onDownloadError((data) => {
      const dl = this.downloads.get(data.id);
      if (!dl) return; // Download was deleted or not in list, ignore any late error
      dl.status = 'error';
      dl.stage = 'Error';
      dl.error = data.error;
      dl.speed = 0;
      this._updateDownloadCard(dl);
      this._updateSidebarBadges();
      this._updateStatusBar();
      this.notifications.show(`Error downloading ${dl.filename || 'file'}: ${data.error}`, 'error');
    });

    window.appAPI.onDownloadPaused?.((data) => {
      const dl = this.downloads.get(data.id);
      if (!dl) return;
      Object.assign(dl, data);
      dl.status = 'paused';
      dl.speed = 0;
      this._updateDownloadCard(dl);
      this._updateSidebarBadges();
      this._updateStatusBar();
    });

    window.appAPI.onDownloadResumed?.((data) => {
      const dl = this.downloads.get(data.id);
      if (!dl) return;
      dl.status = data.status || 'downloading';
      dl.stage = data.status === 'downloading' ? 'Connecting' : 'Queued';
      dl.speed = 0;
      this._updateDownloadCard(dl);
      this._updateSidebarBadges();
      this._updateStatusBar();
    });

    window.appAPI.onDownloadCancelled?.((data) => {
      const dl = this.downloads.get(data.id);
      if (!dl) return;
      Object.assign(dl, data);
      dl.status = 'cancelled';
      dl.speed = 0;
      this._updateDownloadCard(dl);
      this._updateSidebarBadges();
      this._updateStatusBar();
    });

    const notifiedDownloads = new Set();
    window.appAPI.onNewDownload((dl) => {
      if (!dl || !dl.id) return;
      this.downloads.set(dl.id, dl);
      this.renderDownloads();
      this._updateSidebarBadges();
      this._updateStatusBar();

      if (!notifiedDownloads.has(dl.id) && dl.filename) {
        notifiedDownloads.add(dl.id);
        this.notifications.show(`Downloading: ${dl.filename}`, 'info');
      }
    });

    window.appAPI.onSpeedUpdate((speeds) => {
      this.currentSpeed = speeds.downloadSpeed || 0;
      this._updateStatusBar();
    });
  }

  /* ─────────────────────────────────────────────────────────────
     DATA LOADING
     ───────────────────────────────────────────────────────────── */
  async _loadSettings() {
    if (!window.appAPI) return;
    try {
      this.settings = await window.appAPI.getSettings() || {};
      this.backendReady = true;
      this._applySettingsToUi();
    } catch (err) {
      this.backendReady = false;
      console.warn('Error loading settings:', err);
    }
  }

  _applySettingsToUi() {
    const settings = this.settings || {};
    const downloadDir = settings.downloadDir || '';
    const maxSegments = Math.max(1, Math.min(32, Number(settings.maxSegments) || 8));

    if (this.dom.inputSaveDir && !this.dom.inputSaveDir.value) {
      this.dom.inputSaveDir.value = downloadDir;
    }
    if (this.dom.inputSegments) {
      this.dom.inputSegments.value = maxSegments;
      if (this.dom.valSegments) this.dom.valSegments.textContent = maxSegments;
    }
    if (this.dom.setDownloadDir) this.dom.setDownloadDir.value = downloadDir;
    if (this.dom.lblDefaultDir) this.dom.lblDefaultDir.textContent = downloadDir;
    if (this.dom.setMaxConcurrent) {
      this.dom.setMaxConcurrent.value = Math.max(1, Math.min(10, Number(settings.maxConcurrent) || 3));
    }
    if (this.dom.setDefaultSegments) {
      this.dom.setDefaultSegments.value = maxSegments;
      if (this.dom.valDefaultSegments) this.dom.valDefaultSegments.textContent = maxSegments;
    }
    if (this.dom.setSpeedLimit) {
      const limitKb = Math.round((Number(settings.speedLimit) || 0) / 1024);
      this.dom.setSpeedLimit.value = limitKb;
    }
    if (this.dom.setNotifications) this.dom.setNotifications.checked = settings.notifications !== false;
    if (this.notifications) this.notifications.enabled = settings.notifications !== false;
    if (this.dom.setMinToTray) this.dom.setMinToTray.checked = settings.startMinimized === true;
    if (this.dom.setCloseToTray) this.dom.setCloseToTray.checked = settings.closeToTray === true;
    if (this.dom.setStartWithWindows) this.dom.setStartWithWindows.checked = settings.startWithWindows === true;
    if (this.dom.setShutdown) this.dom.setShutdown.checked = settings.shutdownAfterComplete === true;
    if (this.dom.setUpdateFeedUrl) {
      this.dom.setUpdateFeedUrl.value = settings.updateFeedUrl || 'https://gist.githubusercontent.com/Harry2210/d461b829673780c2340d6e54b85d51a9/raw/Syntax%20downloader%20version.json';
    }
    if (this.dom.setAutoCheckUpdates) {
      this.dom.setAutoCheckUpdates.checked = settings.autoCheckUpdates !== false;
    }

    if (settings.theme === 'light' || settings.theme === 'dark') {
      this._setTheme(settings.theme, false);
    }
  }

  async _pickDirectory(title) {
    try {
      const res = await window.appAPI?.showSaveDialog({ title, properties: ['openDirectory'] });
      if (res && !res.canceled && res.filePaths?.[0]) return res.filePaths[0];
    } catch (err) {
      this.notifications.show(`Could not open folder picker: ${err.message || err}`, 'error');
    }
    return null;
  }

  async _loadDownloads() {
    if (!window.appAPI) return;
    try {
      const list = await window.appAPI.getDownloads() || [];
      this.downloads.clear();
      list.forEach(dl => this.downloads.set(dl.id, dl));
      this.renderDownloads();
      this._updateSidebarBadges();
      this._updateStatusBar();
    } catch (err) {
      console.warn('Error loading downloads:', err);
      this.notifications.show(`Could not load downloads: ${err.message || err}`, 'error');
    }
  }

  /* ─────────────────────────────────────────────────────────────
     VIEW & RENDERING
     ───────────────────────────────────────────────────────────── */
  _updateViewTitle() {
    const titles = {
      all: 'All Downloads',
      downloading: 'Downloading',
      completed: 'Completed',
      scheduled: 'Scheduled Downloads',
      'category:Videos': 'Videos',
      'category:Music': 'Music',
      'category:Documents': 'Documents',
      'category:Programs': 'Programs',
      'category:Compressed': 'Compressed Archives',
      'category:Images': 'Images',
      'category:Other': 'Other Files'
    };
    this.dom.viewTitle.textContent = titles[this.currentView] || 'Downloads';
  }

  getFilteredDownloads() {
    let list = Array.from(this.downloads.values());

    // Category or State filter
    if (this.currentView === 'downloading') {
      list = list.filter(d => d.status === 'downloading' || d.status === 'queued');
    } else if (this.currentView === 'completed') {
      list = list.filter(d => d.status === 'complete');
    } else if (this.currentView === 'scheduled') {
      list = list.filter(d => d.status === 'scheduled');
    } else if (this.currentView.startsWith('category:')) {
      const cat = this.currentView.split(':')[1];
      list = list.filter(d => this.getCategory(d) === cat);
    }

    // Search filter
    if (this.searchQuery) {
      list = list.filter(d =>
        (d.filename && d.filename.toLowerCase().includes(this.searchQuery)) ||
        (d.url && d.url.toLowerCase().includes(this.searchQuery))
      );
    }

    // Sorting
    list.sort((a, b) => {
      if (this.currentSort === 'filename') {
        return (a.filename || '').localeCompare(b.filename || '');
      } else if (this.currentSort === 'totalSize') {
        return (b.totalSize || 0) - (a.totalSize || 0);
      } else if (this.currentSort === 'progress') {
        return (b.progress || 0) - (a.progress || 0);
      } else {
        // dateAdded default
        const dateA = new Date(a.dateAdded || 0).getTime();
        const dateB = new Date(b.dateAdded || 0).getTime();
        return dateB - dateA;
      }
    });

    return list;
  }

  renderDownloads() {
    const items = this.getFilteredDownloads();
    this.dom.downloadList.innerHTML = '';

    if (items.length === 0) {
      this.dom.downloadList.appendChild(this.dom.emptyState);
      this.dom.emptyState.style.display = 'flex';
      return;
    }

    this.dom.emptyState.style.display = 'none';
    items.forEach(dl => {
      const card = this._createDownloadCard(dl);
      this.dom.downloadList.appendChild(card);
    });
  }

  getCategory(dl) {
    if (!dl) return 'Other';
    const cat = (dl.category || '').toLowerCase();
    const fn = (dl.filename || dl.url || '').toLowerCase();

    if (cat.includes('music') || cat.includes('audio') || /\.(mp3|wav|ogg|aac|flac|m4a|wma|alac|aiff)$/i.test(fn)) {
      return 'Music';
    }
    if (cat.includes('video') || /\.(mp4|mkv|webm|avi|mov|wmv|flv|m4v|3gp|ts|mpg|mpeg)$/i.test(fn) || dl.isStream) {
      return 'Videos';
    }
    if (cat.includes('doc') || /\.(pdf|doc|docx|xls|xlsx|ppt|pptx|txt|rtf|odt|csv|epub)$/i.test(fn)) {
      return 'Documents';
    }
    if (cat.includes('compress') || cat.includes('archive') || /\.(zip|rar|7z|tar|gz|bz2|xz|tgz|cab|iso)$/i.test(fn)) {
      return 'Compressed';
    }
    if (cat.includes('program') || cat.includes('app') || /\.(exe|msi|dmg|deb|rpm|appimage|apk|bat|cmd)$/i.test(fn)) {
      return 'Programs';
    }
    if (cat.includes('image') || /\.(jpg|jpeg|png|gif|bmp|svg|webp|ico|tiff|psd|ai)$/i.test(fn)) {
      return 'Images';
    }
    return 'Other';
  }

  getCategoryIcon(dl) {
    const icons = {
      Videos: '🎬',
      Music: '🎵',
      Documents: '📄',
      Programs: '💾',
      Compressed: '📦',
      Images: '🖼️',
      Other: '📁'
    };
    return icons[this.getCategory(dl)] || '📁';
  }

  _createDownloadCard(dl) {
    const card = document.createElement('div');
    card.className = 'download-item';
    card.id = `dl-item-${dl.id}`;
    card.dataset.id = dl.id;

    const progress = Math.min(100, Math.max(0, Math.round(dl.progress || 0)));
    const isDownloading = dl.status === 'downloading';
    const totalSize = Number(dl.totalSize) || 0;
    const downloadedSize = Number(dl.downloadedSize) || 0;
    const hasKnownTotal = totalSize > 0 && totalSize >= downloadedSize;
    const hasDownloadedBytes = downloadedSize > 0;
    const isIndeterminate = isDownloading && (!hasKnownTotal || !hasDownloadedBytes);
    const isComplete = dl.status === 'complete';
    const isPaused = dl.status === 'paused';

    const icon = this.getCategoryIcon(dl);
    const safeName = this.escapeHtml(dl.filename || 'Unknown');
    const safeTitle = this.escapeHtml(dl.filename || dl.url || 'Unknown');
    const safeStatus = this.escapeHtml(dl.status || 'unknown');
    const safeStage = this.escapeHtml(dl.stage || dl.status || '');
    const eta = isDownloading && dl.speed > 0 && hasKnownTotal && totalSize > downloadedSize
      ? this.formatEta(totalSize - downloadedSize, dl.speed)
      : '';

    card.innerHTML = `
      <div class="dl-icon">${icon}</div>
      <div class="dl-info-row">
        <span class="dl-filename" title="${safeTitle}">${safeName}</span>
        <span class="dl-status ${safeStatus}">${safeStage}</span>
      </div>

      <div class="dl-progress-row">
        <div class="dl-progress-bar-wrap">
          <div class="dl-progress-bar ${isDownloading ? 'active' : ''} ${isIndeterminate ? 'indeterminate' : ''}" style="width: ${isIndeterminate ? 35 : progress}%"></div>
        </div>
        <span class="dl-progress-pct">${isIndeterminate ? (hasDownloadedBytes ? 'Working' : 'Starting') : `${progress}%`}</span>
      </div>

      <div class="dl-details-row">
        <span class="dl-size">${this.formatBytes(downloadedSize)} / ${hasKnownTotal ? this.formatBytes(totalSize) : 'Size unknown'}</span>
        <span class="dl-speed">${isDownloading ? this.formatSpeed(dl.speed || 0) : ''}</span>
        <span class="dl-eta">${eta ? `ETA: ${eta}` : ''}</span>
        ${dl.segments && dl.segments.length > 1 ? `<button class="btn-toggle-segments" style="background:none; border:none; color:var(--accent); cursor:pointer; font-size:11px;">${dl.segments.length} Segments ▾</button>` : ''}
      </div>

      <div class="dl-actions"></div>

      <!-- Segment Detail Row (collapsible) -->
      <div class="dl-segments">
        ${(dl.segments || []).map((seg, i) => {
          const segTotal = ((seg.end - seg.start) + 1) || 1;
          const segProgress = Math.min(100, Math.max(0, Math.round(((seg.downloaded || 0) / segTotal) * 100)));
          return `
            <div class="dl-segment">
              <span class="dl-segment-label">Part ${i + 1}</span>
              <div class="dl-segment-bar">
                <div class="dl-segment-fill" style="width: ${segProgress}%"></div>
              </div>
              <span class="dl-segment-pct">${segProgress}%</span>
            </div>
          `;
        }).join('')}
      </div>
    `;

    // Render action buttons
    this._renderCardActions(card, dl);

    // Toggle segment details
    card.querySelector('.btn-toggle-segments')?.addEventListener('click', (e) => {
      e.stopPropagation();
      const segPanel = card.querySelector('.dl-segments');
      segPanel?.classList.toggle('expanded');
    });

    // Right-click context menu
    card.addEventListener('contextmenu', (e) => {
      e.preventDefault();
      this._showContextMenuForDownload(dl, e.clientX, e.clientY);
    });

    return card;
  }

  _renderCardActions(card, dl) {
    const actionsEl = card.querySelector('.dl-actions');
    if (!actionsEl) return;
    card.dataset.status = dl.status;

    const isDownloading = dl.status === 'downloading' || dl.status === 'queued';
    const isComplete = dl.status === 'complete';
    const canResume = dl.status === 'paused' || dl.status === 'error';
    const canCancel = !['complete', 'cancelled'].includes(dl.status);

    actionsEl.innerHTML = `
      ${isDownloading ? `
        <button class="dl-action-btn btn-pause" title="Pause">
          <svg viewBox="0 0 24 24" fill="none"><path d="M6 4h4v16H6zm8 0h4v16h-4z" fill="currentColor"/></svg>
        </button>
      ` : ''}
      ${canResume ? `
        <button class="dl-action-btn btn-resume" title="Resume">
          <svg viewBox="0 0 24 24" fill="none"><polygon points="6,4 20,12 6,20" fill="currentColor"/></svg>
        </button>
      ` : ''}
      ${canCancel ? `
        <button class="dl-action-btn btn-cancel" title="Stop">
          <svg viewBox="0 0 24 24" fill="none"><rect x="6" y="6" width="12" height="12" rx="1.5" fill="currentColor"/></svg>
        </button>
      ` : ''}
      ${isComplete ? `
        <button class="dl-action-btn btn-open" title="Open File">
          <svg viewBox="0 0 24 24" fill="none"><path d="M15 3h6v6m-11 5L21 3M10 5H6a2 2 0 00-2 2v12a2 2 0 002 2h12a2 2 0 002-2v-4" stroke="currentColor" stroke-width="2" stroke-linecap="round"/></svg>
        </button>
      ` : ''}
      <button class="dl-action-btn btn-folder" title="Open Folder">
        <svg viewBox="0 0 24 24" fill="none"><path d="M22 19a2 2 0 01-2 2H4a2 2 0 01-2-2V5a2 2 0 012-2h5l2 3h9a2 2 0 012 2z" stroke="currentColor" stroke-width="2"/></svg>
      </button>
      <button class="dl-action-btn danger btn-delete" title="Delete">
        <svg viewBox="0 0 24 24" fill="none"><path d="M19 7l-.867 12.142A2 2 0 0116.138 21H7.862a2 2 0 01-1.995-1.858L5 7m5 4v6m4-6v6m1-10V4a1 1 0 00-1-1h-4a1 1 0 00-1 1v3M4 7h16" stroke="currentColor" stroke-width="2" stroke-linecap="round"/></svg>
      </button>
    `;

    // Action button listeners
    actionsEl.querySelector('.btn-pause')?.addEventListener('click', async (e) => {
      e.stopPropagation();
      try {
        await window.appAPI?.pauseDownload(dl.id);
      } catch (error) {
        this.notifications.show(`Could not pause download: ${error.message || error}`, 'error');
        await this._loadDownloads();
      }
    });

    actionsEl.querySelector('.btn-resume')?.addEventListener('click', async (e) => {
      e.stopPropagation();
      try {
        await window.appAPI?.resumeDownload(dl.id);
      } catch (error) {
        this.notifications.show(`Could not resume download: ${error.message || error}`, 'error');
        await this._loadDownloads();
      }
    });

    actionsEl.querySelector('.btn-cancel')?.addEventListener('click', async (e) => {
      e.stopPropagation();
      try {
        await window.appAPI?.cancelDownload(dl.id);
        await this._loadDownloads();
      } catch (error) {
        this.notifications.show(`Could not stop download: ${error.message || error}`, 'error');
      }
    });

    actionsEl.querySelector('.btn-open')?.addEventListener('click', (e) => {
      e.stopPropagation();
      const filePath = this._downloadPath(dl);
      window.appAPI?.openFile(filePath).catch((error) => {
        this.notifications.show(`Could not open file: ${error.message || error}`, 'error');
      });
    });

    actionsEl.querySelector('.btn-folder')?.addEventListener('click', (e) => {
      e.stopPropagation();
      const targetDir = dl.saveDir || this.settings?.downloadDir || '';
      window.appAPI?.openFolder(targetDir).catch((error) => {
        this.notifications.show(`Could not open folder: ${error.message || error}`, 'error');
      });
    });

    actionsEl.querySelector('.btn-delete')?.addEventListener('click', async (e) => {
      e.stopPropagation();
      if (!confirm(`Remove "${dl.filename}" from the download list?${dl.status === 'downloading' ? ' The partial file will also be removed.' : ''}`)) {
        return;
      }
      try {
        await window.appAPI?.deleteDownload(dl.id);
        this.downloads.delete(dl.id);
        card.classList.add('removing');
        setTimeout(() => {
          this.renderDownloads();
          this._updateSidebarBadges();
          this._updateStatusBar();
        }, 200);
      } catch (error) {
        this.notifications.show(`Could not delete download: ${error.message || error}`, 'error');
      }
    });
  }

  _downloadPath(dl) {
    if (!dl) return '';
    const separator = (dl.saveDir || '').includes('\\') ? '\\' : '/';
    return dl.saveDir ? `${dl.saveDir.replace(/[\\/]$/, '')}${separator}${dl.filename}` : (dl.filename || '');
  }

  _updateDownloadCard(dl) {
    const card = document.getElementById(`dl-item-${dl.id}`);
    if (!card) {
      if (this.getFilteredDownloads().some((item) => item.id === dl.id)) {
        this.renderDownloads();
      }
      return;
    }

    // A status, category, filename, or search change can remove the item from
    // the active view. Rebuild the list instead of leaving a stale card.
    if (!this.getFilteredDownloads().some((item) => item.id === dl.id)) {
      this.renderDownloads();
      return;
    }

    const progress = Math.min(100, Math.max(0, Math.round(dl.progress || 0)));
    const isDownloading = dl.status === 'downloading';
    const totalSize = Number(dl.totalSize) || 0;
    const downloadedSize = Number(dl.downloadedSize) || 0;
    const hasKnownTotal = totalSize > 0 && totalSize >= downloadedSize;
    const hasDownloadedBytes = downloadedSize > 0;
    const isIndeterminate = isDownloading && (!hasKnownTotal || !hasDownloadedBytes);

    // Update category icon and filename
    const iconEl = card.querySelector('.dl-icon');
    if (iconEl) {
      iconEl.textContent = this.getCategoryIcon(dl);
    }
    const nameEl = card.querySelector('.dl-filename');
    if (nameEl && dl.filename && nameEl.textContent !== dl.filename) {
      nameEl.textContent = dl.filename;
      nameEl.title = dl.filename;
    }

    // Segments are discovered after the probe completes; rebuild the card
    // once when their count changes so the detail rows exist.
    if (dl.segments && dl.segments.length !== card.querySelectorAll('.dl-segment').length) {
      this.renderDownloads();
      return;
    }

    // Update status badge
    const statusEl = card.querySelector('.dl-status');
    if (statusEl) {
      statusEl.className = `dl-status ${dl.status}`;
      statusEl.textContent = dl.stage || dl.status;
    }

    // Update action buttons (Pause/Resume/Open) if status changed
    if (card.dataset.status !== dl.status) {
      this._renderCardActions(card, dl);
    }

    // Update progress bar
    const barEl = card.querySelector('.dl-progress-bar');
    if (barEl) {
      barEl.style.width = `${isIndeterminate ? 35 : progress}%`;
      if (isDownloading) barEl.classList.add('active');
      else barEl.classList.remove('active');
      barEl.classList.toggle('indeterminate', isIndeterminate);
    }

    const pctEl = card.querySelector('.dl-progress-pct');
    if (pctEl) {
      pctEl.textContent = isIndeterminate
        ? (hasDownloadedBytes ? 'Working' : 'Starting')
        : `${progress}%`;
    }

    // Update details (size, speed, eta)
    const sizeEl = card.querySelector('.dl-size');
    if (sizeEl) {
      sizeEl.textContent = `${this.formatBytes(downloadedSize)} / ${hasKnownTotal ? this.formatBytes(totalSize) : 'Size unknown'}`;
    }

    const speedEl = card.querySelector('.dl-speed');
    if (speedEl) speedEl.textContent = isDownloading ? this.formatSpeed(dl.speed || 0) : '';

    const etaEl = card.querySelector('.dl-eta');
    if (etaEl) {
      const remaining = totalSize - downloadedSize;
      etaEl.textContent = isDownloading && dl.speed > 0 && hasKnownTotal && remaining > 0
        ? `ETA: ${this.formatEta(remaining, dl.speed)}`
        : '';
    }

    // Update segments if open
    if (dl.segments) {
      const segFills = card.querySelectorAll('.dl-segment-fill');
      const segPcts = card.querySelectorAll('.dl-segment-pct');
      dl.segments.forEach((seg, i) => {
        if (segFills[i]) {
          const segTotal = ((seg.end - seg.start) + 1) || 1;
          const p = Math.min(100, Math.max(0, Math.round(((seg.downloaded || 0) / segTotal) * 100)));
          segFills[i].style.width = `${p}%`;
          if (segPcts[i]) segPcts[i].textContent = `${p}%`;
        }
      });
    }
  }

  _showContextMenuForDownload(dl, x, y) {
    const filePath = this._downloadPath(dl);
    const items = [
      {
        label: 'Open File',
        icon: '📄',
        disabled: dl.status !== 'complete',
        action: () => window.appAPI?.openFile(filePath).catch((error) => {
          this.notifications.show(`Could not open file: ${error.message || error}`, 'error');
        })
      },
      {
        label: 'Open Folder',
        icon: '📁',
        action: () => {
          const targetDir = dl.saveDir || this.settings?.downloadDir || '';
          window.appAPI?.openFolder(targetDir).catch((error) => {
            this.notifications.show(`Could not open folder: ${error.message || error}`, 'error');
          });
        }
      },
      { separator: true },
      {
        label: 'Pause Download',
        icon: '⏸️',
        disabled: dl.status !== 'downloading',
        action: () => window.appAPI?.pauseDownload(dl.id)
      },
      {
        label: 'Resume Download',
        icon: '▶️',
        disabled: !['paused', 'error', 'queued'].includes(dl.status),
        action: () => window.appAPI?.resumeDownload(dl.id)
      },
      {
        label: 'Stop Download',
        icon: '⏹️',
        disabled: ['complete', 'cancelled'].includes(dl.status),
        action: () => window.appAPI?.cancelDownload(dl.id)
      },
      { separator: true },
      {
        label: 'Copy Download Link',
        icon: '🔗',
        action: async () => {
          try {
            await navigator.clipboard.writeText(dl.url);
            this.notifications.show('URL copied to clipboard', 'info');
          } catch {
            this.notifications.show('Could not copy the URL', 'error');
          }
        }
      },
      { separator: true },
      {
        label: 'Delete from List',
        icon: '🗑️',
        action: async () => {
          try {
            await window.appAPI?.deleteDownload(dl.id);
            this.downloads.delete(dl.id);
            this.renderDownloads();
            this._updateSidebarBadges();
            this._updateStatusBar();
          } catch (error) {
            this.notifications.show(`Could not delete download: ${error.message || error}`, 'error');
          }
        }
      }
    ];

    this.contextMenu.show(x, y, items);
  }

  /* ─────────────────────────────────────────────────────────────
     MODAL CONTROLS
     ───────────────────────────────────────────────────────────── */
  showAddModal(prefillUrl = '') {
    this.dom.modalAdd.classList.add('open');
    this.dom.inputUrl.value = prefillUrl;
    this.dom.inputFilename.value = '';
    this.filenameAuto = true;
    if (this.dom.inputSha256) this.dom.inputSha256.value = '';
    if (this.dom.inputSaveDir && !this.dom.inputSaveDir.value) {
      this.dom.inputSaveDir.value = this.settings?.downloadDir || '';
    }
    if (this.dom.inputSegments) {
      const segments = Math.max(1, Math.min(32, Number(this.settings?.maxSegments) || 8));
      this.dom.inputSegments.value = segments;
      if (this.dom.valSegments) this.dom.valSegments.textContent = segments;
    }
    if (this.dom.chkSchedule) {
      this.dom.chkSchedule.checked = false;
      this.dom.scheduleTimeWrap.style.display = 'none';
      this.dom.inputScheduleTime.value = '';
    }
    if (this.dom.chkStartImmediate) this.dom.chkStartImmediate.checked = true;
    if (prefillUrl) this._handleUrlInput();
    this.dom.inputUrl.focus();
  }

  showExtensionModal() {
    this.dom.modalExtension?.classList.add('open');
  }

  showSettingsModal() {
    this._applySettingsToUi();
    this.dom.modalSettings?.classList.add('open');
  }

  async _saveSettings() {
    const folder = this.dom.setDownloadDir?.value.trim() || this.settings?.downloadDir || '';
    const maxConcurrent = Math.max(1, Math.min(10, parseInt(this.dom.setMaxConcurrent?.value, 10) || 3));
    const maxSegments = Math.max(1, Math.min(32, parseInt(this.dom.setDefaultSegments?.value, 10) || 8));
    const speedLimitKb = Math.max(0, parseInt(this.dom.setSpeedLimit?.value, 10) || 0);

    const updated = {
      ...this.settings,
      downloadDir: folder,
      maxConcurrent,
      maxSegments,
      speedLimit: speedLimitKb * 1024,
      notifications: !!this.dom.setNotifications?.checked,
      startMinimized: !!this.dom.setMinToTray?.checked,
      closeToTray: !!this.dom.setCloseToTray?.checked,
      startWithWindows: !!this.dom.setStartWithWindows?.checked,
      shutdownAfterComplete: !!this.dom.setShutdown?.checked,
      updateFeedUrl: this.dom.setUpdateFeedUrl ? this.dom.setUpdateFeedUrl.value.trim() : (this.settings?.updateFeedUrl || 'https://gist.githubusercontent.com/Harry2210/d461b829673780c2340d6e54b85d51a9/raw/Syntax%2520downloader%2520version.json'),
      autoCheckUpdates: this.dom.setAutoCheckUpdates ? !!this.dom.setAutoCheckUpdates.checked : (this.settings?.autoCheckUpdates !== false),
      theme: this.theme
    };

    const button = this.dom.btnSaveSettings;
    if (button) button.disabled = true;
    try {
      this.settings = await window.appAPI?.updateSettings(updated) || updated;
      this._applySettingsToUi();
      if (this.dom.inputSaveDir) this.dom.inputSaveDir.value = folder;
      this._closeModal('modalSettings');
      this.notifications.show('Settings saved', 'success');
    } catch (error) {
      this.notifications.show(`Could not save settings: ${error.message || error}`, 'error');
    } finally {
      if (button) button.disabled = false;
    }
  }

  _closeModal(modalId) {
    const el = document.getElementById(modalId);
    if (el) el.classList.remove('open');
  }

  _handleUrlInput() {
    if (!this.filenameAuto) return;
    const url = this.dom.inputUrl.value.trim();
    if (!url) return;
    try {
      const parsed = new URL(url);
      const name = decodeURIComponent(parsed.pathname.split('/').pop().split('?')[0]);
      if (name) {
        this.dom.inputFilename.value = name;
      }
    } catch {
      // not a valid URL yet
    }
  }

  async _submitAddDownload() {
    const url = this.dom.inputUrl.value.trim();
    if (!url) {
      this.notifications.show('Please enter a download URL', 'warning');
      return;
    }

    let parsed;
    try {
      parsed = new URL(url);
    } catch {
      this.notifications.show('Please enter a valid HTTP or HTTPS URL', 'warning');
      return;
    }
    if (!['http:', 'https:'].includes(parsed.protocol)) {
      this.notifications.show('Only HTTP and HTTPS downloads are supported', 'warning');
      return;
    }

    const typedFilename = this.dom.inputFilename.value.trim();
    // When the filename was auto-filled from the URL, leave it unlocked so the
    // backend can adopt a better name from Content-Disposition.
    const filename = this.filenameAuto ? '' : typedFilename;
    const saveDir = this.dom.inputSaveDir.value.trim();
    const maxSegments = Math.max(1, Math.min(32, parseInt(this.dom.inputSegments.value, 10) || 8));
    const sha256 = (this.dom.inputSha256?.value || '').trim();
    if (sha256 && !/^[a-f0-9]{64}$/i.test(sha256)) {
      this.notifications.show('The SHA-256 value must be 64 hexadecimal characters', 'warning');
      return;
    }

    const startImmediate = this.dom.chkStartImmediate ? !!this.dom.chkStartImmediate.checked : true;
    const isScheduled = !!this.dom.chkSchedule?.checked;
    const scheduleValue = isScheduled ? (this.dom.inputScheduleTime?.value || '') : '';
    if (isScheduled) {
      if (!scheduleValue) {
        this.notifications.show('Choose a date and time for the scheduled download', 'warning');
        return;
      }
      if (new Date(scheduleValue).getTime() <= Date.now()) {
        this.notifications.show('The scheduled time must be in the future', 'warning');
        return;
      }
    }

    const button = this.dom.btnConfirmAdd;
    if (button) button.disabled = true;
    try {
      const dl = await window.appAPI?.addDownload(url, filename, saveDir, {
        maxSegments,
        startImmediate: !isScheduled && startImmediate,
        scheduleTime: isScheduled ? scheduleValue : null,
        sha256
      });

      this._closeModal('modalAdd');
      this.notifications.show(`Download added: ${typedFilename || 'Accelerating...'}`, 'success');

      if (dl) {
        this.downloads.set(dl.id, dl);
        this.renderDownloads();
        this._updateSidebarBadges();
        this._updateStatusBar();
      }
    } catch (err) {
      this.notifications.show(`Error starting download: ${err.message || err}`, 'error');
    } finally {
      if (button) button.disabled = false;
    }
  }

  /* ─────────────────────────────────────────────────────────────
     STATS & BADGES
     ───────────────────────────────────────────────────────────── */
  async _runBulkAction(method, successMessage) {
    if (!window.appAPI) return;
    try {
      await window.appAPI[method]();
      this.notifications.show(successMessage, 'info');
    } catch (error) {
      this.notifications.show(`Action failed: ${error.message || error}`, 'error');
    } finally {
      await this._loadDownloads();
    }
  }

  _updateSidebarBadges() {
    const all = Array.from(this.downloads.values());
    const count = (fn) => all.filter(fn).length;

    const setBadge = (el, n) => {
      if (el) el.textContent = n > 0 ? n : '';
    };

    setBadge(this.dom.badgeAll, all.length);
    setBadge(this.dom.badgeDownloading, count(d => d.status === 'downloading' || d.status === 'queued'));
    setBadge(this.dom.badgeCompleted, count(d => d.status === 'complete'));
    setBadge(this.dom.badgeScheduled, count(d => d.status === 'scheduled'));

    setBadge(this.dom.badgeVideos, count(d => this.getCategory(d) === 'Videos'));
    setBadge(this.dom.badgeMusic, count(d => this.getCategory(d) === 'Music'));
    setBadge(this.dom.badgeDocuments, count(d => this.getCategory(d) === 'Documents'));
    setBadge(this.dom.badgePrograms, count(d => this.getCategory(d) === 'Programs'));
    setBadge(this.dom.badgeCompressed, count(d => this.getCategory(d) === 'Compressed'));
    setBadge(this.dom.badgeImages, count(d => this.getCategory(d) === 'Images'));
    setBadge(this.dom.badgeOther, count(d => this.getCategory(d) === 'Other'));
  }

  _updateStatusBar() {
    const all = Array.from(this.downloads.values());
    const active = all.filter(d => d.status === 'downloading');

    if (this.dom.statusActiveCount) {
      this.dom.statusActiveCount.textContent = active.length;
    }

    if (this.dom.statusCurrentSpeed) {
      this.dom.statusCurrentSpeed.textContent = this.formatSpeed(this.currentSpeed);
    }

    if (this.dom.statusConnectionText) {
      this.dom.statusConnectionText.textContent = this.backendReady ? 'Connected' : 'Connecting...';
    }
    if (this.dom.statusDotIndicator) {
      this.dom.statusDotIndicator.classList.toggle('idle', !this.backendReady);
    }

    let todayBytes = 0;
    const today = new Date().toDateString();
    all.forEach(d => {
      if (d.status === 'complete' && d.dateCompleted && new Date(d.dateCompleted).toDateString() === today) {
        todayBytes += (d.totalSize || d.downloadedSize || 0);
      }
    });

    if (this.dom.statusTodayDownloaded) {
      this.dom.statusTodayDownloaded.textContent = this.formatBytes(todayBytes);
    }
  }

  /* ─────────────────────────────────────────────────────────────
     FORMATTERS
     ───────────────────────────────────────────────────────────── */
  escapeHtml(value) {
    return String(value ?? '')
      .replaceAll('&', '&amp;')
      .replaceAll('<', '&lt;')
      .replaceAll('>', '&gt;')
      .replaceAll('"', '&quot;')
      .replaceAll("'", '&#39;');
  }

  formatBytes(bytes) {
    if (!bytes || isNaN(bytes) || bytes === 0) return '0 B';
    const k = 1024;
    const sizes = ['B', 'KB', 'MB', 'GB', 'TB'];
    const i = Math.floor(Math.log(bytes) / Math.log(k));
    return parseFloat((bytes / Math.pow(k, i)).toFixed(1)) + ' ' + sizes[i];
  }

  formatSpeed(bytesPerSec) {
    if (!bytesPerSec || bytesPerSec <= 0) return '0 KB/s';
    return this.formatBytes(bytesPerSec) + '/s';
  }

  formatEta(remainingBytes, speed) {
    if (!speed || speed <= 0 || !remainingBytes || remainingBytes <= 0) return '--:--';
    const sec = Math.round(remainingBytes / speed);
    if (sec < 60) return `${sec}s`;
    const min = Math.floor(sec / 60);
    const s = sec % 60;
    if (min < 60) return `${min}m ${s}s`;
    const hrs = Math.floor(min / 60);
    const m = min % 60;
    return `${hrs}h ${m}m`;
  }

  _isVersionNewer(remote, current) {
    const parse = v => (v || '').replace(/^[vV]/, '').split(/[+-]/)[0].split('.').map(n => parseInt(n, 10) || 0);
    const r = parse(remote);
    const c = parse(current);
    const len = Math.max(r.length, c.length);
    for (let i = 0; i < len; i++) {
      const rVal = r[i] || 0;
      const cVal = c[i] || 0;
      if (rVal > cVal) return true;
      if (rVal < cVal) return false;
    }
    return false;
  }

  async _handleCheckUpdates(manual = false) {
    if (manual && this.dom.btnCheckUpdates) {
      this.dom.btnCheckUpdates.disabled = true;
      this.dom.btnCheckUpdates.textContent = 'Checking...';
    }
    try {
      let res = null;

      // 1. Try frontend fetch (WebView2 Happy Eyeballs & direct CDN access)
      try {
        const feedUrl = (this.settings?.updateFeedUrl || 'https://gist.githubusercontent.com/Harry2210/d461b829673780c2340d6e54b85d51a9/raw/Syntax%2520downloader%2520version.json').trim();
        let clean = feedUrl.replace(/ /g, '%20');
        const rawIdx = clean.indexOf('/raw/');
        if (rawIdx !== -1 && clean.includes('gist.githubusercontent.com')) {
          const afterRaw = clean.slice(rawIdx + 5);
          const slashIdx = afterRaw.indexOf('/');
          if (slashIdx === 40) {
            clean = clean.slice(0, rawIdx) + '/raw/' + afterRaw.slice(slashIdx + 1);
          }
        }
        if (clean.includes('gist.githubusercontent.com') && clean.includes('Syntax%20downloader') && !clean.includes('Syntax%2520downloader')) {
          clean = clean.replace('Syntax%20downloader', 'Syntax%2520downloader');
        }
        const targetUrl = `${clean}${clean.includes('?') ? '&' : '?'}_t=${Date.now()}`;
        const controller = new AbortController();
        const timeoutId = setTimeout(() => controller.abort(), 5000);
        const resp = await fetch(targetUrl, { signal: controller.signal, cache: 'no-store' });
        clearTimeout(timeoutId);
        if (resp.ok) {
          const payload = await resp.json();
          const currentVersion = '1.0.0';
          const remoteVersion = (payload.version || '').trim();
          res = {
            updateAvailable: this._isVersionNewer(remoteVersion, currentVersion),
            currentVersion,
            remoteVersion,
            name: payload.name || '',
            pubDate: payload.pub_date || payload.pubDate || '',
            downloadUrl: payload.downloadUrl || '',
            installerUrl: payload.installerUrl || '',
            changelog: payload.changelog || [],
            feedUrl: clean
          };
        }
      } catch (fetchErr) {
        console.warn('Frontend direct update check failed, attempting backend IPC:', fetchErr);
      }

      // 2. If frontend fetch didn't return, fallback to Rust backend
      if (!res && window.appAPI?.checkForUpdates) {
        res = await window.appAPI.checkForUpdates();
      }

      if (!res) {
        throw new Error('Could not reach update server');
      }

      this.latestUpdateInfo = res;
      if (res.updateAvailable) {
        if (this.dom.updateModalHeading) {
          this.dom.updateModalHeading.textContent = res.name || `Syntax Downloader v${res.remoteVersion} is available!`;
        }
        if (this.dom.updateModalVersions) {
          this.dom.updateModalVersions.textContent = `Current: v${res.currentVersion} \u2192 Latest: v${res.remoteVersion}`;
        }
        if (this.dom.updateModalChangelog) {
          if (Array.isArray(res.changelog) && res.changelog.length > 0) {
            this.dom.updateModalChangelog.innerHTML = `<ul style="margin: 0; padding-left: 18px;">${res.changelog.map(item => `<li>${item}</li>`).join('')}</ul>`;
          } else {
            this.dom.updateModalChangelog.textContent = 'New performance improvements and bug fixes.';
          }
        }
        this.dom.modalUpdateAvailable?.classList.add('open');
        if (this.dom.updateStatusText) {
          this.dom.updateStatusText.textContent = `Update available: v${res.remoteVersion}!`;
          this.dom.updateStatusText.style.color = 'var(--accent-primary)';
        }
      } else if (manual) {
        this.notifications?.show(`You're up to date! (v${res?.currentVersion || '1.0.0'})`, 'success');
        if (this.dom.updateStatusText) {
          this.dom.updateStatusText.textContent = `You're using the latest version (v${res?.currentVersion || '1.0.0'}).`;
          this.dom.updateStatusText.style.color = '';
        }
      }
    } catch (err) {
      if (manual) {
        this.notifications?.show(`Could not check for updates: ${err.message || err}`, 'error');
        if (this.dom.updateStatusText) {
          this.dom.updateStatusText.textContent = `Update check failed: ${err.message || err}`;
        }
      }
    } finally {
      if (manual && this.dom.btnCheckUpdates) {
        this.dom.btnCheckUpdates.disabled = false;
        this.dom.btnCheckUpdates.textContent = 'Check Now';
      }
    }
  }

  _handleDownloadUpdate() {
    const url = this.latestUpdateInfo?.installerUrl || this.latestUpdateInfo?.downloadUrl;
    if (url) {
      window.appAPI?.openExternal(url);
    } else {
      window.appAPI?.openExternal('https://github.com/Harry2210/Syntax-Downloader/releases/latest');
    }
    this._closeModal('modalUpdateAvailable');
  }
}

// Instantiate and start app on DOM ready
document.addEventListener('DOMContentLoaded', () => {
  const app = new SyntaxDownloaderApp();
  app.init();
  window.syntaxApp = app;
});
