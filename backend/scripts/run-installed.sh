#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."
umask 077
if [[ ! -s var/secret ]]; then
  printf '%s\n' 'Falta backend/var/secret. Consulta la guía local antes de restaurar la configuración.' >&2
  exit 1
fi
export TP_SECRET="$(cat var/secret)"
export TP_CONFIG_FILE="$PWD/var/local-config.json"
exec "$PWD/var/bin/territorio-api" "${@:-serve}"
