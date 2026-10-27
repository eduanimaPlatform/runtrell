#!/bin/sh
# SPDX-License-Identifier: Apache-2.0
set -eu
if [ -z "${RUNTRELL_TEST_DATABASE_URL:-}" ]; then
    exec "$@"
fi
# Do not launch Cargo here: `cargo run` can still own the target-directory lock.
repo=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
runner="${CARGO_TARGET_DIR:-$repo/target}/debug/examples/test_db_runner"
if [ ! -x "$runner" ]; then
    echo 'First run: cargo build -p runtrell-postgres --locked --example test_db_runner' >&2
    exit 1
fi
exec "$runner" "$@"
