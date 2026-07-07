# Run all tests.
test:
    cargo test

# Lint with the full wall, tests included.
lint:
    cargo clippy --all-targets

# Audit advisories, licenses, and dependency sources.
deny:
    cargo deny check

# Coverage summary in the terminal.
cov:
    cargo llvm-cov --summary-only

# Coverage as a browsable HTML report.
cov-html:
    cargo llvm-cov --html --open
