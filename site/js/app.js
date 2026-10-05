/* app.js: nav highlight, copy buttons, replay controls, and demo media that
   falls back to a flat placeholder until the files exist. */
(function () {
  "use strict";
  const $ = (s, r = document) => r.querySelector(s);
  const $$ = (s, r = document) => Array.from(r.querySelectorAll(s));

  /* highlight the nav link for the section in view */
  const navLinks = new Map($$(".topbar__nav a").map((a) => [a.dataset.nav, a]));
  if (navLinks.size && "IntersectionObserver" in window) {
    const spy = new IntersectionObserver((entries) => {
      entries.forEach((e) => {
        if (!e.isIntersecting) return;
        const id = e.target.dataset.section;
        navLinks.forEach((a, k) => {
          a.classList.toggle("is-active", k === id);
          if (k === id) a.setAttribute("aria-current", "true"); else a.removeAttribute("aria-current");
        });
      });
    }, { rootMargin: "-45% 0px -50% 0px" });
    $$("[data-section]").forEach((s) => spy.observe(s));
  }

  /* copy buttons */
  $$(".copy[data-copy]").forEach((btn) => {
    btn.addEventListener("click", () => {
      const el = $(btn.dataset.copy);
      if (!el || !navigator.clipboard) return;
      navigator.clipboard.writeText(el.textContent.trim()).then(() => {
        btn.classList.add("is-done");
        setTimeout(() => btn.classList.remove("is-done"), 1300);
      }).catch(() => {});
    });
  });

  /* replay controls */
  const toggle = $('[data-act="toggle"]');
  if (window.Replay) {
    if (toggle) {
      window.Replay.onChange((running) => {
        toggle.textContent = running ? "pause" : "play";
        toggle.setAttribute("aria-label", running ? "Pause replay" : "Play replay");
      });
      toggle.addEventListener("click", () => window.Replay.toggle());
    }
    const restart = $('[data-act="restart"]');
    if (restart) restart.addEventListener("click", () => window.Replay.restart());
  }

  /* demo media. No poster yet: hide the empty player and show the flat placeholder.
     No mp4 playback: swap in the animated webp (fetched only then). */
  $$(".demo video").forEach((video) => {
    const fig = video.closest(".demo");
    const missing = () => fig.classList.add("is-missing");
    const toImage = () => {
      const src = video.dataset.fallback;
      if (!src || !video.isConnected) return;
      const img = new Image();
      img.className = "demo__img";
      img.alt = video.getAttribute("aria-label") || "";
      img.onerror = missing;
      img.src = src;
      video.replaceWith(img);
    };
    const poster = video.getAttribute("poster");
    if (poster) { const probe = new Image(); probe.onerror = missing; probe.src = poster; }
    if (!video.canPlayType || video.canPlayType("video/mp4") === "") toImage();
    const source = video.querySelector("source");
    if (source) source.addEventListener("error", toImage);
  });
})();
