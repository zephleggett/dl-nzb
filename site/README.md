# dl-nzb site

Static site for [dl-nzb](https://github.com/zephleggett/dl-nzb), at
https://dl-nzb.com/. No build step, no framework, no webfonts, no CDN.
Laid out like the jetlink page: a hero, then short plain sections.

```
site/
  index.html        the page: hero (Mac demo), what you need, get the app,
                    what it does, command line, latest releases
  privacy.html      privacy policy (linked from the App Store listings)
  favicon.png       64px, from docs/images/icon.png
  css/styles.css    base16-eighties; the accent is base0C cyan
  js/app.js         the hero video's play/pause, and the copy button
  media/            icon, demo videos, TestFlight badge, og.png; see media/README.md
  .assetsignore     keeps the READMEs off the deployed site
```

## Preview

```bash
python3 -m http.server 8787 --directory site   # http://127.0.0.1:8787
npx wrangler dev                               # from the repo root; uses wrangler.jsonc
```

## Deploy

Cloudflare static assets, from the repo root: `npx wrangler deploy`.

## Notes

- Colours: base16-eighties, no orange. base03 is for lines only; secondary
  text uses base04 (5.2:1 on the ground).
- The hero video plays muted on a loop, except with reduced motion, where it
  waits for **play**. Without JS it keeps its native controls.
- Latest releases: copied by hand from `CHANGELOG.md`. Update it with each
  release.
- TODOs: the TestFlight code (`XXXXXXXX`, twice in `index.html`), and the
  v0.8.0 date if the tag lands on another day.
