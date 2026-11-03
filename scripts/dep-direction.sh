#!/bin/sh
# berth-core depends on no other workspace crate.
set -eu
bad=$(cargo metadata --format-version 1 --no-deps --locked |
  jq -r '[.packages[].name] as $ws | .packages[] | select(.name == "berth-core") | .dependencies[].name | select(IN($ws[]))')
test -z "$bad" || { echo "berth-core must not depend on: $bad"; exit 1; }
echo "berth-core has no workspace deps"
