/* app.js: the hero video's play/pause, and the copy button.
   Without JS the page still works: the hero video keeps its native controls
   and the copy button stays hidden. */
(function () {
  "use strict";
  const $ = (s, r = document) => r.querySelector(s);
  const $$ = (s, r = document) => Array.from(r.querySelectorAll(s));

  /* demo videos: our own play/pause button replaces the native controls.
     The hero (data-autoplay) plays muted on a loop, unless the visitor
     prefers reduced motion; the others wait for play. */
  $$(".ctrl[data-video]").forEach((btn) => {
    const video = $(btn.dataset.video);
    if (!video) return;
    const still = window.matchMedia("(prefers-reduced-motion: reduce)");
    const label = () => {
      const playing = !video.paused;
      btn.textContent = playing ? "pause" : "play";
      const name = btn.dataset.name || "demo";
      btn.setAttribute("aria-label", (playing ? "Pause the " : "Play the ") + name);
    };
    video.removeAttribute("controls");
    btn.hidden = false;
    video.addEventListener("play", label);
    video.addEventListener("pause", label);
    btn.addEventListener("click", () => (video.paused ? video.play().catch(() => {}) : video.pause()));
    still.addEventListener?.("change", () => { if (still.matches) video.pause(); });
    if (video.hasAttribute("data-autoplay") && !still.matches) {
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
