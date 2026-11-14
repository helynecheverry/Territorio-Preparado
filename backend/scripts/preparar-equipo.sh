#!/usr/bin/env bash
# Deja un equipo nuevo listo para desarrollar, sin pedir permisos de
# administrador en ningún momento.
#
# Hace lo mínimo y lo dice antes de hacerlo:
#
#   1. Comprueba Rust y Node, que sí hay que instalar aparte.
#   2. Instala MongoDB en el espacio del usuario si no hay uno, verificando la
#      suma que publica MongoDB antes de extraer nada. No necesita sudo.
#   3. Genera la clave de sesión y el par VAPID si faltan. Estos archivos NO
#      viajan con el repositorio, así que cada equipo tiene los suyos.
#   4. Importa el territorio, el contenido educativo y los documentos legales.
#
# Es idempotente: volver a ejecutarlo no rompe nada ni regenera claves. Si ya
# tienes un MongoDB propio corriendo, lo usa y no instala otro.
set -euo pipefail
cd "$(dirname "$0")/.."
umask 077

MONGO_HOME="$HOME/.local/opt/mongodb"
MONGO_DATA="$HOME/.local/var/mongodb"
paso() { printf '\n\033[1m==> %s\033[0m\n' "$1"; }
aviso() { printf '    %s\n' "$1"; }

paso "Comprobando lo que hace falta tener instalado"
falta=0
for cmd in cargo openssl curl python3; do
  if command -v "$cmd" >/dev/null 2>&1; then
    aviso "$cmd: ok"
  else
    aviso "$cmd: FALTA"
    falta=1
  fi
done
# Node solo se usa para comprobar la sintaxis del frontend, no para construirlo.
command -v node >/dev/null 2>&1 && aviso "node: ok (opcional)" || aviso "node: falta (opcional, solo para 'node --check')"
if [[ $falta -eq 1 ]]; then
  aviso ""
  aviso "Instala lo marcado como FALTA y vuelve a ejecutar."
  aviso "Rust: https://rustup.rs"
  exit 1
fi

paso "MongoDB"
if command -v mongod >/dev/null 2>&1 || [[ -x "$MONGO_HOME/bin/mongod" ]]; then
  aviso "ya hay un mongod instalado; no se instala otro"
else
  aviso "no hay mongod; se instala en $MONGO_HOME sin permisos de administrador"
  codename="$( . /etc/os-release 2>/dev/null && printf '%s' "${UBUNTU_CODENAME:-noble}" )"
  case "$codename" in
    noble|oracular|plucky) target=ubuntu2404 ;;
    jammy) target=ubuntu2204 ;;
    *) target=ubuntu2404 ;;
  esac
  url="$(curl -fsSL https://downloads.mongodb.org/current.json | python3 -c "
import sys, json
data = json.load(sys.stdin)
for version in data['versions']:
    if not version.get('production_release'):
        continue
    for d in version['downloads']:
        if d.get('target') == '$target' and d.get('arch') == 'x86_64' and d.get('edition') == 'targeted':
            print(d['archive']['url'])
            raise SystemExit
raise SystemExit('sin descarga para $target')
")"
  aviso "descargando $(basename "$url")"
  tmp="$(mktemp -d)"
  trap 'rm -rf "$tmp"' EXIT
  curl -fsSL -o "$tmp/mongodb.tgz" "$url"
  # La suma la publica MongoDB junto al archivo: se verifica antes de extraer.
  curl -fsSL -o "$tmp/mongodb.sha256" "$url.sha256"
  ( cd "$tmp" && printf '%s  mongodb.tgz\n' "$(cut -d' ' -f1 mongodb.sha256)" | sha256sum -c - )
  mkdir -p "$HOME/.local/opt"
  tar xzf "$tmp/mongodb.tgz" -C "$tmp"
  rm -rf "$MONGO_HOME"
  mv "$tmp"/mongodb-linux-* "$MONGO_HOME"
  aviso "instalado: $("$MONGO_HOME/bin/mongod" --version | head -1)"
fi

if ! ss -ltn 2>/dev/null | grep -q 127.0.0.1:27017; then
  paso "Arrancando MongoDB como servicio de usuario"
  mkdir -p "$MONGO_DATA/data" "$MONGO_DATA/log" "$HOME/.config/systemd/user"
  mongod_bin="$(command -v mongod || printf '%s' "$MONGO_HOME/bin/mongod")"
  cat > "$MONGO_DATA/mongod.conf" <<YAML
# MongoDB de desarrollo de Territorio Preparado.
# En el espacio del usuario y solo en loopback: no necesita root ni se expone.
storage:
  dbPath: $MONGO_DATA/data
systemLog:
  destination: file
  path: $MONGO_DATA/log/mongod.log
  logAppend: true
net:
  bindIp: 127.0.0.1
  port: 27017
YAML
  cat > "$HOME/.config/systemd/user/mongod.service" <<UNIT
[Unit]
Description=MongoDB para Territorio Preparado (instancia de usuario, solo loopback)
After=network.target

[Service]
Type=simple
ExecStart=$mongod_bin --config $MONGO_DATA/mongod.conf
Restart=on-failure
RestartSec=5

[Install]
WantedBy=default.target
UNIT
  systemctl --user daemon-reload
  systemctl --user enable --now mongod.service
  for _ in $(seq 1 60); do
    ss -ltn 2>/dev/null | grep -q 127.0.0.1:27017 && break
    sleep 0.5
  done
fi
ss -ltn 2>/dev/null | grep -q 127.0.0.1:27017 \
  && aviso "MongoDB escuchando en 127.0.0.1:27017" \
  || { aviso "MongoDB no respondió; revisa $MONGO_DATA/log/mongod.log"; exit 1; }

paso "Claves locales"
mkdir -p var
# Estas claves NO están en el repositorio y son propias de cada equipo. Si vas a
# compartir base de datos entre equipos, copia la misma TP_VAPID_PRIVATE_KEY en
# todos: una suscripción de push solo acepta mensajes firmados con la clave con
# la que se creó.
if [[ -s var/secret ]]; then
  aviso "var/secret: ya existe, no se toca"
else
  openssl rand -hex 32 > var/secret
  aviso "var/secret: generado"
fi

export TP_SECRET="$(cat var/secret)"
export TP_ENV=development TP_SMTP_HOST="" TP_MONGO_DB="${TP_MONGO_DB:-territorio}"

cargo build --locked
if [[ -s var/vapid-private ]]; then
  aviso "var/vapid-private: ya existe, no se toca"
else
  ./target/debug/territorio-api vapid-keys > var/vapid.json
  python3 -c "
import json
d = json.load(open('var/vapid.json'))
open('var/vapid-private', 'w').write(d['TP_VAPID_PRIVATE_KEY'])
print('    clave pública para el navegador:', d['public_key_for_browser'])
"
  aviso "var/vapid-private: generado"
fi

paso "Importando datos"
./target/debug/territorio-api import-territory territories/co-cali.json
./target/debug/territorio-api import-lessons lessons/co-es.json
./target/debug/territorio-api import-legal legal/co-es.json

paso "Listo"
cat <<'FIN'
    Arranca con:   ./scripts/run-local.sh
    Y abre:        http://127.0.0.1:4180/app/alerta.html

    Queda pendiente, y no lo puede hacer un script:

      · Revisar y publicar el contenido educativo y los términos. Ambos exigen
        una persona identificada como revisora; puede ser alguien del equipo.
        PATCH /api/v1/lessons/{id} y PATCH /api/v1/legal/terms, desde una
        cuenta de operador o administrador.

      · Para que llegue un SMS, contratar proveedor y poner TP_SMS_URL.
      · Para cobrar, contratar pasarela y poner TP_BILLING_*.

    Mientras tanto la aplicación no ofrece esos canales, en lugar de prometer
    algo que no puede entregar. Ver backend/.env.example.
FIN
