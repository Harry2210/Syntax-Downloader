import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import { open, save } from '@tauri-apps/plugin-dialog';

function subscribe(eventName, callback) {
  let unlisten;
  listen(eventName, (event) => callback(event.payload))
    .then((stop) => { unlisten = stop; })
    .catch((error) => console.error(`Failed to listen for ${eventName}:`, error));
  return () => {
    if (unlisten) unlisten();
  };
}

export const appAPI = {
  addDownload: (url, filename, saveDir, options) => {
    const normalizedOptions = options?.scheduleTime
      ? { ...options, scheduleTime: new Date(options.scheduleTime).toISOString() }
      : options;
    return invoke('add_download', { url, filename, saveDir, options: normalizedOptions });
  },
  pauseDownload: (id) => invoke('pause_download', { id }),
  resumeDownload: (id) => invoke('resume_download', { id }),
  cancelDownload: (id) => invoke('cancel_download', { id }),
  deleteDownload: (id) => invoke('delete_download', { id }),
  pauseAll: () => invoke('pause_all'),
  resumeAll: () => invoke('resume_all'),
  cancelAll: () => invoke('cancel_all'),
  getDownloads: () => invoke('get_downloads'),
  getSettings: () => invoke('get_settings'),
  updateSettings: (settings) => invoke('update_settings', { settings }),
  getCategories: () => invoke('get_categories'),
  getStats: () => invoke('get_stats'),
  openFile: (filePath) => invoke('open_path', { path: filePath }),
  openFolder: (folderPath) => invoke('open_path', { path: folderPath }),
  showSaveDialog: async (options = {}) => {
    const directory = options.properties?.includes('openDirectory') ?? true;
    const path = await open({
      directory,
      multiple: false,
      title: options.title || (directory ? 'Choose Folder' : 'Choose File')
    });
    return { canceled: !path, filePaths: path ? [path] : [] };
  },
  showOpenDialog: async (options = {}) => {
    const path = await open({
      multiple: false,
      title: options.title || 'Choose File',
      filters: options.filters
    });
    return { canceled: !path, filePaths: path ? [path] : [] };
  },
  showSaveFileDialog: async (options = {}) => {
    const path = await save({
      title: options.title || 'Save File',
      defaultPath: options.defaultPath,
      filters: options.filters
    });
    return { canceled: !path, filePath: path };
  },
  onDownloadProgress: (callback) => subscribe('download:progress', callback),
  onDownloadComplete: (callback) => subscribe('download:complete', callback),
  onDownloadError: (callback) => subscribe('download:error', callback),
  onDownloadPaused: (callback) => subscribe('download:paused', callback),
  onDownloadResumed: (callback) => subscribe('download:resumed', callback),
  onDownloadCancelled: (callback) => subscribe('download:cancelled', callback),
  onNewDownload: (callback) => subscribe('download:new', callback),
  onSpeedUpdate: (callback) => subscribe('speed:update', callback),
  setClipboardMonitoring: (enabled) => invoke('set_clipboard_monitoring', { enabled }),
  getSystemInfo: () => invoke('get_system_info'),
  verifyFileHash: (filePath, algorithm) => invoke('verify_file_hash', { filePath, algorithm }),
  exportDownloads: (filePath) => invoke('export_downloads', { filePath }),
  importDownloads: (filePath) => invoke('import_downloads', { filePath }),
  scheduleDownload: (id, startTime) =>
    invoke('schedule_download', { id, startTime: new Date(startTime).toISOString() }),
  setSpeedLimit: (bytesPerSecond) => invoke('set_speed_limit', { bytesPerSecond }),
  setBandwidthSchedule: (schedule) => invoke('set_bandwidth_schedule', { schedule }),
  shutdownAfterComplete: (enabled) => invoke('shutdown_after_complete', { enabled }),
  downloadExtension: () => invoke('download_extension'),
  openExtensionFolder: () => invoke('open_extension_folder'),
  openExtensionStore: (url) => invoke('open_external', { url }),
  checkForUpdates: () => invoke('check_for_updates'),
  openExternal: (url) => invoke('open_external', { url })
};
