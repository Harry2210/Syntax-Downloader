/**
 * NotificationManager — Animated toast notification system.
 * Toasts slide in from the top-right, stack, auto-dismiss with progress bar.
 */
export class NotificationManager {
  constructor(container) {
    this.container = container || document.body;
    this._wrapper = null;
    this._id = 0;
    this.enabled = true;
    this._init();
  }

  /* ── public ── */

  /**
   * Show a toast notification.
   * @param {string} message
   * @param {'success'|'error'|'warning'|'info'} [type='info']
   * @param {number} [duration=4000] ms, 0 = persistent
   * @returns {number} notification id
   */
  show(message, type = 'info', duration = 4000) {
    if (!message || !this.enabled) return null;

    // Deduplication: prevent stacking identical notifications
    const active = Array.from(this._wrapper.querySelectorAll('.toast-item:not(.toast-exit)'));
    const duplicate = active.find(t => t.querySelector('.toast-message')?.textContent === message);
    if (duplicate) {
      if (duplicate.dataset.timer) {
        clearTimeout(Number(duplicate.dataset.timer));
      }
      if (duration > 0) {
        duplicate.dataset.timer = setTimeout(() => this.dismiss(Number(duplicate.dataset.toastId)), duration);
      }
      return Number(duplicate.dataset.toastId);
    }

    const id = ++this._id;
    const toast = this._createToast(id, message, type, duration);
    this._wrapper.prepend(toast);

    // Trigger enter animation
    requestAnimationFrame(() => {
      requestAnimationFrame(() => toast.classList.add('toast-enter'));
    });

    if (duration > 0) {
      const timer = setTimeout(() => this.dismiss(id), duration);
      toast.dataset.timer = timer;
    }

    return id;
  }

  /** Programmatically dismiss a notification. */
  dismiss(id) {
    const el = this._wrapper.querySelector(`[data-toast-id="${id}"]`);
    if (!el || el.classList.contains('toast-exit')) return;
    el.classList.remove('toast-enter');
    el.classList.add('toast-exit');
    el.addEventListener('animationend', () => el.remove(), { once: true });
  }

  success(msg, duration) { return this.show(msg, 'success', duration); }
  error(msg, duration)   { return this.show(msg, 'error', duration); }
  warning(msg, duration) { return this.show(msg, 'warning', duration); }
  info(msg, duration)    { return this.show(msg, 'info', duration); }

  /* ── private ── */

  _init() {
    this._wrapper = document.createElement('div');
    this._wrapper.className = 'toast-wrapper';
    this.container.appendChild(this._wrapper);
  }

  _createToast(id, message, type, duration) {
    const toast = document.createElement('div');
    toast.className = `toast toast-${type}`;
    toast.dataset.toastId = id;

    const icons = {
      success: `<svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.5" stroke-linecap="round" stroke-linejoin="round"><polyline points="20 6 9 17 4 12"/></svg>`,
      error:   `<svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.5" stroke-linecap="round" stroke-linejoin="round"><circle cx="12" cy="12" r="10"/><line x1="15" y1="9" x2="9" y2="15"/><line x1="9" y1="9" x2="15" y2="15"/></svg>`,
      warning: `<svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.5" stroke-linecap="round" stroke-linejoin="round"><path d="M10.29 3.86L1.82 18a2 2 0 0 0 1.71 3h16.94a2 2 0 0 0 1.71-3L13.71 3.86a2 2 0 0 0-3.42 0z"/><line x1="12" y1="9" x2="12" y2="13"/><line x1="12" y1="17" x2="12.01" y2="17"/></svg>`,
      info:    `<svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.5" stroke-linecap="round" stroke-linejoin="round"><circle cx="12" cy="12" r="10"/><line x1="12" y1="16" x2="12" y2="12"/><line x1="12" y1="8" x2="12.01" y2="8"/></svg>`,
    };

    toast.innerHTML = `
      <span class="toast-icon">${icons[type] || icons.info}</span>
      <span class="toast-message">${this._escapeHTML(message)}</span>
      <button class="toast-close" aria-label="Close notification">&times;</button>
      ${duration > 0 ? `<div class="toast-progress"><div class="toast-progress-bar" style="animation-duration:${duration}ms"></div></div>` : ''}
    `;

    toast.querySelector('.toast-close').addEventListener('click', () => {
      clearTimeout(Number(toast.dataset.timer));
      this.dismiss(id);
    });

    return toast;
  }

  _escapeHTML(str) {
    const d = document.createElement('div');
    d.textContent = str;
    return d.innerHTML;
  }
}
