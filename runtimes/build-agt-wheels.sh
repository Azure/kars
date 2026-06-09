#!/usr/bin/env bash
# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.
#
# Build local wheels for the upstream agent-governance-toolkit Python
# packages that the kars in-pod adapters depend on. Output goes to
# `runtimes/wheels/` (gitignored). The Dockerfiles for the OpenAI-Agents
# and MAF-Python sandbox images COPY this directory into the build
# context and `pip install` the resulting wheels — keeping the image
# build hermetic without publishing private wheels to PyPI.
#
# Usage:
#   AGT_PYTHON_DIR=/path/to/agent-governance-python ./runtimes/build-agt-wheels.sh
#
# AGT_PYTHON_DIR defaults to a sibling checkout of the upstream repo:
#   ../agt/agent-governance-toolkit/agent-governance-python
set -euo pipefail

HERE="$(cd "$(dirname "$0")" && pwd)"
REPO_ROOT="$(cd "${HERE}/.." && pwd)"
WHEEL_DIR="${HERE}/wheels"

DEFAULT_AGT_DIR="${REPO_ROOT}/../../agt/agent-governance-toolkit/agent-governance-python"
AGT_PYTHON_DIR="${AGT_PYTHON_DIR:-${DEFAULT_AGT_DIR}}"

if [ ! -d "${AGT_PYTHON_DIR}" ]; then
    echo "error: AGT-Python source tree not found at ${AGT_PYTHON_DIR}" >&2
    echo "       set AGT_PYTHON_DIR to the absolute path of agent-governance-python" >&2
    exit 1
fi

PACKAGES=(
    "agent-sandbox"
    "agent-mesh"
    "agentmesh-integrations/a2a-protocol"
)

mkdir -p "${WHEEL_DIR}"
# Wipe previous artifacts so stale versions never leak into images.
find "${WHEEL_DIR}" -maxdepth 1 -type f \( -name '*.whl' -o -name '*.tar.gz' \) -delete

PY="${PYTHON:-python3}"

# Hermetic build: PEP 668 blocks `pip install build` against the
# system Python on modern macOS Homebrew / many Linux distros. Spin
# up a per-invocation venv so we never touch the host site-packages,
# and re-use the same venv across the loop (caches `build`). The
# venv lives OUTSIDE WHEEL_DIR so docker `COPY runtimes/wheels/`
# from the sandbox-image Dockerfiles never picks it up.
VENV_DIR="${HERE}/.builder-venv"
if [ ! -x "${VENV_DIR}/bin/python" ]; then
    "${PY}" -m venv "${VENV_DIR}"
fi
VENV_PY="${VENV_DIR}/bin/python"
"${VENV_PY}" -m pip install --quiet --upgrade pip build >/dev/null

for pkg in "${PACKAGES[@]}"; do
    src="${AGT_PYTHON_DIR}/${pkg}"
    if [ ! -f "${src}/pyproject.toml" ]; then
        echo "error: ${src}/pyproject.toml not found" >&2
        exit 1
    fi
    echo "building wheel for ${pkg} from ${src}"
    "${VENV_PY}" -m build --wheel --outdir "${WHEEL_DIR}" "${src}" >/dev/null
done

echo
echo "wheels written to ${WHEEL_DIR}:"
ls -1 "${WHEEL_DIR}"/*.whl
