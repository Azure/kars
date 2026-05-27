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
    └── book.toml       # mdbook config (src = "..", build-dir = "../../target/book")
```

The `src = ".."` setting in `book.toml` tells mdbook to use the entire `docs/` directory as the source tree. That way the same `.md` files reviewers see on GitHub also become the chapters of the rendered site.

The build output (`/target/book/`) is gitignored.

## Updating the site

1. Edit any `.md` under `docs/` as you would normally.
2. If you added a **new top-level page** that should appear in the site navigation, add it to `docs/SUMMARY.md` under the appropriate section.
3. Run `make docs-site` and skim `target/book/index.html` to confirm the page renders.

There is no separate publication step; deploying the rendered HTML to a hosting target (GitHub Pages, an Azure Static Web App, or a private archive) is performed by a release manager outside this repo.

## Validation in CI

The Helm Lint and other docs-touching workflows do not run mdbook, but `make docs-site` is fast (≈ 1s) and always runnable locally. A future CI job can wrap it as a build-only check; for now the contract is "it builds locally before merge".

## Limitations

- Cross-repo links must be absolute URLs.
- mdbook does not follow links *outside* the `src` tree, so any docs that need to live elsewhere (e.g. `CHANGELOG.md` at repo root) must be referenced via absolute GitHub URLs.
- `docs/internal/` is intentionally **not** included in `SUMMARY.md` — it is a holding area for internal/legacy material and should not appear in the public site.
