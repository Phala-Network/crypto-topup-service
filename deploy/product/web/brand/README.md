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

- **Grid.** The mark is 32 × 32 units. The tile's corner radius is 8. The dot is 12 × 12, centred
  (at 10, 10), with a radius of 3. The dot is the tile at 3/8 scale, so both have the same corner
  ratio, and every edge falls on whole pixels at 16 and 32 px.
- **Colour.** The tile is `#0a0a0a`, the site's dark background. The dot is `#cefc5d`, the site's
  `--brand` token `oklch(0.93 0.19 124)`. On dark backgrounds the tile gets an edge: a 1 px
  hairline inside it, in white at 15% opacity, at any size (a non-scaling 2 px stroke on the
  tile's outline, clipped to the tile).
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
