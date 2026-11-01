# Territorio Preparado · aplicación Rust

Aplicación operativa en `/app/`: registro, contraseña y segundo factor por correo, Google OIDC con segundo factor de correo, recuperación de contraseña, sesiones revocables, roles, reportes, albergues, análisis de texto y administración. API, worker, clasificación BERT y traducción Marian ejecutan Rust; MongoDB conserva los datos. No necesita el servidor Python para operar.

El prototipo anterior sigue en `/`; sus demostraciones, datos sintéticos y simulador se conservan. Sus endpoints Python no se confunden con la nueva API. La evaluación satelital xView2 sigue en el servicio experimental anterior: **no está migrada al runtime Rust**. Tampoco se han integrado pasarelas de telecomunicaciones ni conectores oficiales en vivo. No hay entrenamiento automático ni clasificación médica validada.

## Planeación de albergues

Nuevo módulo en **http://127.0.0.1:4180/app/planner.html**: huellas del inventario, exclusiones dibujadas, capacidad por espacio y recursos, dotación, guardado trazable y visualización de la propuesta en el simulador 3D. [Método, supuestos, API y pruebas](PLANNING.md). Esta fase calcula geometría y reglas; la segmentación visual SAM todavía no se ha integrado.

## Diseño 3D y mapa satelital

El planificador ahora incluye diseño 3D interactivo en la misma pantalla, interiores, servicios ilustrativos, imágenes satelitales de contexto, exportaciones PNG/GLB y registro persistente de revisiones. El explorador público incorpora un selector de mapa y satélite. [Funcionamiento, fuentes y límites](DESIGN_AND_MAPS.md).

## Inicio diario en el equipo de ijoan

El servicio de usuario `territorio.service` está habilitado para arrancar al iniciar sesión en Linux. Usa los accesos del escritorio **Territorio Preparado**, **Mi acceso Territorio**, **Código de acceso Territorio** y **Guía Territorio Preparado**. La cuenta local preparada es `ijoan@territorio.local`; su contraseña está en el archivo privado `var/MI_ACCESO.txt`, excluido de Git. [Guía PDF](../docs/GUIA_INICIO_Y_ACCESO.pdf) y [guía HTML sin conexión](../docs/GUIA_INICIO_Y_ACCESO.html).

Desde la raíz del proyecto: `./territorio abrir`, `./territorio iniciar`, `./territorio reiniciar`, `./territorio estado`, `./territorio codigo --terminal`. El ayudante local muestra únicamente códigos activos, sin consumirlos; códigos usados, bloqueados o caducados no se muestran. Se ejecuta como herramienta del usuario de Linux, fuera de HTTP, y está deshabilitado en producción o con SMTP configurado.

La instalación usa el MongoDB existente (`driveguard-mongodb.service`), base `territorio`. La aplicación diaria ejecuta la copia `var/bin/territorio-api`, no Cargo. `scripts/run-installed.sh` carga la clave privada y apunta a `var/local-config.json`; `TP_CONFIG_FILE` permite leer configuración JSON como datos, con prioridad de las variables del entorno, sin ejecutar archivos como shell. Las rutas privadas y la cuenta persisten en disco.

Para actualizar el ejecutable instalado después de cambios de Rust, ejecutar `backend/scripts/install-local-service.sh` desde la raíz. Compila, actualiza la copia de forma atómica y reinicia su servicio. Los servicios de otros proyectos y los datos compartidos no se eliminan. La configuración local presupone la ruta actual del proyecto y el usuario Linux `ijoan`.

## Arranque de desarrollo

Desde `Territorio-Preparado/backend`, con Rust y MongoDB accesible en loopback:

```bash
./scripts/run-local.sh
```

Abrir **http://127.0.0.1:4180/app/**. El script genera una clave privada en `var/secret` (modo 600). No carga archivos de entorno como código shell. Las variables del entorno prevalecen; `.env.example` documenta todas las opciones. MongoDB debe estar activo; la API falla al iniciar si no puede conectarse.

1. Crear una cuenta con una contraseña de al menos 12 caracteres.
2. Sin SMTP, leer el código en `backend/var/mail/<challenge>.eml`. Los correos se escriben únicamente en desarrollo, fuera de los directorios publicados, con permisos privados. No se devuelve el código por la API.
3. Introducir el código en la interfaz. Todas las cuentas nuevas son ciudadanas.
4. Para habilitar al primer administrador, después de verificar su correo:

```bash
./scripts/run-local.sh grant-admin tu-correo@example.com
```

La administración web permite habilitar operadores y administradores y deshabilitar cuentas. No permite modificar los permisos propios. No existen cuentas ni contraseñas administrativas predeterminadas.

Para instalar en un equipo limpio: Rust >= 1.88 (el lock se verificó con 1.98.1), compilador C para `onig`, MongoDB y los pesos locales. `cargo build --locked --release` compila el ejecutable. Las rutas predeterminadas de modelos, web y frontend presuponen que el directorio actual es `backend`.

## Correo y Google

Correo: configurar `TP_SMTP_HOST`, `TP_SMTP_PORT` (587), `TP_SMTP_USER`, `TP_SMTP_PASSWORD`, `TP_MAIL_FROM`. El transporte exige STARTTLS y valida certificados. Preparar remitente/dominio en el proveedor. Producción rechaza el arranque sin SMTP, clave de aplicación y origen HTTPS.

Google: crear un cliente OAuth de aplicación web y configurar `TP_GOOGLE_CLIENT_ID` y `TP_GOOGLE_CLIENT_SECRET`. Registrar exactamente `${TP_ORIGIN}/api/auth/google/callback`; para desarrollo: `http://127.0.0.1:4180/api/auth/google/callback`. La opción aparece en la interfaz solo cuando ambas variables están configuradas. Requiere red hacia Google. El backend verifica firma, emisor, audiencia, expiración, nonce, correo verificado, state vinculado al navegador y PKCE mediante `openidconnect`. Después exige nuestro código de correo.

No se fusionan cuentas de contraseña y Google automáticamente por compartir correo. Las cuentas ya creadas con contraseña continúan usando contraseña. No se guardan access/refresh tokens de Google. La validación de punta a punta de Google y SMTP requiere credenciales reales; no están suministradas en el repositorio.

## IA local en Rust

Pesos BERT esperados: `models/sos/{config.json,tokenizer.json,model.safetensors}`. El motor Candle aplica tokenización de hasta 128 tokens, BERT, pooler, clasificador y sigmoid sobre 36 salidas. Registra SHA256 de los pesos, idioma, traducción y advertencias de revisión en cada resultado. No inventa nombres para las etiquetas genéricas del modelo original.

Traducción: `models/translation-es-en/{config.json,source.spm,vocab.json,model.safetensors}`. Usa Marian y SentencePiece en Rust con decodificación greedy, hasta 511 tokens de entrada y 256 generados. Requiere revisión de calidad; no es idéntica al beam search del motor Python anterior. Un mensaje demasiado largo falla explícitamente en lugar de truncarse en silencio durante la traducción.

El `pytorch_model.bin` antiguo del proyecto usa un formato de serialización que Candle no puede cargar. La conversión **única** a safetensors se hace sin entrenar ni alterar valores, con el entorno de aprovisionamiento existente:

```bash
# Desde la raíz del repositorio; no forma parte del backend ni de su ejecución.
servidor/ia/.venv/bin/python scripts/prepare_translation.py
```

Guarda hashes de origen y destino en `models/translation-es-en/rust-conversion.json`. Si los pesos no están instalados, seguir el aprovisionamiento explícito descrito en `servidor/ia/README.md`; no hay descargas automáticas durante solicitudes.

Diagnóstico directo, desde `backend`:

```bash
cargo run --locked --release -- infer en 'We need drinking water and shelter'
cargo run --locked --release -- infer es 'Necesitamos agua potable y un refugio para las familias'
```

Los operadores guardan un reporte y pulsan **Analizar con IA**. La API encola el trabajo de forma durable; el worker reclama mediante una operación atómica y un lease con heartbeat. Se guardan errores explícitos si faltan pesos; no hay resultados simulados. Los trabajos interrumpidos se recuperan al vencer el lease; tres interrupciones los dejan fallidos. El operador puede reintentar un fallo. Se requiere clave de idempotencia al encolar. Un worker procesa una inferencia a la vez para acotar memoria; no se ejecutan inferencias en el runtime HTTP.

## Módulos y escalado

```text
src/
  main.rs             composición, modos y señales
  config.rs           configuración y validación de entorno
  db.rs               conexión, índices y auditoría
  security.rs         origen, cookies, hashes y límites compartidos
  auth/
    mod.rs            credenciales, segundo factor, recuperación y sesiones
    google.rs         OIDC con PKCE
    mail.rs           transporte SMTP / correo local privado
  domain/
    mod.rs            composición de módulos, paginación y tablero
    reports.rs        reportes y revisión
    shelters.rs       albergues y capacidad
    admin.rs          usuarios, roles y auditoría
  ai/
    mod.rs            cola durable, arrendamiento y worker
    bert.rs           inferencia BERT nativa
    translation.rs    traducción Marian nativa
web/                  interfaz sin dependencias de compilación Node
```

Tres modos: `serve` (API + worker, desarrollo), `api` (solo HTTP), `worker` (solo inferencia). Para añadir un módulo se define su router con `AppState`, colecciones/índices, validaciones y permisos; se registra en la composición de `main.rs`. Retirar una ruta no elimina datos. Los módulos de IA y autenticación mantienen sus implementaciones en archivos separados.

Las API comparten MongoDB, sesiones, desafíos y contadores de límites; no hay sesiones en memoria ni dependencia de afinidad al balanceador. Los workers pueden multiplicarse independientemente. El lease es una garantía de procesamiento **al menos una vez**, con escritura final condicionada por token de propietario; no promete exactamente una inferencia física por trabajo ante una partición prolongada. Sin broker adicional, MongoDB coordina los trabajos. Las listas usan páginas de 50 registros con orden estable.

El Compose suministrado usa un MongoDB único con volumen. Permite escalar procesos API y workers, pero **no proporciona alta disponibilidad de la base de datos**. Para HA se requiere replica set/servicio Mongo administrado, backups probados y balanceador de producción. Evitar sobreasignar memoria: los pesos de BERT y Marian se cargan por worker.

## API y permisos

Todas las escrituras HTTP requieren `Origin: ${TP_ORIGIN}`. El navegador lo envía automáticamente. Cookie `tp_session` HttpOnly, SameSite=Lax, Secure en producción; duración absoluta de 8 horas. No guardar tokens en localStorage. No se habilita CORS entre orígenes.

| Ruta | Método | Acceso |
|---|---|---|
| `/api/health` | GET | Público, prueba MongoDB |
| `/api/capabilities` | GET | Público, configuración de acceso |
| `/api/auth/register`, `/login`, `/verify`, `/forgot`, `/reset` | POST | Público, validación y límites |
| `/api/auth/google`, `/google/callback` | GET | OIDC |
| `/api/auth/me` | GET | Sesión |
| `/api/auth/logout`, `/logout-all` | POST | Cierre de sesiones |
| `/api/v1/reports` | GET, POST | Sesión; ciudadanos solo ven los propios |
| `/api/v1/reports/{id}` | PATCH | Operador/admin, cambiar estado |
| `/api/v1/shelters` | GET | Sesión |
| `/api/v1/shelters` | POST | Operador/admin |
| `/api/v1/shelters/{id}` | PATCH | Operador/admin |
| `/api/v1/jobs` | GET, POST | Operador/admin |
| `/api/v1/jobs/{id}/retry` | POST | Operador/admin |
| `/api/v1/dashboard` | GET | Operador/admin |
| `/api/v1/admin/users` | GET | Admin, sin hashes ni identidad Google |
| `/api/v1/admin/users/{id}` | PATCH | Admin |
| `/api/v1/admin/audit` | GET | Admin |

Reportes POST: `{ "text":"Necesitamos agua", "zone":"Cali", "latitude":3.45, "longitude":-76.53 }`; coordenadas opcionales, ambas o ninguna. Estados: `new`, `reviewing`, `resolved`, `dismissed`. Trabajo POST: `{ "report_id":"UUID", "language":"es", "request_key":"UUID generado por el cliente" }`. Albergue POST/PATCH: `{ "name":"Colegio", "zone":"Cali", "capacity":100, "occupied":20 }`. Permisos PATCH: `{ "role":"operator", "disabled":false }`. Las rutas que listan aceptan `?page=0`.

## Seguridad implementada y operación

Contraseñas Argon2id con sal aleatoria, operaciones de hash fuera del executor HTTP y hash señuelo al fallar usuario. OTP de seis cifras mediante CSPRNG, almacenado con HMAC y clave externa, válido diez minutos, cinco intentos, consumo atómico y propósito separado para acceso/recuperación. Las expiraciones se comprueban en consultas además del TTL de MongoDB. Sesiones aleatorias de 256 bits, solo hashes en DB; cambios de contraseña/permisos y cierre global incrementan la versión de autenticación, invalidando también sesiones emitidas concurrentemente con versión anterior.

Se limita correo/usuario y dirección del peer en MongoDB, compartido entre réplicas. El proxy se considera un peer: no se confía en `X-Forwarded-For` arbitrario. El despliegue detrás de proxy necesita límites adicionales por cliente en la entrada; el límite de peer agrupa clientes de ese proxy. El cuerpo JSON se limita a 32 KiB; se validan coordenadas, capacidad/ocupación y campos de operaciones. La UI usa textContent para datos de usuario. Hay CSP restrictiva en `/app` y `/api`, protección de framing, no-sniff, no-store y HSTS en producción. El explorador público envía `Referrer-Policy: strict-origin-when-cross-origin` para identificar el origen ante OpenStreetMap, sin divulgar rutas a terceros. Permite marcos del mismo origen (`SAMEORIGIN`) para su simulador integrado; `/app` y `/api` mantienen `no-referrer`, `DENY` y CSP restrictiva.

La auditoría guarda actor, acción, destino y fecha, nunca contraseñas, códigos ni textos de los reportes. Las operaciones de negocio registran intención antes de la escritura; una acción auditada puede haber fallado después. No es un registro inmutable ni una transacción de dos colecciones; para ese requisito se necesita almacenamiento de auditoría externo y/o transacciones con replica set. El correo local de desarrollo contiene códigos caducados: limpiar `var/mail` periódicamente. No publicar ese directorio.

Cifrado de discos/backups y política de retención de reportes corresponden a la infraestructura y al responsable de datos; no los activa mágicamente MongoDB. La plantilla restringe la red de la base, exige usuario de aplicación y termina HTTPS con Caddy. Nunca exponer el puerto 27017. Para MongoDB remoto, usar TLS con certificados válidos y credenciales con permisos mínimos. No se ha realizado auditoría de penetración independiente.

## Despliegue preparado

Desde `backend`:

```bash
cp deploy/production.env.example .env.production
chmod 600 .env.production
# Completar origen/dominio, SMTP, Google y tres secretos distintos (openssl rand -hex 32).
docker compose --env-file .env.production -f deploy/compose.yml config --quiet
docker compose --env-file .env.production -f deploy/compose.yml build api
docker compose --env-file .env.production -f deploy/compose.yml up -d
# Con recursos suficientes:
docker compose --env-file .env.production -f deploy/compose.yml up -d --scale worker=2
```

El usuario Mongo de aplicación se crea solo al inicializar un volumen vacío. Rotar su contraseña requiere actualizar el usuario en MongoDB y la configuración; cambiar el archivo de entorno no modifica usuarios existentes. El contenedor de aplicación no tiene socket Docker, ejecuta con usuario sin privilegios, filesystem de solo lectura y sin capabilities. Caddy requiere DNS/dominio y puertos 80/443. Mantener secretos fuera de Git y con acceso restringido. No se ha publicado ni desplegado externamente.

El Docker daemon de este equipo no permite acceso al usuario actual; construcción/arranque de contenedores no se han validado aquí. El binario, HTTP, MongoDB e inferencia se verifican directamente en el host.

## Verificación reproducible

```bash
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test --locked
# Crea y borra únicamente su propia DB tp_test_<UUID> en MongoDB:
cargo test --locked --test workflow -- --ignored
node --check web/app.js
```

La prueba integral comprueba origen, segundo factor, bloqueo de OTP, roles, aislamiento por propietario, validaciones, idempotencia secuencial, persistencia tras reinicio, recuperación y revocación de sesiones. No sustituye la prueba con proveedores reales ni una prueba de carga.

Referencias de implementación: [driver Rust de MongoDB](https://www.mongodb.com/docs/drivers/rust/current/), [OpenID Connect de Google](https://developers.google.com/identity/openid-connect/openid-connect), [Candle](https://github.com/huggingface/candle), [modelo BERT original](https://huggingface.co/AventIQ-AI/Bert-Disaster-SOS-Message-Classifier).

## Evidencia de esta entrega

[Verificación local del 31 de octubre de 2026](../docs/evidence/rust-backend-2026-10-31.json): tres pruebas unitarias, un escenario integral HTTP/MongoDB, compilación optimizada, Clippy y flujo de navegador con inferencia real en español. BERT nativo se comparó con el motor Python anterior para un mensaje en inglés: diferencia absoluta máxima de 0,000000578 en 36 puntuaciones. Es paridad de implementación para un ejemplo, no una evaluación de exactitud del modelo. La cuenta administrativa temporal y sus registros fueron eliminados al finalizar la prueba.
