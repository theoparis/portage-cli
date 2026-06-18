#!/usr/bin/env bash
# Create GitHub issue labels for all portage-cli workspace crates
# Usage: ./create-labels.sh [--dry-run]
# Requires gh CLI to be installed and authenticated

set -euo pipefail

DRY_RUN=false

if [ $# -ge 1 ] && [ "$1" = "--dry-run" ]; then
    DRY_RUN=true
fi

# Colors for crate labels (GitHub uses hex without #)
CRATE_COLOR="b60206"

echo "Creating crate labels..."

# Array of crate names from workspace
CRATES=(
    "benchmarks"
    "gentoo-core"
    "gentoo-interner"
    "gentoo-stages"
    "portage-atom"
    "portage-atom-pubgrub"
    "portage-atom-resolvo"
    "portage-cli"
    "portage-distfiles"
    "portage-metadata"
    "portage-repo"
    "portage-vdb"
)

for crate in "${CRATES[@]}"; do
    LABEL_NAME="crate: ${crate}"
    DESCRIPTION="Issues related to the ${crate} crate"
    
    if gh label view "$LABEL_NAME" > /dev/null 2>&1; then
        echo "✓ Label '${LABEL_NAME}' already exists, skipping"
        continue
    fi
    
    if [ "$DRY_RUN" = true ]; then
        echo "[DRY RUN] Would create: ${LABEL_NAME} (color: #${CRATE_COLOR})"
    else
        if gh label create "$LABEL_NAME" --color "$CRATE_COLOR" --description "$DESCRIPTION"; then
            echo "✓ Created label: ${LABEL_NAME}"
        else
            echo "✗ Failed to create label: ${LABEL_NAME}"
        fi
    fi
done

echo "Done!"
