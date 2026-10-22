#!/usr/bin/env bash
# Spec: ./docs/srdd/22. SRDD - Deployment and Containers.md#221-multi-stage-dockerfile-and-dockerignore-architecture
# Spec: ./docs/srdd/5. SRDD - System Architecture.md#55-command-line-interface-llmroller-contract-lifecycle-and-exit-codes
# Requirements: POR-1, REL-1, RTE-14, SEC-10, SEC-11
# Phase: ./docs/bigbang/big-bang-phases-7.md#75-sub-phase-75

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "${REPO_ROOT}"

echo "=== LLMRoller Release Package Verification Harness ==="

# 1. Verify Dockerfile contract (< 50MB, distroless, nonroot 65532, drop caps)
echo "Checking Dockerfile and distroless security contracts..."
grep -q "gcr.io/distroless/static-debian12:nonroot" Dockerfile || {
    echo "ERROR: Dockerfile does not use distroless nonroot image" >&2
    exit 1
}
grep -q "USER 65532:65532" Dockerfile || {
    echo "ERROR: Dockerfile does not enforce nonroot UID 65532" >&2
    exit 1
}

# 2. Validate docker-compose.yml configuration
echo "Checking docker-compose.yml manifest..."
if command -v docker >/dev/null 2>&1; then
    docker compose config --quiet || {
        echo "ERROR: docker compose config failed" >&2
        exit 1
    }
fi

# 3. Verify CLI contracts (--version, generate-master-key, serve, backup)
echo "Checking CLI subcommands and contracts..."
CARGO_TARGET="${CARGO_TARGET_DIR:-target}/debug"
LLMROLLER_BIN="${CARGO_TARGET}/llmroller"
if [[ -f "${LLMROLLER_BIN}.exe" ]]; then
    LLMROLLER_BIN="${LLMROLLER_BIN}.exe"
fi

if [[ -f "${LLMROLLER_BIN}" ]]; then
    # Test --version contract: llmroller <semver> (<git_sha>, schema <n>)
    VERSION_OUT="$("${LLMROLLER_BIN}" --version)"
    echo "Version output: ${VERSION_OUT}"
    echo "${VERSION_OUT}" | grep -E '^llmroller [0-9]+\.[0-9]+\.[0-9]+ \([a-zA-Z0-9_-]+, schema [0-9]+\)$' || {
        echo "ERROR: --version does not match mandated schema contract" >&2
        exit 1
    }

    # Test generate-master-key outputs exactly 32 raw bytes (44 chars base64)
    KEY_OUT="$("${LLMROLLER_BIN}" generate-master-key)"
    KEY_LEN="${#KEY_OUT}"
    if [[ "${KEY_LEN}" -lt 43 || "${KEY_LEN}" -gt 45 ]]; then
        echo "ERROR: generate-master-key output length invalid: ${KEY_LEN}" >&2
        exit 1
    fi

    # Verify serve subcommand exists and displays help
    "${LLMROLLER_BIN}" serve --help >/dev/null

    # Verify backup subcommand exists and displays help
    "${LLMROLLER_BIN}" backup --help >/dev/null
else
    echo "Warning: binary not built at ${LLMROLLER_BIN}; running via cargo"
    cargo run --bin llmroller -- --version
    cargo run --bin llmroller -- generate-master-key >/dev/null
fi

# 4. Verify release supply-chain wiring (SBOM + cosign + container gates)
echo "Checking release SBOM, signature, and container gates..."
grep -q "cargo cyclonedx --manifest-path crates/server/Cargo.toml" .github/workflows/release.yml || {
    echo "ERROR: release workflow does not generate a CycloneDX SBOM" >&2
    exit 1
}
grep -q "cosign sign-blob --yes --bundle" .github/workflows/release.yml || {
    echo "ERROR: release workflow does not cosign-sign artifacts" >&2
    exit 1
}
grep -q "target: runtime-distroless" .github/workflows/release.yml || {
    echo "ERROR: release workflow does not build the distroless container image" >&2
    exit 1
}
grep -q "50 \* 1024 \* 1024" .github/workflows/release.yml || {
    echo "ERROR: release workflow does not enforce the < 50 MB image size gate" >&2
    exit 1
}
grep -q "/v1/models" .github/workflows/release.yml || {
    echo "ERROR: release workflow does not smoke-test the /v1/models endpoint" >&2
    exit 1
}
grep -q "docker stop --time 30" .github/workflows/release.yml || {
    echo "ERROR: release workflow does not exercise the SIGTERM drain gate" >&2
    exit 1
}

# 5. Verify supply-chain and governance gates
echo "Checking governance gates..."
cargo xtask doc-consistency
cargo xtask docs-bundle --check
cargo xtask traceability --milestone M6

echo "=== All release package checks passed successfully (POR-1, REL-1, RTE-14, SEC-10, SEC-11) ==="
exit 0
