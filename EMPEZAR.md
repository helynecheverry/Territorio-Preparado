# Empezar en un equipo nuevo

Esta guía deja el proyecto andando en una máquina recién clonada. **No necesita
permisos de administrador en ningún paso.**

## 1. Traer la rama de trabajo

```bash
git clone https://github.com/helynecheverry/Territorio-Preparado.git
cd Territorio-Preparado
git checkout feat/avisos-de-emergencia
```

El trabajo del sistema de avisos vive en `feat/avisos-de-emergencia`, no en
`main`.

## 2. Instalar Rust

Es lo único que hay que instalar aparte, y tampoco pide administrador:

```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
```

Node es opcional: solo se usa para `node --check` sobre el frontend, que no tiene
paso de compilación.

## 3. Preparar el equipo

```bash
cd backend
./scripts/preparar-equipo.sh
```

El script comprueba lo que falta, instala MongoDB en tu directorio de usuario si
no hay uno —verificando la suma que publica MongoDB antes de extraer nada—, lo
arranca como servicio de usuario, genera las claves locales e importa el
territorio de Cali, el contenido educativo y los documentos legales.

Es idempotente: volver a ejecutarlo no regenera claves ni rompe nada. Si ya
tienes tu propio MongoDB corriendo, lo usa y no instala otro.

## 4. Arrancar

```bash
./scripts/run-local.sh
```

| Dónde | Qué es |
|---|---|
| http://127.0.0.1:4180/app/alerta.html | Aplicación ciudadana: avisos, lugares, a dónde ir, qué hacer |
| http://127.0.0.1:4180/app/ | Consola de operación: reportes, albergues, administración |
| http://127.0.0.1:4180/app/planner.html | Planificador de albergues |

Sin servidor de correo configurado, el código de verificación se escribe en
`backend/var/mail/<desafío>.eml`. Para hacer administradora una cuenta ya
verificada:

```bash
./scripts/run-local.sh grant-admin tu-correo@example.com
```

## 5. Lo que no puede hacer un script

- **Revisar y publicar el contenido educativo y los términos.** Ambos exigen una
  persona identificada como revisora, que puede ser del equipo: no hace falta
  ninguna entidad externa. Son dos llamadas:
  `PATCH /api/v1/lessons/{id}` y `PATCH /api/v1/legal/terms`, con
  `{"reviewed_by": "tu nombre", "published": true}`.
- **SMS**: contratar proveedor y configurar `TP_SMS_URL` y compañía.
- **Cobro**: contratar pasarela y configurar `TP_BILLING_*`.

Mientras falten, la aplicación **no ofrece** esos canales en lugar de prometer
algo que no puede entregar. Todas las variables están documentadas en
`backend/.env.example`.

## Lo que no viaja con el repositorio

`backend/var/` está ignorado a propósito y contiene secretos:

| Archivo | Qué es |
|---|---|
| `var/secret` | Clave de sesión. Se genera sola en cada equipo |
| `var/vapid-private` | Clave privada de Web Push |
| `var/mail/` | Correo local de desarrollo, con códigos de verificación |

**Cuidado con la clave VAPID si compartes base de datos entre equipos.** Una
suscripción de notificaciones solo acepta mensajes firmados con la clave con la
que se creó. Con bases separadas por equipo no pasa nada; si comparten base, usa
la misma `TP_VAPID_PRIVATE_KEY` en todos, o las notificaciones registradas desde
un equipo fallarán desde el otro sin dar ningún error visible.

Cópiala por un gestor de contraseñas o un canal seguro, nunca pegándola en un
comando: quedaría en el historial del intérprete.

## Comprobar que todo está bien

```bash
cd backend
cargo fmt --check
cargo clippy --all-targets
cargo test --locked
cargo test --locked --test workflow -- --ignored   # necesita MongoDB
node --check web/app.js web/alerta.js web/sw.js
```

La prueba de integración crea y borra únicamente su propia base `tp_test_<UUID>`.

## Dónde está documentado cada módulo

`backend/README.md` explica la arquitectura y el porqué de cada decisión:
territorios y cobertura, ingesta de amenazas, reparto de avisos, recomendación de
destino, entrega por Web Push y SMS, contenido educativo, cobro y la aplicación
ciudadana. `backend/PLANNING.md` cubre el planificador de albergues.
