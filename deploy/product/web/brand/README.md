# Phala Pay brand

The logo is the green dot: a lime square in a near-black tile.

| File | Use |
|---|---|
| [`mark-light.svg`](mark-light.svg) | The mark, on light backgrounds |
| [`mark-dark.svg`](mark-dark.svg) | The mark, on dark backgrounds: the tile gets an edge |
| [`lockup-light.svg`](lockup-light.svg) | The mark and the name, on light backgrounds |
| [`lockup-dark.svg`](lockup-dark.svg) | The mark and the name, on dark backgrounds |
| [`app-icon.svg`](app-icon.svg) | The mark full-bleed, for the Apple touch icon and Android's maskable icon |
| [`og-image.svg`](og-image.svg) | The link preview |

## Construction

- **Grid.** The mark is 32 × 32 units. The tile's corner radius is 8. The dot is 12 × 12 at
  (10, 10), so centred on (16, 16), with a radius of 3. The dot is the tile at 3/8 scale, so both
  have the same corner ratio, and every edge falls on whole pixels at 16 and 32 px.
- **Colour.** The tile is `#0a0a0a`, the site's dark background. The dot is `#cefc5d`, the site's
  `--brand` token `oklch(0.93 0.19 124)`. Use `mark-dark` on any background darker than about
  zinc-600 (`#52525b`).
- **Edge.** On dark backgrounds the tile gets an edge: a ring inside its outline, in white at 15%
  opacity. It is a filled even-odd path, not a stroke, so design tools and browsers draw it alike.
  Each file's ring is about 1 px at the size the file is used at:
  - `mark-dark.svg` and `lockup-dark.svg`: 1 unit, 1 px at their natural 32 px mark (2 px in the
    link preview's 64 px mark);
  - `public/favicon.svg`, which shows the edge in a dark browser theme: 2 units, 1 px in a 16 px
    tab, on whole pixels at 1x and 2x;
  - the site's 24 px mark: 4/3 units.
- **Lockup.** The name "Phala Pay" is set in Geist SemiBold (600) at 64/3 units, the site's
  `tracking-tight` (−0.025em), and converted to outlines, so the lockup renders the same anywhere,
  with or without the font. The P's ink starts 10 units from the tile: the same as the dot's inset.
  The cap height is centred on the mark. The name is `#0a0a0a` on light backgrounds and `#fafafa`
  on dark ones.
- **Clear space and size.** Keep 8 units clear around the mark or lockup. Use the mark at 16 px or
  larger, and the lockup with a mark of 20 px or larger.

The site draws the same geometry inline (`Lockup` in `src/Site.tsx`), with live Geist text.

## Rendering

`pnpm run brand` renders `public/favicon-32.png`, the manifest's icons, the Apple touch icon, and
`public/og-image.png` from these SVGs and `public/favicon.svg`; commit the PNGs. The icons that
platforms mask are full-bleed, and the dot stays well inside Android's maskable safe zone, the
centred circle of 80% diameter.
