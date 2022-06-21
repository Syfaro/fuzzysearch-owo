#!/bin/sh
set -euxo pipefail

grep -Fq "v$NEW_VERSION" content/CHANGELOG.md
cargo sqlx prepare --check
