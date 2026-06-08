# Showcase asset builders

## Pitch deck (15 slides)

```bash
NODE_PATH=$(npm root -g) node scripts/showcase/build-deck.js
# → docs/showcase/deliverables/kars-pitch-deck.pptx
```

Requires `npm install -g pptxgenjs`.

The deck's content is ground-truthed against repo HEAD via
`docs/showcase/outline.md`. Edit either the outline or
`build-deck.js`, then re-run to regenerate.

## Visual QA

```bash
brew install poppler libreoffice  # one-time
soffice --headless --convert-to pdf docs/showcase/deliverables/kars-pitch-deck.pptx
pdftoppm -jpeg -r 100 kars-pitch-deck.pdf slide
# → slide-01.jpg .. slide-15.jpg
```

Inspect every slide for overlap, overflow, alignment. Re-render
specific slides after fixes:

```bash
pdftoppm -jpeg -r 100 -f N -l N kars-pitch-deck.pdf slide-fixed
```
