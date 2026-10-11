#!/usr/bin/env bash
# Repeatable local gate; writes build/evidence files only to the selected directories.
set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/.."
target_dir="${CARGO_TARGET_DIR:-$PWD/target}"
evidence_dir="${BIGSOUND_VALIDATION_DIR:-$PWD/validation-output}"
mkdir -p "$evidence_dir"
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features --locked --target-dir "$target_dir" -- -D warnings
cargo test --workspace --locked --target-dir "$target_dir" | tee "$evidence_dir/tests.txt"
cargo build --workspace --release --locked --target-dir "$target_dir"
python3 scripts/test-ladspa.py "$target_dir/release" | tee "$evidence_dir/ladspa.txt"
"$target_dir/release/bigsound-qa" "${BIGSOUND_STRESS_SECONDS:-30}" | tee "$evidence_dir/performance.csv"
for shell_source in scripts/*.sh packaging/PKGBUILD packaging/bigsound.install; do
    bash -n "$shell_source"
done
if command -v shellcheck >/dev/null; then
    shellcheck scripts/*.sh
    # makepkg provides the package metadata and srcdir/pkgdir variables.
    shellcheck --shell=bash --exclude=SC2034,SC2154 packaging/PKGBUILD packaging/bigsound.install
else
    echo 'ShellCheck skipped: install shellcheck for the full shell lint gate.' >&2
fi
msgfmt --check -o "$evidence_dir/pt_BR.mo" crates/gtk-app/po/pt_BR.po
if [[ "${BIGSOUND_TEST_SESSION:-0}" == 1 ]]; then
    python3 scripts/test-session.py "$target_dir/release" | tee "$evidence_dir/session.txt"
fi
if command -v cargo-audit >/dev/null; then
    audit_args=(--deny warnings)
    if [[ -n "${BIGSOUND_AUDIT_DB:-}" ]]; then
        audit_args+=(--db "$BIGSOUND_AUDIT_DB" --no-fetch)
    fi
    cargo audit "${audit_args[@]}" | tee "$evidence_dir/dependencies.txt"
else
    echo 'RustSec audit skipped: install cargo-audit and run cargo audit --deny warnings before publishing.' >&2
fi
echo "Validation evidence: $evidence_dir"
