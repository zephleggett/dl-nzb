# site/media

Media for `index.html`, copied from `docs/images`. After re-recording a demo,
copy it here again:

```bash
cp docs/images/mac-demo.mp4 docs/images/mac-demo-poster.png site/media/
cp docs/images/iphone-demo.mp4 docs/images/iphone-demo-poster.png site/media/
```

| File | Used as |
| --- | --- |
| `icon.png` | the icon beside the name in the hero (256px, `docs/images/icon.png`) |
| `mac-demo.mp4`, `mac-demo-poster.png` | the hero video and its poster |
| `iphone-demo.mp4`, `iphone-demo-poster.png` | the iPhone demo in "get the app" |
| `testflight-badge.svg` | the TestFlight badge (`docs/images/testflight-badge.svg`) |
| `og.png` | link preview, 1200×630: the Mac poster scaled to 630 high, centred |

Rebuild `og.png` after a new Mac poster:

```bash
python3 - <<'EOF'
from PIL import Image, ImageStat
src = Image.open('site/media/mac-demo-poster.png').convert('RGB')
s = src.resize((round(src.width * 630 / src.height), 630), Image.LANCZOS)
edge = lambda x: ImageStat.Stat(src.crop((x, 0, x + 8, src.height))).mean
bg = tuple(round((a + b) / 2) for a, b in zip(edge(0), edge(src.width - 8)))
card = Image.new('RGB', (1200, 630), bg)
card.paste(s, ((1200 - s.width) // 2, 0))
card.save('site/media/og.png', optimize=True)
EOF
```

Use legitimate names only in anything on screen: Blender open movies (Sintel,
Big Buck Bunny, Tears of Steel) or Linux ISOs.
