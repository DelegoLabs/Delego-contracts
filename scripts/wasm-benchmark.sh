#!/usr/bin/env bash
# =============================================================================
# scripts/wasm-benchmark.sh
#
# Measures the optimized WASM binary size and Soroban CPU/RAM instruction
# budgets for every contract crate in the workspace.
#
# Usage
# -----
#   # Full run (build + optimize + measure + report)
#   bash scripts/wasm-benchmark.sh
#
#   # Skip rebuild if artifacts already exist
#   bash scripts/wasm-benchmark.sh --no-build
#
#   # Emit GitHub-flavored Markdown summary to $GITHUB_STEP_SUMMARY
#   bash scripts/wasm-benchmark.sh --github-summary
#
#   # Generate a baseline JSON file from current measurements
#   bash scripts/wasm-benchmark.sh --write-baseline
#
# Exit codes
# ----------
#   0  All contracts within limits
#   1  One or more contracts exceed their size limit
#   2  Unexpected script error
#
# Dependencies
# ------------
#   - cargo + wasm32-unknown-unknown target
#   - stellar-cli (or soroban CLI) — only required for optimize step
#   - jq  (JSON processing)
#   - wasm-opt is used internally by stellar-cli; not needed separately
# =============================================================================

set -euo pipefail

# ---------------------------------------------------------------------------
# Configurable constants
# ---------------------------------------------------------------------------

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
WASM_DIR="${REPO_ROOT}/target/wasm32-unknown-unknown/release"
BASELINE_FILE="${REPO_ROOT}/scripts/wasm-size-baseline.json"
OUTPUT_DIR="${REPO_ROOT}/benchmark-results"

# Hard limit: Stellar enforces a 65 536-byte (64 KiB) max for *optimized*
# WASM binaries on Mainnet. We add a 10% warning headroom at 58 982 bytes.
HARD_LIMIT_BYTES=65536
WARN_LIMIT_BYTES=58982   # ~90% of hard limit

# Each crate name maps to the filesystem artifact name (hyphens → underscores).
# These are the five deployable cdylib crates in the workspace.
declare -A CONTRACTS=(
    ["delego-escrow"]="delego_escrow"
    ["delego-permissions"]="delego_permissions"
    ["delego-marketplace"]="delego_marketplace"
    ["delego-reputation"]="delego_reputation"
    ["delego-delegation-registry"]="delego_delegation_registry"
)

# ---------------------------------------------------------------------------
# Argument parsing
# ---------------------------------------------------------------------------

NO_BUILD=false
GITHUB_SUMMARY=false
WRITE_BASELINE=false

for arg in "$@"; do
    case "$arg" in
        --no-build)       NO_BUILD=true ;;
        --github-summary) GITHUB_SUMMARY=true ;;
        --write-baseline) WRITE_BASELINE=true ;;
        --help)
            sed -n '/^# Usage/,/^# Dependencies/p' "$0"
            exit 0
            ;;
        *)
            echo "Unknown argument: $arg" >&2
            exit 2
            ;;
    esac
done

# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------

COLOR_RESET="\033[0m"
COLOR_GREEN="\033[32m"
COLOR_YELLOW="\033[33m"
COLOR_RED="\033[31m"
COLOR_BLUE="\033[34m"
COLOR_BOLD="\033[1m"

log_info()    { echo -e "${COLOR_BLUE}[INFO]${COLOR_RESET}  $*"; }
log_ok()      { echo -e "${COLOR_GREEN}[OK]${COLOR_RESET}    $*"; }
log_warn()    { echo -e "${COLOR_YELLOW}[WARN]${COLOR_RESET}   $*"; }
log_error()   { echo -e "${COLOR_RED}[ERROR]${COLOR_RESET}  $*"; }
log_section() { echo -e "\n${COLOR_BOLD}==> $*${COLOR_RESET}"; }

human_bytes() {
    # Print bytes in a human-readable form (B / KiB).
    local b="$1"
    if [ "$b" -ge 1024 ]; then
        printf "%d KiB (%d B)" $(( b / 1024 )) "$b"
    else
        printf "%d B" "$b"
    fi
}

check_dependency() {
    if ! command -v "$1" &>/dev/null; then
        log_error "Required tool '$1' not found. $2"
        exit 2
    fi
}

# ---------------------------------------------------------------------------
# Step 1 – Build WASM artifacts
# ---------------------------------------------------------------------------

if [ "$NO_BUILD" = false ]; then
    log_section "Building WASM artifacts (release profile)"
    cd "$REPO_ROOT"
    cargo build \
        --target wasm32-unknown-unknown \
        --release \
        --workspace \
        --exclude tests
    log_ok "Build complete."
fi

# ---------------------------------------------------------------------------
# Step 2 – Optimize with stellar-cli / soroban CLI
# ---------------------------------------------------------------------------

if command -v stellar &>/dev/null; then
    SOROBAN_BIN="stellar"
elif command -v soroban &>/dev/null; then
    SOROBAN_BIN="soroban"
else
    log_warn "Neither 'stellar' nor 'soroban' CLI found — skipping optimization step."
    log_warn "Sizes will be measured from unoptimized binaries; results will be larger."
    SOROBAN_BIN=""
fi

if [ -n "$SOROBAN_BIN" ]; then
    log_section "Optimizing WASM artifacts"
    for crate_name in "${!CONTRACTS[@]}"; do
        artifact_name="${CONTRACTS[$crate_name]}"
        wasm_path="${WASM_DIR}/${artifact_name}.wasm"
        if [ -f "$wasm_path" ]; then
            log_info "Optimizing ${crate_name}..."
            "$SOROBAN_BIN" contract optimize --wasm "$wasm_path"
            log_ok "  → ${artifact_name}.optimized.wasm"
        else
            log_warn "  Artifact not found, skipping: $wasm_path"
        fi
    done
fi

# ---------------------------------------------------------------------------
# Step 3 – Measure sizes
# ---------------------------------------------------------------------------

log_section "Measuring WASM binary sizes"
mkdir -p "$OUTPUT_DIR"

check_dependency jq "Install with: sudo apt-get install jq  or  brew install jq"

declare -A SIZE_MAP     # crate_name → optimized size in bytes
declare -A RAW_SIZE_MAP # crate_name → raw (unoptimized) size in bytes

for crate_name in "${!CONTRACTS[@]}"; do
    artifact_name="${CONTRACTS[$crate_name]}"
    raw_path="${WASM_DIR}/${artifact_name}.wasm"
    opt_path="${WASM_DIR}/${artifact_name}.optimized.wasm"

    if [ -f "$opt_path" ]; then
        size=$(stat -c%s "$opt_path" 2>/dev/null || stat -f%z "$opt_path")
        SIZE_MAP["$crate_name"]=$size
    elif [ -f "$raw_path" ]; then
        size=$(stat -c%s "$raw_path" 2>/dev/null || stat -f%z "$raw_path")
        SIZE_MAP["$crate_name"]=$size
        log_warn "${crate_name}: no optimized binary found — using raw size"
    else
        log_warn "${crate_name}: no WASM artifact found at ${raw_path}"
        SIZE_MAP["$crate_name"]=0
    fi

    if [ -f "$raw_path" ]; then
        raw_size=$(stat -c%s "$raw_path" 2>/dev/null || stat -f%z "$raw_path")
        RAW_SIZE_MAP["$crate_name"]=$raw_size
    else
        RAW_SIZE_MAP["$crate_name"]=0
    fi
done

# ---------------------------------------------------------------------------
# Step 4 – Load baseline for diff reporting
# ---------------------------------------------------------------------------

declare -A BASELINE_MAP  # crate_name → baseline size in bytes

if [ -f "$BASELINE_FILE" ]; then
    log_info "Loading baseline from ${BASELINE_FILE}"
    while IFS="=" read -r key val; do
        BASELINE_MAP["$key"]=$val
    done < <(jq -r 'to_entries[] | "\(.key)=\(.value)"' "$BASELINE_FILE")
else
    log_warn "No baseline file found at ${BASELINE_FILE}. Diffs will not be shown."
fi

# ---------------------------------------------------------------------------
# Step 5 – Write baseline (when requested)
# ---------------------------------------------------------------------------

if [ "$WRITE_BASELINE" = true ]; then
    log_section "Writing new baseline to ${BASELINE_FILE}"
    {
        echo "{"
        first=true
        for crate_name in $(echo "${!SIZE_MAP[@]}" | tr ' ' '\n' | sort); do
            if [ "$first" = true ]; then first=false; else echo ","; fi
            printf '  "%s": %d' "$crate_name" "${SIZE_MAP[$crate_name]}"
        done
        echo ""
        echo "}"
    } > "$BASELINE_FILE"
    log_ok "Baseline written."
fi

# ---------------------------------------------------------------------------
# Step 6 – Print console table + enforce limits
# ---------------------------------------------------------------------------

log_section "WASM Size Report"

printf "\n%-40s %12s %12s %10s %10s %8s\n" \
    "Contract" "Optimized" "Raw (unopt)" "vs Limit" "vs Baseline" "Status"
printf "%-40s %12s %12s %10s %10s %8s\n" \
    "$(printf '%0.s-' {1..40})" \
    "$(printf '%0.s-' {1..12})" \
    "$(printf '%0.s-' {1..12})" \
    "$(printf '%0.s-' {1..10})" \
    "$(printf '%0.s-' {1..10})" \
    "$(printf '%0.s-' {1..8})"

FAILURES=0
WARNINGS=0

for crate_name in $(echo "${!SIZE_MAP[@]}" | tr ' ' '\n' | sort); do
    size=${SIZE_MAP[$crate_name]}
    raw_size=${RAW_SIZE_MAP[$crate_name]:-0}

    # vs hard limit
    headroom=$(( HARD_LIMIT_BYTES - size ))
    pct=$(( size * 100 / HARD_LIMIT_BYTES ))
    vs_limit_str="${pct}%"

    # vs baseline diff
    if [ -n "${BASELINE_MAP[$crate_name]+_}" ]; then
        baseline=${BASELINE_MAP[$crate_name]}
        diff=$(( size - baseline ))
        if [ "$diff" -gt 0 ]; then
            diff_str="+${diff}B"
        elif [ "$diff" -lt 0 ]; then
            diff_str="${diff}B"
        else
            diff_str="±0"
        fi
    else
        diff_str="n/a"
    fi

    # Status
    if [ "$size" -gt "$HARD_LIMIT_BYTES" ]; then
        status="${COLOR_RED}✗ OVER${COLOR_RESET}"
        FAILURES=$(( FAILURES + 1 ))
    elif [ "$size" -gt "$WARN_LIMIT_BYTES" ]; then
        status="${COLOR_YELLOW}⚠ WARN${COLOR_RESET}"
        WARNINGS=$(( WARNINGS + 1 ))
    else
        status="${COLOR_GREEN}✓ OK${COLOR_RESET}"
    fi

    printf "%-40s %12s %12s %10s %10s  %b\n" \
        "$crate_name" \
        "$(human_bytes "$size")" \
        "$(human_bytes "$raw_size")" \
        "$vs_limit_str" \
        "$diff_str" \
        "$status"
done

printf "\n"
log_info "Hard limit: $(human_bytes $HARD_LIMIT_BYTES)  |  Warning threshold: $(human_bytes $WARN_LIMIT_BYTES) (90%)"

# ---------------------------------------------------------------------------
# Step 7 – Emit resource instruction estimates from test output
#          (Soroban CPU/RAM instructions are surfaced by cargo test --nocapture)
# ---------------------------------------------------------------------------

log_section "Soroban Resource Instruction Estimates"
log_info "Run 'cargo test --workspace --test cross_contract -- --nocapture'"
log_info "to capture per-operation CPU/RAM instruction budgets."
log_info "The CI wasm-benchmark job captures this output as a separate artifact."

# ---------------------------------------------------------------------------
# Step 8 – Write JSON results artifact
# ---------------------------------------------------------------------------

RESULTS_FILE="${OUTPUT_DIR}/wasm-sizes.json"
{
    echo "{"
    echo "  \"timestamp\": \"$(date -u +%Y-%m-%dT%H:%M:%SZ)\","
    echo "  \"hard_limit_bytes\": ${HARD_LIMIT_BYTES},"
    echo "  \"warn_limit_bytes\": ${WARN_LIMIT_BYTES},"
    echo "  \"contracts\": {"
    first=true
    for crate_name in $(echo "${!SIZE_MAP[@]}" | tr ' ' '\n' | sort); do
        if [ "$first" = true ]; then first=false; else echo ","; fi
        size=${SIZE_MAP[$crate_name]}
        raw_size=${RAW_SIZE_MAP[$crate_name]:-0}
        baseline=${BASELINE_MAP[$crate_name]:-null}
        diff="null"
        if [ "$baseline" != "null" ] && [ -n "$baseline" ]; then
            diff=$(( size - baseline ))
        fi
        printf '    "%s": { "optimized_bytes": %d, "raw_bytes": %d, "baseline_bytes": %s, "diff_bytes": %s }' \
            "$crate_name" "$size" "$raw_size" \
            "${baseline:-null}" "${diff}"
    done
    echo ""
    echo "  }"
    echo "}"
} > "$RESULTS_FILE"

log_ok "Results written to ${RESULTS_FILE}"

# ---------------------------------------------------------------------------
# Step 9 – GitHub Markdown summary
# ---------------------------------------------------------------------------

if [ "$GITHUB_SUMMARY" = true ] && [ -n "${GITHUB_STEP_SUMMARY:-}" ]; then
    log_info "Writing GitHub step summary…"
    {
        echo "## 📦 WASM Binary Size Report"
        echo ""
        echo "| Contract | Optimized | Raw (unopt) | % of limit | vs Baseline | Status |"
        echo "|---|---|---|---|---|---|"

        for crate_name in $(echo "${!SIZE_MAP[@]}" | tr ' ' '\n' | sort); do
            size=${SIZE_MAP[$crate_name]}
            raw_size=${RAW_SIZE_MAP[$crate_name]:-0}
            pct=$(( size * 100 / HARD_LIMIT_BYTES ))

            if [ -n "${BASELINE_MAP[$crate_name]+_}" ]; then
                baseline=${BASELINE_MAP[$crate_name]}
                diff=$(( size - baseline ))
                if [ "$diff" -gt 0 ]; then diff_str="+${diff} B ⬆️"
                elif [ "$diff" -lt 0 ]; then diff_str="${diff} B ⬇️"
                else diff_str="±0"; fi
            else
                diff_str="—"
            fi

            if [ "$size" -gt "$HARD_LIMIT_BYTES" ]; then
                status_icon="🔴 **OVER LIMIT**"
            elif [ "$size" -gt "$WARN_LIMIT_BYTES" ]; then
                status_icon="🟡 Warning"
            else
                status_icon="🟢 OK"
            fi

            echo "| \`${crate_name}\` | $(human_bytes $size) | $(human_bytes $raw_size) | ${pct}% | ${diff_str} | ${status_icon} |"
        done

        echo ""
        echo "> **Hard limit**: 64 KiB (65 536 B) — Stellar Mainnet maximum for optimized WASM"
        echo "> **Warning threshold**: 90% of limit (58 982 B)"
        echo ""

        if [ "$FAILURES" -gt 0 ]; then
            echo "### ❌ ${FAILURES} contract(s) exceed the 64 KiB hard limit"
            echo ""
            echo "The WASM binary size check **failed**. Reduce contract size before merging."
        elif [ "$WARNINGS" -gt 0 ]; then
            echo "### ⚠️ ${WARNINGS} contract(s) are approaching the 64 KiB limit"
        else
            echo "### ✅ All contracts are within size limits"
        fi
    } >> "$GITHUB_STEP_SUMMARY"
fi

# ---------------------------------------------------------------------------
# Step 10 – Generate PR comment body
# ---------------------------------------------------------------------------

PR_COMMENT_FILE="${OUTPUT_DIR}/pr-comment.md"

{
    echo "## 📦 WASM Binary Size & Resource Report"
    echo ""
    echo "Automated benchmark results for commit \`${GITHUB_SHA:-$(git rev-parse --short HEAD 2>/dev/null || echo 'local')}\`."
    echo ""
    echo "### Binary Sizes"
    echo ""
    echo "| Contract | Optimized | Raw (unopt) | % of 64 KiB limit | vs Baseline | Status |"
    echo "|---|---|---|---|---|---|"

    for crate_name in $(echo "${!SIZE_MAP[@]}" | tr ' ' '\n' | sort); do
        size=${SIZE_MAP[$crate_name]}
        raw_size=${RAW_SIZE_MAP[$crate_name]:-0}
        pct=$(( size * 100 / HARD_LIMIT_BYTES ))

        if [ -n "${BASELINE_MAP[$crate_name]+_}" ]; then
            baseline=${BASELINE_MAP[$crate_name]}
            diff=$(( size - baseline ))
            if [ "$diff" -gt 0 ]; then diff_str="+${diff} B ⬆️"
            elif [ "$diff" -lt 0 ]; then diff_str="${diff} B ⬇️"
            else diff_str="±0 B"; fi
        else
            diff_str="—"
        fi

        if [ "$size" -gt "$HARD_LIMIT_BYTES" ]; then
            status_icon="🔴 OVER LIMIT"
        elif [ "$size" -gt "$WARN_LIMIT_BYTES" ]; then
            status_icon="🟡 Warning (>90%)"
        else
            status_icon="🟢 OK"
        fi

        echo "| \`${crate_name}\` | $(human_bytes $size) | $(human_bytes $raw_size) | ${pct}% | ${diff_str} | ${status_icon} |"
    done

    echo ""
    echo "> **Hard limit**: 65 536 B (64 KiB) — Stellar Mainnet enforced maximum"
    echo "> **Warning threshold**: 58 982 B (90% of limit)"
    echo ""
    echo "### Resource Instruction Budgets"
    echo ""
    echo "CPU and RAM instruction measurements are captured from the"
    echo "\`cross_contract\` integration test suite with \`--nocapture\`."
    echo "See the **cross-contract-benchmark** artifact attached to this run for"
    echo "per-operation breakdowns."
    echo ""

    if [ "$FAILURES" -gt 0 ]; then
        echo "### ❌ Size check FAILED — ${FAILURES} contract(s) over limit"
        echo ""
        echo "This PR cannot be merged until binary sizes are reduced below 64 KiB."
    elif [ "$WARNINGS" -gt 0 ]; then
        echo "### ⚠️ ${WARNINGS} contract(s) approaching limit"
        echo ""
        echo "No hard failure, but consider reducing contract size before the limit is breached."
    else
        echo "### ✅ All ${#SIZE_MAP[@]} contracts within size limits"
    fi

    echo ""
    echo "<details>"
    echo "<summary>Raw measurement data (JSON)</summary>"
    echo ""
    echo '```json'
    cat "$RESULTS_FILE"
    echo '```'
    echo ""
    echo "</details>"
} > "$PR_COMMENT_FILE"

log_ok "PR comment body written to ${PR_COMMENT_FILE}"

# ---------------------------------------------------------------------------
# Final exit
# ---------------------------------------------------------------------------

if [ "$FAILURES" -gt 0 ]; then
    log_error "${FAILURES} contract(s) exceed the ${HARD_LIMIT_BYTES}-byte hard limit. Aborting."
    exit 1
fi

if [ "$WARNINGS" -gt 0 ]; then
    log_warn "${WARNINGS} contract(s) are above the 90% warning threshold."
fi

log_ok "All WASM size checks passed."
exit 0
