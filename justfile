# Every CI gate, locally.
check: fmt clippy test doc deny cap deps hygiene

fmt:
    cargo fmt --all --check

clippy:
    cargo clippy --workspace --all-targets --locked -- -D warnings
    # At most 10 allows in the workspace, each with a reason (clippy checks the reason).
    test "$(grep -rE 'allow\(' crates | wc -l)" -le 10

test:
    cargo test --workspace --locked

doc:
    RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps --locked

deny:
    cargo deny --locked check

cap:
    scripts/line-cap.sh

deps:
    scripts/dep-direction.sh

hygiene:
    scripts/hygiene.sh

# The CLI timing report (soft target: a non-waiting call under 20 ms).
bench:
    cargo test -p berth --locked --test cli timing_report -- --nocapture
