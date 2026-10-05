# dl-nzb site

Static site for [dl-nzb](https://github.com/zephleggett/dl-nzb). No build step,
no framework, no webfonts, no CDN.

```
site/
  index.html        the page
  privacy.html      privacy policy (linked from the App Store listings)
  favicon.png       64px, from docs/images/icon.png
  css/styles.css    base16-eighties; the accent is base0C cyan
  js/replay.js      the terminal replay: one run that mirrors the CLI's output
  js/app.js         nav highlight, copy buttons, replay controls, media fallback
  media/            demo videos; see media/README.md
  .assetsignore     keeps the READMEs off the deployed site
```

## Preview

```bash
python3 -m http.server 8787 --directory site   # http://127.0.0.1:8787
npx wrangler dev                               # from the repo root; uses wrangler.jsonc
```

`#t=<ms>` shows one frame of the replay, e.g. `index.html#t=12000` (repairing).

## Deploy

Cloudflare static assets, from the repo root: `npx wrangler deploy`.

## Notes

- Colours: base16-eighties, no orange. base03 is for borders only; secondary
  text uses base04 (5.2:1 on the ground).
- Replay: the lines follow `src/cli/observer.rs`, `src/progress.rs` and
  `src/main.rs`. Update `replay.js` when that output changes. The content is
  Sintel, the Blender open movie.
- Reduced motion: the replay shows a still of the finished run; **play** runs
  it once. The replay always has pause and restart buttons.
- TODOs: the TestFlight code (`XXXXXXXX`, twice in `index.html`), the demo
  media and the `og:image`.
