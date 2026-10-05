/* replay.js: a sped-up replay of one dl-nzb command-line run.
   The lines and bars follow the real output: the banner (src/main.rs
   print_banner), the spinners, bars and status lines (src/cli/observer.rs,
   src/progress.rs), then the stage lines and summary (src/main.rs
   print_stage_lines, print_final_summary). Sizes use the same units: lines
   use human_bytes (1000-based "GB"), bars use indicatif ("GiB").
   The content is Sintel, the Blender open movie (CC BY 3.0).
   One rAF loop drives a virtual clock; the screen auto-scrolls to the bottom. */
(function () {
  "use strict";
  const $ = (id) => document.getElementById(id);
  const screen = $("screen"), scrBuf = $("scrBuf");
  if (!screen || !scrBuf) return;
  const clockEl = $("replayClock");

  /* ── the run ─────────────────────────────────────────────────────────── */
  const TITLE = "Sintel.2010.2160p";
  const DATA = 4.30 * 1024 ** 3;          // RAR volumes, as the bar counts them
  const RECOVERY = 64.5 * 1024 ** 2;      // PAR2 volumes fetched for the repair
  const FILES = 6, RECOVERY_FILES = 3, FAILED = 47;
  const VOLUMES = [
    ["Sintel.2010.2160p.part1.rar", "1 GB"],
    ["Sintel.2010.2160p.part2.rar", "1 GB"],
    ["Sintel.2010.2160p.part3.rar", "1 GB"],
    ["Sintel.2010.2160p.part4.rar", "1 GB"],
    ["Sintel.2010.2160p.part5.rar", "617.1 MB"],
  ];
  const SPIN = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
  const TXT = "︎";                   // keep ⚠ and ℹ as text, not emoji

  /* timeline, in ms of virtual clock */
  const T = { banner: 250, check: 900, avail: 2000, recovery: 8600, verify: 9600,
              repair: 11200, extract: 13000, done: 14800, fade: 20500, loop: 21500 };

  /* ── helpers ─────────────────────────────────────────────────────────── */
  const clamp = (v, a, b) => (v < a ? a : v > b ? b : v);
  const ease = (x) => 1 - Math.pow(1 - clamp(x, 0, 1), 1.6);
  const esc = (s) => String(s).replace(/[&<>]/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;" }[c]));
  const sp = (cls, t) => `<span class="${cls}">${esc(t)}</span>`;
  const padL = (s, n) => String(s).padStart(n, " ");
  const padR = (s, n) => String(s).padEnd(n, " ");
  const frac = (t, a, b) => clamp((t - a) / (b - a), 0, 1);
  /* indicatif's binary bytes, e.g. "1.81 GiB" */
  function ibytes(b) {
    const u = ["B", "KiB", "MiB", "GiB"]; let i = 0;
    while (b >= 1024 && i < u.length - 1) { b /= 1024; i++; }
    return i === 0 ? Math.round(b) + " B" : b.toFixed(2) + " " + u[i];
  }
  /* ui::format_duration */
  function dur(s) { s = Math.max(0, Math.round(s)); if (s < 60) return s + "s"; const m = (s / 60) | 0, r = s % 60; return r ? `${m}m ${r}s` : `${m}m`; }
  const wander = (t) => Math.sin(t / 700) * 0.5 + Math.sin(t / 271 + 1.3) * 0.32 + Math.sin(t / 113 + 2.1) * 0.18;
  const child = (body, last) => "  " + sp("t-deco", last ? "└─" : "├─") + " " + body;
  const ok = (text) => sp("t-green", "✓ " + text);
  const warn = (text) => sp("t-yellow", "⚠" + TXT + " " + text);
  const info = (text, cls) => sp("t-dim", "ℹ" + TXT) + " " + sp(cls || "t-cyan", text);

  /* ── persistent lines ────────────────────────────────────────────────── */
  const CMD = sp("t-green", "$") + " " + sp("t-ink", "dl-nzb " + TITLE + ".nzb");
  const BANNER = sp("t-head", TITLE) + sp("t-dim", "  ·  4.9 GB · 14 files");
  const EVENTS = [
    { t: T.banner,   lines: ["", BANNER] },
    { t: T.avail,    lines: [child(warn("99.2% of data available; 296 MB recovery present — PAR2 repair likely."))] },
    { t: T.recovery, lines: [child(warn(`Downloaded 4.6 GB (${FAILED} articles failed) at 58.1 MiB/s`)),
                             child(sp("t-yellow", "↻ Missing/corrupt data — fetching PAR2 recovery for repair…"))] },
    { t: T.verify,   lines: [child(ok(`Downloaded 67.6 MB (${RECOVERY_FILES} files) at 52.4 MiB/s`))] },
  ];
  const SUMMARY = [
    child(ok(`PAR2 verified (repaired ${FAILED} blocks)`)),
    child(ok("Extracted 1 archive")),
    "",
    sp("t-green", "✓ Complete"),
    ...VOLUMES.map(([name, size]) => child(sp("t-green", "✓") + "  " + sp("t-ink", name) + "  " + sp("t-cyan", size))),
    child(info("downloads/" + TITLE, "t-blue")),
    child(sp("t-dim", "ℹ" + TXT) + " " + sp("t-cyan", "4.9 GB") + " in " + sp("t-cyan", "1m 19s"), true),
  ];

  /* ── the bar reflows with the screen width, like indicatif's {wide_bar} ── */
  let cols = 80;
  const ruler = document.createElement("span");
  ruler.textContent = "0".repeat(80);
  ruler.style.cssText = "position:absolute;visibility:hidden;white-space:pre;left:-9999px;";
  function measure() {
    screen.appendChild(ruler);
    const cw = ruler.getBoundingClientRect().width / 80;
    screen.removeChild(ruler);
    const style = getComputedStyle(screen);
    const inner = screen.clientWidth - parseFloat(style.paddingLeft) - parseFloat(style.paddingRight);
    cols = Math.max(30, Math.floor(inner / (cw || 8)) - 1);
  }
  function bar(f, fill, W) {
    f = clamp(f, 0, 1);
    if (f >= 1) return sp(fill, "━".repeat(W));
    const n = Math.floor(f * W);
    return sp(fill, "━".repeat(n) + "╸") + sp("bar-rest", "─".repeat(Math.max(0, W - n - 1)));
  }
  const pct = (f) => sp("t-head", padL(Math.round(clamp(f, 0, 1) * 100), 3) + "%");

  /* [bar] pct% bytes/total │ speed │ ETA (files) */
  function downloadBar(t, start, end, total, files, base) {
    const f = ease(frac(t, start, end)), done = f * total;
    const speed = clamp(base + wander(t) * 18, 8, 96);
    const eta = (total - done) / 1024 ** 2 / speed;
    const msg = `(${Math.min(files, Math.floor(f * files))}/${files})`;
    const narrow = 2 + 6 + 3 + 12 + 3 + 10 + 1 + msg.length;
    const wide = cols - (narrow + 22) >= 10;    // drop the bytes column before the bar gets tiny
    const fixed = narrow + (wide ? 22 : 0);
    const W = clamp(cols - fixed, 6, 40);
    let line = sp("t-dim", "[") + bar(f, "t-cyan", W) + sp("t-dim", "]") + " " + pct(f) + " ";
    if (wide) line += sp("t-cyan", padL(ibytes(done), 10)) + sp("t-dim", "/" + padR(ibytes(total), 10)) + " ";
    line += sp("t-dim", "│") + " " + sp("t-green", padL(speed.toFixed(2), 6) + " MiB/s") + " " + sp("t-dim", "│") + " ";
    line += sp("t-yellow", "ETA " + padL(dur(eta), 6)) + " " + sp("t-cyan", msg);
    return line;
  }
  /* [bar] pct% msg */
  function phaseBar(f, cls, msg) {
    const W = clamp(cols - (2 + 6 + msg.length), 6, 48);
    return sp("t-dim", "[") + bar(f, cls, W) + sp("t-dim", "]") + " " + pct(f) + " " + sp(cls, msg);
  }
  const spinner = (t, msg) => sp("t-cyan", SPIN[Math.floor(t / 80) % SPIN.length]) + " " + sp("t-ink", msg);

  function live(t) {
    if (t < T.banner) return null;
    if (t < T.check) return spinner(t, "Connecting to server…");
    if (t < T.avail) return spinner(t, "Checking article availability…");
    if (t < T.recovery) return downloadBar(t, T.avail, T.recovery, DATA, FILES, 58);
    if (t < T.verify) return downloadBar(t, T.recovery, T.verify, RECOVERY, RECOVERY_FILES, 52);
    if (t < T.repair) {
      const f = frac(t, T.verify, T.repair);
      let msg = "Verifying...";
      if (f < 0.15) msg += " (Loading recovery data)";
      else if (f < 0.3) msg += " (Scanning files)";
      else {
        const dmg = Math.round(FAILED * ease((f - 0.3) / 0.7));
        if (dmg > 0) msg += ` (${dmg} damaged block${dmg === 1 ? "" : "s"})`;
      }
      return phaseBar(ease(f), "t-yellow", msg);
    }
    if (t < T.extract) return phaseBar(ease(frac(t, T.repair, T.extract)), "t-magenta", `Repairing... (${FAILED} damaged blocks)`);
    if (t < T.done) return phaseBar(ease(frac(t, T.extract, T.done)), "t-green", "Extracting...");
    return null;
  }

  /* run time shown on the clock, in seconds */
  function runSecs(t) {
    if (t < T.banner) return 0;
    if (t < T.avail) return frac(t, T.banner, T.avail) * 2;
    if (t < T.recovery) return 2 + frac(t, T.avail, T.recovery) * 75;
    if (t < T.verify) return 77 + frac(t, T.recovery, T.verify) * 2;
    if (t < T.done) return 79 + frac(t, T.verify, T.done) * 17;
    return 96;
  }

  /* ── render the whole buffer from the virtual clock ──────────────────── */
  let lastBuf = "";
  function render(t) {
    screen.style.opacity = t >= T.fade ? (1 - ease(frac(t, T.fade, T.loop)) * 0.85).toFixed(3) : "1";
    const out = [CMD];
    for (const e of EVENTS) if (t >= e.t) out.push(...e.lines);
    if (t >= T.done) out.push(...SUMMARY);
    else { const l = live(t); if (l !== null) out.push(l); }
    const html = out.join("\n");
    if (html !== lastBuf) { lastBuf = html; scrBuf.innerHTML = html; screen.scrollTop = screen.scrollHeight; }
    if (clockEl) { const s = runSecs(t) | 0; clockEl.textContent = padL((s / 60) | 0, 2).replace(/ /g, "0") + ":" + String(s % 60).padStart(2, "0"); }
  }

  /* ── clock and loop ──────────────────────────────────────────────────── */
  const reduced = matchMedia("(prefers-reduced-motion: reduce)").matches;
  const FINAL = T.done + 1100;              // a still of the finished run
  let vclock = 0, last = 0, raf = 0, running = false, userPaused = false, onScreen = true;
  const listeners = [];
  const notify = () => listeners.forEach((fn) => fn(running));

  function tick(now) {
    if (!running) return;
    // dt from the rAF timestamp only; clamp so a stall or a backwards step can't jump the clock
    if (last === 0) last = now;
    let next = vclock + Math.max(0, Math.min(now - last, 64));
    last = now;
    if (reduced && next >= FINAL) { vclock = FINAL; render(vclock); stop(); return; }  // play once, no loop
    vclock = next % T.loop;
    render(vclock);
    raf = requestAnimationFrame(tick);
  }
  function start() { if (running) return; running = true; last = 0; raf = requestAnimationFrame(tick); notify(); }
  function stop() { if (!running) return; running = false; cancelAnimationFrame(raf); notify(); }
  function seek(ms) { vclock = ((ms % T.loop) + T.loop) % T.loop; lastBuf = ""; render(vclock); }

  window.Replay = {
    play() { userPaused = false; if (reduced && vclock >= FINAL) seek(0); start(); },
    pause() { userPaused = true; stop(); },
    toggle() { running ? this.pause() : this.play(); },
    restart() { seek(0); this.play(); },
    seek,
    onChange(fn) { listeners.push(fn); fn(running); },
  };

  measure();
  let rz;
  addEventListener("resize", () => { cancelAnimationFrame(rz); rz = requestAnimationFrame(() => { measure(); lastBuf = ""; render(vclock); }); }, { passive: true });

  render(0);
  const hash = location.hash.match(/t=(\d+)/);   // dev: #t=12000 shows that frame
  if (hash) seek(+hash[1]);
  else if (reduced) seek(FINAL);                  // reduced motion: a still; the play button runs it once
  else {
    start();
    // pause while off-screen; resume on return unless the reader paused it
    if ("IntersectionObserver" in window) {
      new IntersectionObserver((es) => es.forEach((e) => {
        onScreen = e.isIntersecting;
        if (onScreen && !userPaused) start(); else if (!onScreen) stop();
      }), { threshold: 0.08 }).observe($("replay"));
    }
  }
})();
