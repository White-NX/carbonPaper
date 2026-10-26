# Release banner workflow

Render CarbonPaper release headers locally on Windows with
[`render-version-banner.ps1`](../scripts/render-version-banner.ps1). The output
is **1910 × 395 pixels**, matching the actual banner attachment in
[v0.8.5](https://github.com/White-NX/carbonPaper/releases/tag/v0.8.5). The
renderer uses Windows System.Drawing and installed fonts; it needs no additional
image-generation service or project dependencies.

## Inputs and layout

Keep the supplied background at `.version-banner.local/bannerBackground.png`.
That directory is ignored by Git. Keep each release's reproducible text, crop and
palette recipe in `docs/releases/v<version>.banner.json`; the
[v0.9.1 recipe](releases/v0.9.1.banner.json) is the starting point.

- `version` must match the requested release version. The script reads
  `package.json` when `-Version` is omitted; it accepts a leading `v`.
- `codename` follows the current release series: `Taeka` for v0.9.x. An empty
  string omits the codename. `category` is a short label such as `General Update`;
  `headline` describes the release's main capability in one short English line.
- `background` is a path relative to the repository root, or an absolute path.
  The image covers the canvas without stretching. `focus.x` and `focus.y`, each
  from 0 to 1, position the crop within the excess image area.
- `font` names an installed family. The Taeka recipe uses `Segoe UI` and ink,
  teal and ochre colors drawn from the supplied background. All four colors use
  `#RRGGBB`.

The layout keeps the label, separator, version and headline aligned at x=146.
Text occupies at most 1100 pixels, leaving the right-hand artwork visible. The
renderer reduces overly wide text within a limited font-size range and rejects
text that still does not fit. It renders at twice the final size and downsamples
to improve image and text edges.

## Render and review

Run from the repository root after creating the release's JSON recipe:

```powershell
powershell -NoProfile -ExecutionPolicy Bypass -File scripts/render-version-banner.ps1 -Version 0.9.1
```

For a different background, recipe or destination:

```powershell
powershell -NoProfile -ExecutionPolicy Bypass -File scripts/render-version-banner.ps1 -Version 0.9.1 -ConfigPath docs/releases/v0.9.1.banner.json -BackgroundPath .version-banner.local/bannerBackground.png -OutputPath .version-banner.local/output/v0.9.1-banner.png
```

The default output is `.version-banner.local/output/v<version>-banner.png`.
Its adjacent `.render.json` records the input and output SHA-256 hashes, crop,
font, fitted text sizes and dimensions. It contains local absolute paths and
stays with the ignored working files. To reproduce an older release after a
version bump, pass that release's `-Version` explicitly.

Inspect the PNG at full size and around 960 pixels wide. Check all text, the
1910 × 395 dimensions, readability against the artwork, crop, and right-hand
decoration. If needed, shorten the headline, adjust the focus or change colors,
then rerender. Functional screenshots are prepared separately by the maintainer.

After visual review, retain the final PNG at
`docs/releases/images/v<version>-banner.png` and use its raw GitHub URL in the
release body. Keep the original background local. The
[v0.9.1 notes](releases/v0.9.1.md) demonstrate the header and screenshot comments.
Generating a banner does not create tags or publish a GitHub release.
