/**
 * ContextMenu — Custom styled right-click context menu.
 * Position-aware, keyboard-navigable, animated open/close.
 */
export class ContextMenu {
  /**
   * @param {HTMLElement} container  - element to append menu to (usually document.body)
   */
  constructor(container) {
    this.container = container || document.body;
    this._menu = null;
    this._activeIdx = -1;
    this._onKeyDown = this._handleKeyDown.bind(this);
    this._onClickOutside = this._handleClickOutside.bind(this);
    this._build();
  }

  /* ── public ── */

  /**
   * Show context menu at (x, y) with given items.
   * @param {number} x
   * @param {number} y
   * @param {Array<{label:string, icon?:string, action?:Function, disabled?:boolean, separator?:boolean, shortcut?:string}>} items
   */
  show(x, y, items) {
    this.hide(); // close any open instance
    this._populate(items);

    // Position within viewport
    this._menu.style.left = '0px';
    this._menu.style.top = '0px';
    this._menu.classList.add('context-menu-visible');

    requestAnimationFrame(() => {
      const rect = this._menu.getBoundingClientRect();
      const vw = window.innerWidth;
      const vh = window.innerHeight;
      let posX = x;
      let posY = y;
      if (x + rect.width > vw - 8) posX = vw - rect.width - 8;
      if (y + rect.height > vh - 8) posY = vh - rect.height - 8;
      if (posX < 4) posX = 4;
      if (posY < 4) posY = 4;
      this._menu.style.left = posX + 'px';
      this._menu.style.top = posY + 'px';
      this._menu.classList.add('context-menu-open');
    });

    document.addEventListener('keydown', this._onKeyDown, true);
    document.addEventListener('mousedown', this._onClickOutside, true);
    this._activeIdx = -1;
  }

  /** Hide (animate out). */
  hide() {
    if (!this._menu.classList.contains('context-menu-visible')) return;
    this._menu.classList.remove('context-menu-open');
    this._menu.classList.add('context-menu-closing');
    const onEnd = () => {
      this._menu.classList.remove('context-menu-visible', 'context-menu-closing');
      this._menu.removeEventListener('animationend', onEnd);
    };
    this._menu.addEventListener('animationend', onEnd);
    document.removeEventListener('keydown', this._onKeyDown, true);
    document.removeEventListener('mousedown', this._onClickOutside, true);
  }

  destroy() {
    this.hide();
    this._menu.remove();
  }

  /* ── private ── */

  _build() {
    this._menu = document.createElement('div');
    this._menu.className = 'context-menu';
    this._menu.setAttribute('role', 'menu');
    this.container.appendChild(this._menu);
  }

  _populate(items) {
    this._menu.innerHTML = '';
    items.forEach((item, idx) => {
      if (item.separator) {
        const sep = document.createElement('div');
        sep.className = 'context-menu-separator';
        this._menu.appendChild(sep);
        return;
      }
      const el = document.createElement('button');
      el.className = 'context-menu-item' + (item.disabled ? ' disabled' : '');
      el.setAttribute('role', 'menuitem');
      el.dataset.index = idx;
      if (item.disabled) el.setAttribute('disabled', '');

      el.innerHTML = `
        ${item.icon ? `<span class="ctx-icon">${item.icon}</span>` : '<span class="ctx-icon"></span>'}
        <span class="ctx-label">${item.label}</span>
        ${item.shortcut ? `<span class="ctx-shortcut">${item.shortcut}</span>` : ''}
      `;

      if (!item.disabled && item.action) {
        el.addEventListener('click', (e) => {
          e.stopPropagation();
          this.hide();
          item.action();
        });
      }
      this._menu.appendChild(el);
    });
  }

  _getItems() {
    return [...this._menu.querySelectorAll('.context-menu-item:not(.disabled)')];
  }

  _handleKeyDown(e) {
    const items = this._getItems();
    if (!items.length) return;

    if (e.key === 'Escape') {
      e.preventDefault();
      this.hide();
      return;
    }

    if (e.key === 'ArrowDown') {
      e.preventDefault();
      this._activeIdx = (this._activeIdx + 1) % items.length;
      this._focusItem(items);
    } else if (e.key === 'ArrowUp') {
      e.preventDefault();
      this._activeIdx = (this._activeIdx - 1 + items.length) % items.length;
      this._focusItem(items);
    } else if (e.key === 'Enter' || e.key === ' ') {
      e.preventDefault();
      if (this._activeIdx >= 0 && items[this._activeIdx]) {
        items[this._activeIdx].click();
      }
    }
  }

  _focusItem(items) {
    items.forEach((it, i) => {
      it.classList.toggle('context-menu-item-active', i === this._activeIdx);
    });
    if (items[this._activeIdx]) items[this._activeIdx].focus();
  }

  _handleClickOutside(e) {
    if (!this._menu.contains(e.target)) {
      this.hide();
    }
  }
}
