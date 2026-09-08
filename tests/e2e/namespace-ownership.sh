#!/usr/bin/env bash
# Copyright (c) Microsoft Corporation.
# Licensed under the MIT License.

# Retains the original SRE source/namespace lifecycle gate, now using the
# required safe CLI stage/review/enroll/migrate/install/retire/uninstall flow.
# The fresh source is created after legacy retirement and receives new UIDs;
# retained registration history is updated with exact reviewed UID/RV CAS.
test_sre_namespace_ownership() {
    sre_authority_phase fresh
}
