# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

"""Guard repository-local Rust inputs in the CLI's multi-stage image builds."""

from pathlib import Path
import re
import shlex
import unittest


ROOT = Path(__file__).resolve().parents[1]
RUST_INPUT = re.compile(
    r'(?:include_(?:str|bytes)!\s*\(\s*|#\[path\s*=\s*)"([^"]+)"'
)


def copied_inputs(dockerfile: Path) -> list[tuple[Path, Path]]:
    copies = []
    for line in dockerfile.read_text().splitlines():
        if not line.startswith("COPY "):
            continue
        parts = shlex.split(line, comments=True)
        if parts[1].startswith("--from="):
            continue
        destination = Path(parts[-1])
        for source in parts[1:-1]:
            for match in ROOT.glob(source):
                target = destination if match.is_dir() else destination / match.name
                copies.append((match.resolve(), (ROOT / target).resolve()))
    return copies


def is_copied(path: Path, copies: list[tuple[Path, Path]]) -> bool:
    return any(
        path.is_relative_to(source) and target / path.relative_to(source) == path
        for source, target in copies
    )


class RustImageContextTests(unittest.TestCase):
    def check_context(self, crate: str) -> None:
        copies = copied_inputs(ROOT / crate / "Dockerfile.multistage")
        # Shared modules and corpus data are compiled through these crate sources.
        source_roots = [ROOT / crate / "src", ROOT / "shared"]
        source_roots.append(ROOT / ("eval-corpus" if crate == "controller" else "kars-a2a-core") / "src")
        for source_root in source_roots:
            for source in source_root.rglob("*.rs"):
                with self.subTest(source=str(source.relative_to(ROOT))):
                    self.assertTrue(is_copied(source, copies), f"source not copied: {source.relative_to(ROOT)}")
                for reference in RUST_INPUT.findall(source.read_text()):
                    dependency = (source.parent / reference).resolve()
                    with self.subTest(source=str(source.relative_to(ROOT)), reference=reference):
                        self.assertTrue(dependency.is_relative_to(ROOT), "Rust input escapes repository")
                        self.assertTrue(
                            is_copied(dependency, copies),
                            f"Rust input not copied: {dependency.relative_to(ROOT)}",
                        )

    def test_controller_context(self) -> None:
        self.check_context("controller")

    def test_router_context(self) -> None:
        self.check_context("inference-router")


if __name__ == "__main__":
    unittest.main()
