# Phase 3 License Preamble OSPO Compliance (S20)

**Date:** 2026-04-30  
**Phase:** Phase 3  
**Ticket:** S20

## Overview

Fix LICENSE file to conform to Microsoft OSPO boilerplate as mandated by the 2026-04-28 Azure-azureclaw OSPO compliance audit (finding: FILE-LICENSE).

## Finding Reference

From `docs/internal/2026-04-28-Azure-azureclaw.md` (FILE-LICENSE check):

> **Status:** ❌ **fail**  
> **Evidence:** First line is `MIT License`; copyright reads `Copyright (c) 2025-2026 Microsoft Corporation` (year range, no trailing period).  
> **OSPO prescribed form:** bare `Copyright (c) Microsoft Corporation.` then standard MIT body.

## Changes Made

### LICENSE (lines 1–3)

**Before:**
```
MIT License

Copyright (c) 2025-2026 Microsoft Corporation
```

**After:**
```
MIT License

Copyright (c) Microsoft Corporation.
```

**Rationale:**
- Removed year range (`2025-2026`) per OSPO guidance — copyright line should state the corporate entity, not a specific interval.
- Added trailing period after "Corporation" to conform to OSPO canonical form.
- MIT body remains unchanged and matches canonical MIT per `https://opensource.org/licenses/MIT`.

## Verification

- ✅ Copyright line: `Copyright (c) Microsoft Corporation.` (no year range, trailing period).
- ✅ MIT body: byte-identical to canonical MIT.
- ✅ First line: `MIT License` (preserved).

## Impact

This is a compliance-only change. No functional code is modified. The license remains MIT with identical terms; only the copyright preamble now matches OSPO standards.

---

Signed-off-by: Copilot <223556219+Copilot@users.noreply.github.com>  
Signed-off-by: Pal Lakatos-Toth <pallakatos@microsoft.com>
