# dl-nzb site

Static site for [dl-nzb](https://github.com/zephleggett/dl-nzb), at
https://dl-nzb.com/. No build step, no framework, no webfonts, no CDN.
One column: what it does, the Mac app as wide as the page, then the
iPhone and iPad app and the command line, each shown as it looks.

```
site/
  index.html        the page: hero and Mac demo; download, repair, extract;
                    iPhone and iPad; command line
  privacy.html      privacy policy (linked from the App Store listings)
  favicon.png       64px, from docs/images/icon.png
  css/styles.css    base16-eighties; the accent is base09 orange
  js/app.js         the demos' play/pause, and the copy button
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
`wrangler.jsonc` attaches dl-nzb.com and www.dl-nzb.com as custom domains
and keeps the workers.dev address on.

## Notes

- Colours: base16-eighties, with orange as the accent, as in the app icon.
  base03 is for lines only; secondary text uses base04 (5.2:1 on the
  ground).
- The Mac demo plays muted on a loop, except with reduced motion, where it
  waits for **play**. The iPhone demo always waits for **play**. Without JS
  both keep their native controls.
- The version under the hero's buttons is set by hand. Update it with each
  release.
