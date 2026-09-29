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
  - `mark-dark.svg`, `lockup-dark.svg` and the site's lockup: 1 unit, 1 px at their natural 32 px
    mark (2 px in the link preview's 64 px mark);
  - `public/favicon.svg`, which shows the edge in a dark browser theme: 2 units, 1 px in a 16 px
    tab, on whole pixels at 1x and 2x.
- **Lockup.** The lockup is [Phala's logo](https://phala.com/home/logo.svg) with our mark in
  place of Phala's: it keeps that logo's units, a 48-unit mark and 16-unit caps centred on it, 10.67
  units away. The files draw the mark at 1.5 times its 32-unit grid, and display at 128 × 32, so
  the mark is its natural 32 px. The name is `#0a0a0a` on light backgrounds and `#fafafa` on dark
  ones.
- **Clear space and size.** Keep 8 units of the mark's grid clear around the mark or lockup. Use the
  mark at 16 px or larger, and the lockup with a mark of 24 px or larger.

The site draws the same SVG inline (`Lockup` in `src/Site.tsx`), 32 px high.

## Lettering

The name, PHALA PAY, is set in the lettering of [Phala's logo](https://phala.com/home/logo.svg):
Phala's own brand, used by a Phala product. The letters are outlines, not a font, so nothing is
embedded or licensed separately.

- **P, H, A, L.** Phala's outlines, copied verbatim from `logo.svg`. PHALA keeps their
  positions, so it is Phala's wordmark exactly; the P and A of PAY are the same outlines, moved
  along the baseline (`translate`).
- **Y.** Phala's logo has no Y, so it is built from the A. Its arms are the A's legs turned 180°,
  the right leg becoming the left arm, so they have the A's angles (23.1° and 23.5° from vertical
  outside, 22.0° and 22.2° inside), weights (3.87 and 3.73 units across the cut) and taper, and end
  in the A's feet: flat cuts, now on the cap line. The stem is 3.61 units, the mean of the stems of
  P, H and L, and ends flat on the baseline. The arms meet the stem, on average, at the underside
  of the H's bar, 4.8 units up, which puts the crotch at 9.01 units, just above the middle.
- **Spacing.** PHALA keeps Phala's spacing and kerning. The new pairs match it by area: measured
  across the cap height, with each letter's recesses counted to 1.1 units deep, Phala's four pairs
  (PH, HA, AL, LA) all hold about 2.67 units of white, within 6% of their mean. PA and AY are
  spaced to that mean, which leaves 0.91 and 0.65 units between their outlines; LA, the source's
  tightest pair, has 0.80.
- **Word space.** The white between PHALA and PAY is an invisible I, a stem and its spacing on
  each side: 2.67 + 3.61 + 2.67 = 8.96 units of white, 7.95 between the A and the P.

## Rendering

`pnpm run brand` renders `public/favicon-32.png`, the manifest's icons, the Apple touch icon, and
`public/og-image.png` from these SVGs and `public/favicon.svg`; commit the PNGs. The icons that
platforms mask are full-bleed, and the dot stays well inside Android's maskable safe zone, the
centred circle of 80% diameter.
