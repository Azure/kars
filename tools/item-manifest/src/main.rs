// Byte-level equivalence extractor for refactor-safety proofs.
//
// Walks one or more Rust source files, parses them with `syn`, and emits a
// JSON manifest of every top-level and nested item. Each entry includes:
//   - fully-qualified path (e.g. "routes::chat_completions", or
//     "routes::tests::test_chat_to_responses_body")
//   - kind (fn, struct, enum, trait, impl, const, static, type, macro, mod)
//   - sha256 of the item's TOKEN stream (whitespace-insensitive)
//   - sha256 of the item's BODY-only token stream (for fn: the braced block;
//     for impl/trait/mod: the contained items as one stream)
//   - source file + starting line
//
// A pure-move refactor must preserve:
//   - every fq_path present in baseline
//   - every token_sha256 per fq_path
// Location (file/line) may change.

use proc_macro2::Span;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use syn::spanned::Spanned;
use syn::{Item, ImplItem, TraitItem};
use walkdir::WalkDir;

#[derive(serde::Serialize)]
struct Entry {
    fq_path: String,
    kind: &'static str,
    token_sha256: String,
    body_sha256: String,
    file: String,
    line: usize,
}

fn sha(s: &str) -> String {
    let mut h = Sha256::new();
    h.update(s.as_bytes());
    format!("{:x}", h.finalize())
}

/// Normalise a syn::Item (or any `ToTokens` thing) to a canonical token
/// stream string (ToString uses proc_macro2's canonical whitespace).
fn tokens_of<T: quote::ToTokens>(t: &T) -> String {
    use quote::ToTokens;
    let mut ts = proc_macro2::TokenStream::new();
    t.to_tokens(&mut ts);
    ts.to_string()
}

fn visit_item(prefix: &str, item: &Item, file: &Path, out: &mut Vec<Entry>) {
    match item {
        Item::Fn(f) => {
            let name = f.sig.ident.to_string();
            let fq = format!("{prefix}::{name}");
            let full = tokens_of(item);
            let body = tokens_of(&f.block);
            out.push(Entry {
                fq_path: fq,
                kind: "fn",
                token_sha256: sha(&full),
                body_sha256: sha(&body),
                file: file.display().to_string(),
                line: line_of(f.span()),
            });
        }
        Item::Struct(s) => push_simple(prefix, &s.ident.to_string(), "struct", item, file, s.span(), out),
        Item::Enum(e) => push_simple(prefix, &e.ident.to_string(), "enum", item, file, e.span(), out),
        Item::Const(c) => push_simple(prefix, &c.ident.to_string(), "const", item, file, c.span(), out),
        Item::Static(s) => push_simple(prefix, &s.ident.to_string(), "static", item, file, s.span(), out),
        Item::Type(t) => push_simple(prefix, &t.ident.to_string(), "type", item, file, t.span(), out),
        Item::Trait(t) => {
            let name = t.ident.to_string();
            let fq = format!("{prefix}::{name}");
            out.push(Entry {
                fq_path: fq.clone(),
                kind: "trait",
                token_sha256: sha(&tokens_of(item)),
                body_sha256: sha(&t.items.iter().map(tokens_of).collect::<Vec<_>>().join("")),
                file: file.display().to_string(),
                line: line_of(t.span()),
            });
            for ti in &t.items {
                if let TraitItem::Fn(m) = ti {
                    let n = m.sig.ident.to_string();
                    let inner_fq = format!("{fq}::{n}");
                    out.push(Entry {
                        fq_path: inner_fq,
                        kind: "trait_fn",
                        token_sha256: sha(&tokens_of(ti)),
                        body_sha256: sha(&m.default.as_ref().map(tokens_of).unwrap_or_default()),
                        file: file.display().to_string(),
                        line: line_of(m.span()),
                    });
                }
            }
        }
        Item::Impl(i) => {
            // Stable name: `impl` + target type (+ optional trait)
            let target = tokens_of(&*i.self_ty);
            let trait_part = i
                .trait_
                .as_ref()
                .map(|(_, p, _)| tokens_of(p))
                .unwrap_or_default();
            let key = if trait_part.is_empty() {
                format!("{prefix}::impl::{target}")
            } else {
                format!("{prefix}::impl::{trait_part}_for_{target}")
            };
            out.push(Entry {
                fq_path: key.clone(),
                kind: "impl",
                token_sha256: sha(&tokens_of(item)),
                body_sha256: sha(&i.items.iter().map(tokens_of).collect::<Vec<_>>().join("")),
                file: file.display().to_string(),
                line: line_of(i.span()),
            });
            for ii in &i.items {
                if let ImplItem::Fn(m) = ii {
                    let n = m.sig.ident.to_string();
                    let inner_fq = format!("{key}::{n}");
                    out.push(Entry {
                        fq_path: inner_fq,
                        kind: "impl_fn",
                        token_sha256: sha(&tokens_of(ii)),
                        body_sha256: sha(&tokens_of(&m.block)),
                        file: file.display().to_string(),
                        line: line_of(m.span()),
                    });
                }
            }
        }
        Item::Mod(m) => {
            let name = m.ident.to_string();
            let fq = format!("{prefix}::{name}");
            if let Some((_, items)) = &m.content {
                // Record the mod itself (wrapping tokens) and recurse into its items.
                out.push(Entry {
                    fq_path: fq.clone(),
                    kind: "mod",
                    token_sha256: sha(&tokens_of(item)),
                    body_sha256: sha(&items.iter().map(tokens_of).collect::<Vec<_>>().join("")),
                    file: file.display().to_string(),
                    line: line_of(m.span()),
                });
                for inner in items {
                    visit_item(&fq, inner, file, out);
                }
            } else {
                out.push(Entry {
                    fq_path: fq,
                    kind: "mod_extern",
                    token_sha256: sha(&tokens_of(item)),
                    body_sha256: String::new(),
                    file: file.display().to_string(),
                    line: line_of(m.span()),
                });
            }
        }
        Item::Use(u) => {
            // `use` items don't affect runtime behavior of moved code — track
            // their token hash but under a stable key that ignores count.
            let t = tokens_of(u);
            out.push(Entry {
                fq_path: format!("{prefix}::use::{}", sha(&t)[..8].to_string()),
                kind: "use",
                token_sha256: sha(&t),
                body_sha256: String::new(),
                file: file.display().to_string(),
                line: line_of(u.span()),
            });
        }
        _ => {}
    }
}

fn push_simple(
    prefix: &str,
    name: &str,
    kind: &'static str,
    item: &Item,
    file: &Path,
    span: Span,
    out: &mut Vec<Entry>,
) {
    out.push(Entry {
        fq_path: format!("{prefix}::{name}"),
        kind,
        token_sha256: sha(&tokens_of(item)),
        body_sha256: String::new(),
        file: file.display().to_string(),
        line: line_of(span),
    });
}

fn line_of(span: Span) -> usize {
    span.start().line
}

fn process_file(path: &Path, root_module: &str, out: &mut Vec<Entry>) {
    let src = fs::read_to_string(path).expect("read file");
    let file = syn::parse_file(&src).unwrap_or_else(|e| {
        panic!("parse error in {}: {e}", path.display());
    });
    for item in &file.items {
        visit_item(root_module, item, path, out);
    }
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 3 {
        eprintln!("usage: item-manifest <root-module-name> <path> [<path> ...]");
        eprintln!("  root-module-name: e.g. 'routes' — prefix for fq_paths");
        eprintln!("  path: .rs file or directory (walked recursively for .rs)");
        std::process::exit(2);
    }
    let root = &args[1];
    let mut entries: Vec<Entry> = Vec::new();
    for p in &args[2..] {
        let path = PathBuf::from(p);
        if path.is_dir() {
            for e in WalkDir::new(&path).into_iter().filter_map(|e| e.ok()) {
                if e.path().extension().map(|x| x == "rs").unwrap_or(false) {
                    process_file(e.path(), root, &mut entries);
                }
            }
        } else {
            process_file(&path, root, &mut entries);
        }
    }

    // Deduplicate by fq_path (mod entries recurse, but each item is emitted
    // once at its own level). Keep first.
    let mut by_path: BTreeMap<String, Entry> = BTreeMap::new();
    for e in entries {
        by_path.entry(e.fq_path.clone()).or_insert(e);
    }
    let out: Vec<&Entry> = by_path.values().collect();
    println!("{}", serde_json::to_string_pretty(&out).unwrap());
}
