#!/bin/sh
set -euxo pipefail

grep -Fq "v$NEW_VERSION" CHANGELOG.md
cargo sqlx prepare --check
