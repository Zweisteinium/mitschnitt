<script lang="ts">
  /**
   * Stereo level meter on a canvas. The engine reports peak and RMS every 100 ms; each new value is reached in a
   * linear 80 ms glide, as the meter always moved. Cells and gaps are laid out in whole device pixels, so they stay
   * crisp at any display scaling.
   */
  import { onMount } from "svelte";

  let { rms, peak }: { rms: number[]; peak: number[] } = $props();

  const GLIDE_MS = 80;
  /** CSS px: cell pitch, gap, bar height, gap between bars, peak line overhang and width. */
  const PITCH = 8, GAP = 2, BAR = 6, BAR_GAP = 3, OVERHANG = 1, LINE = 2;
  const STOPS: [number, number[]][] = [[0, [14, 95, 115]], [0.38, [34, 211, 238]], [0.64, [167, 139, 250]], [0.8, [244, 63, 94]], [1, [185, 28, 28]]];

  /** IEC 60268-18 style: -20 dB sits at half. */
  function position(x: number): number {
    const db = 20 * Math.log10(Math.max(x, 1e-6));
    const curve: [number, number][] = [[-70, 0], [-60, 0.025], [-50, 0.075], [-40, 0.15], [-30, 0.3], [-20, 0.5], [0, 1]];
    if (db <= -70) return 0;
    if (db >= 0) return 1;
    const i = curve.findIndex(([d]) => d >= db);
    const [[d0, m0], [d1, m1]] = [curve[i - 1], curve[i]];
    return m0 + ((db - d0) / (d1 - d0)) * (m1 - m0);
  }

  function color(x: number): string {
    const j = STOPS.findIndex(([at]) => at >= x);
    const [[a, ca], [b, cb]] = [STOPS[j - 1], STOPS[j]];
    const t = (x - a) / (b - a);
    return `rgb(${ca.map((c, k) => Math.round(c + (cb[k] - c) * t)).join(" ")})`;
  }

  /** A value gliding from `from` to `to`, started at `at`. */
  type Glide = { from: number; to: number; at: number };
  const value = (g: Glide, now: number) => g.from + (g.to - g.from) * Math.min(1, (now - g.at) / GLIDE_MS);
  const glide = (g: Glide, to: number, now: number): Glide => ({ from: value(g, now), to, at: now });

  let canvas: HTMLCanvasElement;
  let width = $state(0);
  const still: Glide = { from: 0, to: 0, at: 0 };
  let levels = [still, still];
  let peaks = [still, still];
  let frame = 0;
  let palette: string[] = [];
  let unlit = "";

  $effect(() => {
    const now = performance.now();
    levels = levels.map((g, c) => glide(g, position(rms[c] ?? 0), now));
    peaks = peaks.map((g, c) => glide(g, position(peak[c] ?? 0), now));
    if (!frame) frame = requestAnimationFrame(tick);
  });

  function tick(now: number) {
    frame = 0;
    draw(now);
    if ([...levels, ...peaks].some((g) => now - g.at < GLIDE_MS)) frame = requestAnimationFrame(tick);
  }

  function draw(now = performance.now()) {
    const dpr = window.devicePixelRatio || 1;
    const px = (v: number) => Math.max(1, Math.round(v * dpr));
    const [bar, barGap, overhang, line, gap] = [px(BAR), px(BAR_GAP), px(OVERHANG), px(LINE), px(GAP)];
    const w = Math.round(width * dpr);
    const h = overhang * 2 + bar * 2 + barGap;
    if (!w) return;
    if (canvas.width !== w || canvas.height !== h) {
      canvas.width = w;
      canvas.height = h;
    }
    canvas.style.height = `${h / dpr}px`;
    const ctx = canvas.getContext("2d")!;
    ctx.clearRect(0, 0, w, h);

    const cells = Math.max(16, Math.round((w + gap) / px(PITCH)));
    const step = (w + gap) / cells;
    if (palette.length !== cells) palette = Array.from({ length: cells }, (_, i) => color((i + 0.5) / cells));
    unlit ||= getComputedStyle(canvas).getPropertyValue("--faint").trim() || "#1b2430";
    for (const c of [0, 1]) {
      const y = overhang + c * (bar + barGap);
      const lit = Math.round(value(levels[c], now) * w);
      for (let i = 0; i < cells; i++) {
        const x0 = Math.round(i * step);
        const x1 = Math.round((i + 1) * step) - gap;
        ctx.fillStyle = unlit;
        ctx.fillRect(x0, y, x1 - x0, bar);
        if (x0 < lit) {
          ctx.fillStyle = palette[i];
          ctx.fillRect(x0, y, Math.min(x1, lit) - x0, bar);
        }
      }
      const p = value(peaks[c], now);
      if (p > 0) {
        const x = Math.min(w - line, Math.max(0, Math.round(p * w) - line));
        ctx.fillStyle = "#e6f6fb";
        ctx.fillRect(x, y - overhang, line, bar + overhang * 2);
      }
    }
  }

  onMount(() => {
    // Display scaling can change while open (moving the window to another screen); a query matches one scale only.
    let scale: MediaQueryList;
    const watch = () => {
      scale = window.matchMedia(`(resolution: ${window.devicePixelRatio}dppx)`);
      scale.addEventListener("change", rescale, { once: true });
    };
    const rescale = () => {
      draw();
      watch();
    };
    watch();
    return () => {
      scale.removeEventListener("change", rescale);
      cancelAnimationFrame(frame);
    };
  });

  $effect(() => {
    if (width) draw();
  });
</script>

<div class="meter" bind:clientWidth={width}>
  <canvas bind:this={canvas} aria-hidden="true"></canvas>
</div>

<style>
  .meter {
    margin-top: 16px;
  }

  canvas {
    display: block;
    width: 100%;
  }
</style>
