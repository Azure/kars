#!/usr/bin/env bash
# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.
set -euo pipefail

if [ "${SCOPE_RESULT:?Missing scope result}" != success ]; then
  echo "Native contract scope selection did not succeed" >&2
  exit 1
fi

case "${NATIVE_REQUIRED:?Missing native contract decision}" in
  true)
    if [ "${API_RESULT:?Missing API result}" != success ] ||
       [ "${RUNTIME_RESULT:?Missing runtime result}" != success ]; then
      echo "Both native API and runtime acceptance must succeed" >&2
      exit 1
    fi
    ;;
  false)
    if [ "${API_RESULT:?Missing API result}" != skipped ] ||
       [ "${RUNTIME_RESULT:?Missing runtime result}" != skipped ]; then
      echo "Unexpected native job outcome for a documentation-only change" >&2
      exit 1
    fi
    echo "Documentation-only change: native execution not required, not claimed as tested"
    ;;
  *)
    echo "Invalid native contract scope decision" >&2
    exit 1
    ;;
esac
