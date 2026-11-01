#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."
umask 077
[[ -s var/secret && -f var/local-config.json && -f var/local-account.json ]] || { printf '%s\n' 'Faltan la configuración privada o la cuenta local. Consulta la guía de esta instalación.' >&2; exit 1; }
[[ -x "$HOME/.local/opt/mongodb/bin/mongod" ]] || { printf '%s\n' 'No se encontró la instalación MongoDB utilizada por este equipo.' >&2; exit 1; }
systemctl --user cat driveguard-mongodb.service >/dev/null
cargo build --locked --release
mkdir -p var/bin
install -m 700 target/release/territorio-api var/bin/territorio-api.new
mv -f var/bin/territorio-api.new var/bin/territorio-api
python3 - <<'PY'
from pathlib import Path
import os, shutil, subprocess
backend=Path.cwd();project=backend.parent
units=Path.home()/'.config/systemd/user';units.mkdir(parents=True,exist_ok=True)
def quote(value):return '"'+str(value).replace('\\','\\\\').replace('"','\\"').replace('%','%%')+'"'
unit=f'''[Unit]
Description=Territorio Preparado API Rust e IA local
Requires=driveguard-mongodb.service
After=driveguard-mongodb.service
StartLimitIntervalSec=0

[Service]
Type=exec
WorkingDirectory={str(backend).replace(chr(37),chr(37)*2)}
ExecStart={quote(backend/'scripts/run-installed.sh')} serve
Restart=on-failure
RestartSec=5
TimeoutStopSec=45
KillSignal=SIGTERM
UMask=0077
NoNewPrivileges=true
StandardOutput=journal
StandardError=journal

[Install]
WantedBy=default.target
'''
path=units/'territorio.service';path.write_text(unit);path.chmod(0o644)
try:desktop=Path(subprocess.check_output(['xdg-user-dir','DESKTOP'],text=True).strip())
except (FileNotFoundError,subprocess.CalledProcessError):desktop=Path.home()/'Desktop'
desktop.mkdir(parents=True,exist_ok=True)
menu=Path.home()/'.local/share/applications';menu.mkdir(parents=True,exist_ok=True)
def desktop_quote(value):return '"'+str(value).replace('\\','\\\\').replace('"','\\"').replace('`','\\`').replace('$','\\$')+'"'
for filename,name,command,description in [
 ('Territorio Preparado.desktop','Territorio Preparado','abrir','Iniciar la aplicación y abrir el acceso'),
 ('Codigo Territorio Preparado.desktop','Código de acceso Territorio','codigo','Consultar el segundo factor vigente de la cuenta local'),
 ('Mi acceso Territorio Preparado.desktop','Mi acceso Territorio','acceso','Consultar usuario y contraseña locales'),
 ('Guia Territorio Preparado.desktop','Guía Territorio Preparado','guia','Abrir la guía local para iniciar y acceder'),
]:
    content=f'''[Desktop Entry]
Version=1.0
Type=Application
Name={name}
Comment={description}
Exec={desktop_quote(project/'territorio')} {command}
Icon={project/'backend/web/icon.svg'}
Terminal=false
Categories=Utility;
StartupNotify=false
'''
    for parent in [desktop,menu]:
        path=parent/filename;path.write_text(content);path.chmod(0o755)
        if shutil.which('gio'):subprocess.run(['gio','set',str(path),'metadata::trusted','true'],stdout=subprocess.DEVNULL,stderr=subprocess.DEVNULL)
print('Servicio de usuario y cuatro accesos instalados.')
PY
systemctl --user daemon-reload
systemctl --user enable territorio.service
if [[ "${1:-}" != --no-start ]]; then
  if ! systemctl --user is-active --quiet territorio.service && curl --fail --silent --max-time 2 http://127.0.0.1:4180/api/health >/dev/null; then
    printf '%s\n' 'Hay un servidor manual en 4180. Ciérralo antes de iniciar el servicio; no se detuvo ningún proceso ajeno.' >&2
    exit 1
  fi
  systemctl --user restart territorio.service
  ../territorio iniciar
fi
