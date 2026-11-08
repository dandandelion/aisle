# Source this to use the project-local Rust toolchain:  . scripts/env.sh
AISLE_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
export RUSTUP_HOME="$AISLE_ROOT/.toolchain/rustup"
export CARGO_HOME="$AISLE_ROOT/.toolchain/cargo"
export PATH="$CARGO_HOME/bin:$PATH"
