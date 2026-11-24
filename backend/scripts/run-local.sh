#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."
umask 077
mkdir -p var
if [[ -z "${TP_SECRET:-}" ]]; then
  if [[ ! -f var/secret ]]; then openssl rand -hex 32 > var/secret; fi
  export TP_SECRET="$(cat var/secret)"
fi
# Las notificaciones necesitan el par VAPID. Sin esto el canal quedaría
# desactivado en silencio en un equipo recién preparado. `preparar-equipo.sh`
# genera el archivo; si no existe, se arranca igual y la API informa de que el
# canal no está disponible en lugar de ofrecerlo.
if [[ -z "${TP_VAPID_PRIVATE_KEY:-}" && -s var/vapid-private ]]; then
  export TP_VAPID_PRIVATE_KEY="$(cat var/vapid-private)"
  export TP_VAPID_CONTACT="${TP_VAPID_CONTACT:-mailto:ops@example.com}"
fi
if [[ $# -eq 0 ]]; then set -- serve; fi
exec cargo run --locked --release -- "$@"
