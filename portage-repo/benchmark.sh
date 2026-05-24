#!/bin/bash
set -e

REPO_DIR="$(cd "$(dirname "$0")" && pwd)"
GENTOO_REPO="${REPO_DIR}/gentoo"
NPROC=$(nproc)

echo "=== Portage-repo Benchmark ==="
echo ""

# Clone gentoo mirror if not present
if [ ! -d "${GENTOO_REPO}" ]; then
    echo "Cloning gentoo-mirror/gentoo..."
    git clone --depth 1 https://github.com/gentoo-mirror/gentoo "${GENTOO_REPO}"
else
    echo "Gentoo repo already present at ${GENTOO_REPO}"
fi

# Build release binary
echo ""
echo "Building release binaries..."
cd "${REPO_DIR}"
cargo build --release --example regen_cache --example regen_only

# Use relative path for the examples (avoids issues with absolute paths)
GENTOO_REL="./gentoo"

# Run regen_cache to verify nothing broke
echo ""
echo "=== Running regen_cache (verification) ==="
echo "This will source all ebuilds and compare against existing md5-cache..."
echo ""

START=$(date +%s.%N)
cargo run --release --example regen_cache -- "${GENTOO_REL}" -j "${NPROC}" 2>&1 | tee /tmp/regen_cache_output.txt
REGEN_EXIT=${PIPESTATUS[0]}
END=$(date +%s.%N)

echo ""
echo "regen_cache completed in $(echo "${END} - ${START}" | bc) seconds"

if [ ${REGEN_EXIT} -ne 0 ]; then
    echo ""
    echo "ERROR: regen_cache failed with exit code ${REGEN_EXIT}"
    echo "Check /tmp/regen_cache_output.txt for details"
    exit 1
fi

echo ""
echo "=== Verification passed ==="
echo ""

# Benchmark functions
run_benchmark() {
    local NAME="$1"
    local FILTER="$2"
    local JOBS="$3"
    
    echo ""
    echo "=== Benchmark: ${NAME} (jobs=${JOBS}) ==="
    
    local FILTER_ARG=""
    if [ -n "${FILTER}" ]; then
        FILTER_ARG="${FILTER}"
    fi
    
    START=$(date +%s.%N)
    cargo run --release --example regen_only -- "${GENTOO_REL}" ${FILTER_ARG} -j "${JOBS}" 2>&1 | tee "/tmp/regen_only_${NAME}_j${JOBS}.txt"
    END=$(date +%s.%N)
    
    ELAPSED=$(echo "${END} - ${START}" | bc)
    echo "Completed in ${ELAPSED} seconds"
    echo "${NAME},${JOBS},${ELAPSED}" >> /tmp/benchmark_results.csv
}

# Initialize results CSV
echo "name,jobs,seconds" > /tmp/benchmark_results.csv

# Full tree benchmark with nproc
echo ""
echo "=== Full tree benchmark ==="
run_benchmark "full" "" "${NPROC}"

# dev-util subset with different job counts
echo ""
echo "=== dev-util subset benchmarks ==="
for J in 1 2 4 8; do
    run_benchmark "dev-util" "dev-util/*" "${J}"
done

echo ""
echo "=== Benchmark complete ==="
echo ""
echo "Results summary:"
cat /tmp/benchmark_results.csv | column -t -s,
