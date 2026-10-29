#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-only
# Additional terms under AGPLv3 section 7: see LICENSE_ADDITIONAL_TERMS.md in the repository root.
#
# Brings the GitHub issues in line with the 0.3.0 release (needs `gh auth login` or GH_TOKEN).
# Review the lists, then run:  bash scripts/sync-issues.sh
set -euo pipefail
REPO="Johnny-Wickky/arca"
NOTES="docs/releases/v0.3.0.md"

# Done in 0.3.0.
for n in 35 50 51; do
    gh issue close "$n" --repo "$REPO" --comment "Done in v0.3.0 (see $NOTES)."
done
# A duplicate.
gh issue close 52 --repo "$REPO" --reason "not planned" --comment "Duplicate of #50."

# Still limited: reopen with the honest status.
for n in 42 44 47 48; do
    gh issue reopen "$n" --repo "$REPO" --comment "Reopened: v0.3.0 was checked by compilation, unit tests and host-side JNI tests only; no device, ARM or emulator run, and no real-world documents. Remaining limits are listed in $NOTES."
done
