/**
 * SpeedGraph — Real-time download speed line chart using Canvas API
 * Renders the last 60 seconds of download speed with gradient fill.
 */
export class SpeedGraph {
  /**
   * @param {HTMLCanvasElement} canvas
   * @param {object} [options]
   */
  constructor(canvas, options = {}) {
    this.canvas = canvas;
    this.ctx = canvas.getContext('2d');
    this.data = []; // {time, speed} entries
    this.maxPoints = options.maxPoints || 60;
    this.lineColor = options.lineColor || '#00d4ff';
    this.gradientTop = options.gradientTop || 'rgba(0,212,255,0.35)';
    this.gradientBottom = options.gradientBottom || 'rgba(0,212,255,0.0)';
    this.gridColor = options.gridColor || 'rgba(255,255,255,0.06)';
    this.labelColor = options.labelColor || '#606070';
    this.textColor = options.textColor || '#ffffff';
    this.padding = { top: 28, right: 12, bottom: 22, left: 48 };
    this._raf = null;
    this._running = false;
    this._peak = 0;

    this._resize();
    this._resizeObserver = new ResizeObserver(() => this._resize());
    this._resizeObserver.observe(canvas.parentElement || canvas);
  }

  /* ── public ── */

  /** Push a new speed sample (bytes/s). Called once per second from app.js. */
  push(speed) {
    this.data.push({ time: Date.now(), speed });
    if (this.data.length > this.maxPoints) this.data.shift();
    if (speed > this._peak) this._peak = speed;
  }

  /** Start the render loop. */
  start() {
    if (this._running) return;
    this._running = true;
    this._loop();
  }

  /** Stop rendering. */
  stop() {
    this._running = false;
    if (this._raf) cancelAnimationFrame(this._raf);
  }

  /** Clean up. */
  destroy() {
    this.stop();
    this._resizeObserver.disconnect();
  }

  /* ── private ── */

  _resize() {
    const rect = this.canvas.getBoundingClientRect();
    const dpr = window.devicePixelRatio || 1;
    this.canvas.width = rect.width * dpr;
    this.canvas.height = rect.height * dpr;
    this.ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
    this.width = rect.width;
    this.height = rect.height;
  }

  _loop() {
    if (!this._running) return;
    this._draw();
    this._raf = requestAnimationFrame(() => this._loop());
  }

  _draw() {
    const ctx = this.ctx;
    const w = this.width;
    const h = this.height;
    const p = this.padding;
    const plotW = w - p.left - p.right;
    const plotH = h - p.top - p.bottom;

    ctx.clearRect(0, 0, w, h);

    // Determine Y scale
    const maxSpeed = this._autoMax();

    // Grid lines (4 horizontal)
    ctx.strokeStyle = this.gridColor;
    ctx.lineWidth = 1;
    for (let i = 0; i <= 4; i++) {
      const y = p.top + (plotH / 4) * i;
      ctx.beginPath();
      ctx.moveTo(p.left, y);
      ctx.lineTo(w - p.right, y);
      ctx.stroke();

      // Y labels
      const val = maxSpeed - (maxSpeed / 4) * i;
      ctx.fillStyle = this.labelColor;
      ctx.font = '10px system-ui, sans-serif';
      ctx.textAlign = 'right';
      ctx.textBaseline = 'middle';
      ctx.fillText(this._formatSpeed(val), p.left - 6, y);
    }

    // X-axis time labels
    ctx.fillStyle = this.labelColor;
    ctx.font = '9px system-ui, sans-serif';
    ctx.textAlign = 'center';
    ctx.textBaseline = 'top';
    ctx.fillText('60s', p.left, h - p.bottom + 4);
    ctx.fillText('30s', p.left + plotW / 2, h - p.bottom + 4);
    ctx.fillText('now', w - p.right, h - p.bottom + 4);

    if (this.data.length < 2) {
      // Empty state
      ctx.fillStyle = this.labelColor;
      ctx.font = '11px system-ui, sans-serif';
      ctx.textAlign = 'center';
      ctx.textBaseline = 'middle';
      ctx.fillText('Waiting for data…', w / 2, h / 2);
      return;
    }

    // Build points
    const points = [];
    for (let i = 0; i < this.data.length; i++) {
      const x = p.left + (i / (this.maxPoints - 1)) * plotW;
      const y = p.top + plotH - (this.data[i].speed / maxSpeed) * plotH;
      points.push({ x, y });
    }

    // Smooth line (Catmull-Rom → Bezier)
    ctx.beginPath();
    ctx.moveTo(points[0].x, points[0].y);
    for (let i = 0; i < points.length - 1; i++) {
      const p0 = points[i - 1] || points[i];
      const p1 = points[i];
      const p2 = points[i + 1];
      const p3 = points[i + 2] || p2;
      const cp1x = p1.x + (p2.x - p0.x) / 6;
      const cp1y = p1.y + (p2.y - p0.y) / 6;
      const cp2x = p2.x - (p3.x - p1.x) / 6;
      const cp2y = p2.y - (p3.y - p1.y) / 6;
      ctx.bezierCurveTo(cp1x, cp1y, cp2x, cp2y, p2.x, p2.y);
    }

    // Stroke line
    ctx.strokeStyle = this.lineColor;
    ctx.lineWidth = 2;
    ctx.lineJoin = 'round';
    ctx.stroke();

    // Gradient fill
    const grad = ctx.createLinearGradient(0, p.top, 0, p.top + plotH);
    grad.addColorStop(0, this.gradientTop);
    grad.addColorStop(1, this.gradientBottom);
    ctx.lineTo(points[points.length - 1].x, p.top + plotH);
    ctx.lineTo(points[0].x, p.top + plotH);
    ctx.closePath();
    ctx.fillStyle = grad;
    ctx.fill();

    // Dot on the latest point
    const last = points[points.length - 1];
    ctx.beginPath();
    ctx.arc(last.x, last.y, 3.5, 0, Math.PI * 2);
    ctx.fillStyle = this.lineColor;
    ctx.fill();
    ctx.strokeStyle = '#0a0a0f';
    ctx.lineWidth = 1.5;
    ctx.stroke();

    // Current speed overlay
    const currentSpeed = this.data[this.data.length - 1].speed;
    ctx.fillStyle = this.textColor;
    ctx.font = 'bold 13px system-ui, sans-serif';
    ctx.textAlign = 'left';
    ctx.textBaseline = 'top';
    ctx.fillText(this._formatSpeed(currentSpeed) + '/s', p.left + 4, 6);
  }

  _autoMax() {
    let max = 0;
    for (const d of this.data) {
      if (d.speed > max) max = d.speed;
    }
    if (max === 0) return 1024 * 1024; // 1 MB default
    // Round up to a nice number
    const order = Math.pow(10, Math.floor(Math.log10(max)));
    return Math.ceil(max / order) * order * 1.15;
  }

  _formatSpeed(bytes) {
    if (bytes >= 1073741824) return (bytes / 1073741824).toFixed(1) + ' GB';
    if (bytes >= 1048576) return (bytes / 1048576).toFixed(1) + ' MB';
    if (bytes >= 1024) return (bytes / 1024).toFixed(0) + ' KB';
    return bytes.toFixed(0) + ' B';
  }
}
