/* app.js: the hero video's play/pause, and the copy button.
   Without JS the page still works: the hero video keeps its native controls
   and the copy button stays hidden. */
(function () {
  "use strict";
  const $ = (s, r = document) => r.querySelector(s);
  const $$ = (s, r = document) => Array.from(r.querySelectorAll(s));

  /* hero video: plays muted on a loop, unless the visitor prefers reduced
     motion. Our own play/pause button replaces the native controls. */
  $$(".ctrl[data-video]").forEach((btn) => {
    const video = $(btn.dataset.video);
    if (!video) return;
    const still = window.matchMedia("(prefers-reduced-motion: reduce)");
    const label = () => {
      const playing = !video.paused;
      btn.textContent = playing ? "pause" : "play";
      btn.setAttribute("aria-label", playing ? "Pause the Mac demo" : "Play the Mac demo");
    };
    video.removeAttribute("controls");
    btn.hidden = false;
    video.addEventListener("play", label);
    video.addEventListener("pause", label);
    btn.addEventListener("click", () => (video.paused ? video.play().catch(() => {}) : video.pause()));
    still.addEventListener?.("change", () => { if (still.matches) video.pause(); });
    if (!still.matches) {
      video.preload = "auto";
      video.play().catch(() => {});
    }
    label();
  });

  /* copy button */
  $$(".copy[data-copy]").forEach((btn) => {
    if (!navigator.clipboard) return;
    btn.hidden = false;
    btn.addEventListener("click", () => {
      const el = $(btn.dataset.copy);
      if (!el) return;
      navigator.clipboard.writeText(el.textContent.trim()).then(() => {
        btn.classList.add("is-done");
        setTimeout(() => btn.classList.remove("is-done"), 1300);
      }).catch(() => {});
    });
  });
})();
