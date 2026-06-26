# kars documentation site

The `docs/site/` directory contains the **mdbook** configuration that turns the canonical markdown tree under `docs/` into a browsable HTML site.

## Why mdbook

- Same source files as on GitHub — no duplication, no drift.
- Searchable, themed, and navigable from a single index.
- Trivial to host: the output is plain static HTML.
- Zero runtime dependencies — `cargo install mdbook` and you have a builder.

## Building locally

```bash
make docs-site
# → renders to ./target/book/
```

Live preview with hot-reload:

```bash
make docs-site-serve
# → http://localhost:3000
```

If `mdbook` is not on your `$PATH`, the Makefile target prints how to install it:

```bash
cargo install mdbook
```

## Layout

```
docs/
├── SUMMARY.md          # mdbook chapter index — single source of truth for the site nav
├── README.md           # rendered as the site introduction page
├── getting-started.md  # …everything else here is rendered as-is
├── architecture/
├── security/
├── operations/
├── api/
├── adr/
└── site/
    ├── README.md       # this file
    ├── book.toml       # mdbook config (src = "..", build-dir = "../../target/book")
    ├── mermaid.min.js  # bundled Mermaid runtime (additional-js)
    ├── mermaid-init.js # brand-aligned Mermaid theme bootstrap (additional-js)
    └── theme/          # kars theme overrides
        ├── index.hbs   # HTML template: top nav bar with kars logo + GitHub CTA
        ├── favicon.svg # kars brand favicon (vector)
        ├── favicon.png # kars brand favicon (raster fallback)
        └── css/
            └── custom.css  # comprehensive theme layer (additional-css)
```

The `src = ".."` setting in `book.toml` tells mdbook to use the entire `docs/` directory as the source tree. That way the same `.md` files reviewers see on GitHub also become the chapters of the rendered site.

The build output (`/target/book/`) is gitignored.

## Theme

The site ships a custom mdbook theme under `site/theme/`:

- **`theme/index.hbs`** overrides the default HTML template to add a sticky top
  navigation bar with the kars logo lockup (mark + wordmark + `docs` tag), the
  centered book title, and a GitHub call-to-action pill. It also loads the
  Inter (UI/prose) and JetBrains Mono (code) webfonts.
- **`theme/css/custom.css`** is the comprehensive theme layer, wired up via
  `additional-css` so it loads last and wins over the stock themes. It defines
  per-theme design tokens (light/rust/coal/navy/ayu), modern typography, CTA
  buttons (`.btn-primary` / `.btn-secondary` inside `.cta-row`), card-style
  tables and admonitions, framed code blocks with shared syntax highlighting,
  and styled Mermaid diagram cards.
- **`theme/favicon.svg` / `theme/favicon.png`** brand the browser tab.
- **`mermaid-init.js`** initialises Mermaid with a kars Azure-family palette and
  the Inter font, switching between light and dark variants with the theme.

Because the overrides only add an `index.hbs` plus a `custom.css` layer (rather
than forking every stock CSS file), the theme stays resilient across mdbook
upgrades.

## Updating the site

1. Edit any `.md` under `docs/` as you would normally.
2. If you added a **new top-level page** that should appear in the site navigation, add it to `docs/SUMMARY.md` under the appropriate section.
3. Run `make docs-site` and skim `target/book/index.html` to confirm the page renders.

There is no separate publication step; deploying the rendered HTML to a hosting target (GitHub Pages, an Azure Static Web App, or a private archive) is performed by a release manager outside this repo.

## Validation in CI

The Helm Lint and other docs-touching workflows do not run mdbook, but `make docs-site` is fast (≈ 1s) and always runnable locally. A future CI job can wrap it as a build-only check; for now the contract is "it builds locally before merge".

## `llms.txt`

`docs/llms.txt` is a machine-readable index of the documentation following the
[llms.txt convention](https://llmstxt.org/), so AI/agent tooling can consume the
docs without scraping. It is generated from `SUMMARY.md` + the first prose line
of each page by `site/gen-llms-txt.py`. Regenerate it after changing `SUMMARY.md`
or a page intro:

```bash
python3 docs/site/gen-llms-txt.py
```

Because `src = ".."`, mdbook copies `docs/llms.txt` to the rendered site root.

## Limitations

- Cross-repo links must be absolute URLs.
- mdbook does not follow links *outside* the `src` tree, so any docs that need to live elsewhere (e.g. `CHANGELOG.md` at repo root) must be referenced via absolute GitHub URLs.
- `docs/internal/` is intentionally **not** included in `SUMMARY.md` — it is a holding area for internal/legacy material and should not appear in the public site.
