#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."
umask 077
mkdir -p var
if [[ -z "${TP_SECRET:-}" ]]; then
  if [[ ! -f var/secret ]]; then openssl rand -hex 32 > var/secret; fi
  export TP_SECRET="$(cat var/secret)"
fi
if [[ $# -eq 0 ]]; then set -- serve; fi
exec cargo run --locked --release -- "$@"
