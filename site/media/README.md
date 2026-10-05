# site/media

Demo media for the apps section of `index.html`, copied from `docs/images`. If a
poster is missing, the demo shows a flat "demo video coming soon" box instead
(`js/app.js` hides the player when the poster can't load).

After re-recording a demo, copy it here again:

```bash
cp docs/images/mac-demo.mp4 docs/images/mac-demo.webp site/media/
cp docs/images/iphone-demo.mp4 docs/images/iphone-demo.webp site/media/
cp docs/images/mac-demo-poster.png docs/images/iphone-demo-poster.png site/media/
```

| File | Used as |
| --- | --- |
| `mac-demo.mp4`, `iphone-demo.mp4` | the `<video>` source |
| `mac-demo-poster.png`, `iphone-demo-poster.png` | the poster; the page checks for it |
| `mac-demo.webp`, `iphone-demo.webp` | fallback when the mp4 can't play; fetched only then |
| `og.png` | link preview, 1200×630; then uncomment the `og:image` tags in `index.html` |

Use legitimate names only in anything on screen: Blender open movies (Sintel,
Big Buck Bunny) or Linux ISOs.

The Mac frame is 16:10 and the iPhone frame is square; video is letterboxed to
fit, so other sizes work too.
