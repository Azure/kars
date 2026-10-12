#!/usr/bin/env bash
# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.
#
# Build local wheels for the upstream agent-governance-toolkit Python
# packages that the kars in-pod adapters depend on. Output goes to
# `runtimes/wheels/` (gitignored). The Dockerfiles for the OpenAI-Agents
# and MAF-Python sandbox images COPY this directory into the build
# context and `pip install` the resulting wheels — keeping the image
# build independent of PyPI releases of AGT. AGT 5 core and CLI own
# the mesh and sandbox namespaces; do not coinstall the legacy shims.
# Native ACS wheels are built for both Linux architectures, not the host OS.
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
PY="${PYTHON:-python3}"

if [ ! -d "${AGT_PYTHON_DIR}" ]; then
    echo "error: AGT-Python source tree not found at ${AGT_PYTHON_DIR}" >&2
    echo "       set AGT_PYTHON_DIR to the absolute path of agent-governance-python" >&2
    exit 1
fi

AGT_REPO="$(cd "${AGT_PYTHON_DIR}/.." && pwd -P)"
PIN_SHA="$("${PY}" -c 'import json,sys; print(json.load(open(sys.argv[1]))["sha"])' "${REPO_ROOT}/vendor/agt/pin.json")"
SOURCE_ROOT="$(git -C "${AGT_REPO}" rev-parse --show-toplevel)"
SOURCE_SHA="$(git -C "${AGT_REPO}" rev-parse HEAD)"
SOURCE_STATUS="$(git -C "${AGT_REPO}" status --porcelain --untracked-files=normal)"
if [ "${SOURCE_ROOT}" != "${AGT_REPO}" ] || \
   [ "${SOURCE_SHA}" != "${PIN_SHA}" ] || [ -n "${SOURCE_STATUS}" ]; then
    echo "error: AGT source must be a clean checkout at ${PIN_SHA}; no checkout was changed" >&2
    exit 1
fi
if [ ! -f "${AGT_REPO}/policy-engine/sdk/python/pyproject.toml" ]; then
    echo "error: pinned AGT native ACS source is missing under ${AGT_REPO}/policy-engine" >&2
    exit 1
fi

PACKAGES=(
    "agent-governance-toolkit-core"
    "agent-governance-toolkit-cli"
    "agentmesh-integrations/a2a-protocol"
)

mkdir -p "${WHEEL_DIR}"
# Direct invocations must not attribute replacement wheels to an earlier CLI build.
rm -f "${WHEEL_DIR}/.agt-sha"
# Wipe previous artifacts so stale versions never leak into images.
find "${WHEEL_DIR}" -maxdepth 1 -type f \( -name '*.whl' -o -name '*.tar.gz' \) -delete
for arch in amd64 arm64; do
    rm -rf -- "${WHEEL_DIR:?}/${arch}"
done

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

NATIVE_OUTPUT="$(mktemp -d "${HERE}/.native-wheels.XXXXXX")"
trap 'rm -rf -- "${NATIVE_OUTPUT}"' EXIT
mkdir "${NATIVE_OUTPUT}/source" "${NATIVE_OUTPUT}/wheels"
# A clean Git checkout can still contain ignored compiler caches.
git -C "${AGT_REPO}" archive "${PIN_SHA}" policy-engine \
    | tar -xf - -C "${NATIVE_OUTPUT}/source"
docker buildx build \
    --file "${HERE}/Dockerfile.agt-native" \
    --build-context "agt=${NATIVE_OUTPUT}/source" \
    --output "type=local,dest=${NATIVE_OUTPUT}/wheels" \
    "${REPO_ROOT}"
for arch in amd64 arm64; do
    if [ ! -f "${NATIVE_OUTPUT}/wheels/${arch}/build.json" ]; then
        echo "error: native ACS build receipt missing for ${arch}" >&2
        exit 1
    fi
    mv "${NATIVE_OUTPUT}/wheels/${arch}" "${WHEEL_DIR}/${arch}"
done

"${VENV_PY}" "${HERE}/install-agt-wheels.py" --validate "${WHEEL_DIR}"
echo
echo "wheels written to ${WHEEL_DIR}:"
find "${WHEEL_DIR}" -maxdepth 2 -type f -name '*.whl' -print
