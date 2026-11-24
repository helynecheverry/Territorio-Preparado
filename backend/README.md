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
4. Importar el inventario de un territorio, que el planificador necesita para
   funcionar. El inventario ya no se carga desde un archivo al arrancar:

```bash
./scripts/run-local.sh import-territory territories/co-cali.json ../frontend/dist/data/spaces.json
```

   El comando es idempotente —reimportar el mismo archivo no cambia nada— y
   nombra en el registro cada huella que no pasa la validación geométrica y
   cada capa de amenaza ausente. Para añadir una ciudad basta declararla en
   `territories/<id>.json` e importar su GeoJSON; no hay que recompilar.

5. Para habilitar al primer administrador, después de verificar su correo:

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
  territory.rs        territorios, capas de amenaza y nivel de cobertura
  sectors.rs          límites administrativos y resolución de un punto a su sector
  subscriptions.rs    afiliación por ubicación, canales y consentimiento
  destinations.rs     a dónde ir: candidatos filtrados por amenaza
  hazards/
    mod.rs            ingesta, agrupación de revisiones y umbrales de aviso
    usgs.rs           catálogo FDSN de USGS, piso universal
    sgc.rs            autoridad nacional colombiana, pendiente de acceso a API
  alerts/
    mod.rs            reparto, deduplicación, presupuesto y cola de avisos
    entitlements.rs   planes y derecho a cada canal
  delivery/
    mod.rs            cola, arrendamiento, reintentos y dispositivos de push
    webpush.rs        Web Push: cifrado RFC 8291 y firma VAPID RFC 8292
    sms.rs            SMS contra el proveedor propio, por plantilla
    message.rs        redacción del aviso, con el destino dentro
  lessons.rs          contenido educativo, barrera de revisión y paquete sin red
  legal.rs            documentos versionados y aceptación registrada
  billing.rs          cobro: referencia propia, webhook firmado y conciliación
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

El inventario de espacios también vive en MongoDB, no en la memoria de cada
proceso, así que un despliegue nacional no multiplica el consumo de memoria por
ciudad ni exige que todas las réplicas lleven el mismo archivo de datos.

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
| `/api/v1/territories` | GET | Sesión; territorios y nivel de cobertura |
| `/api/v1/sectors?territory=co-cali&kind=commune` | GET | Sesión; barrios o comunas |
| `/api/v1/subscriptions` | GET, POST | Sesión; solo las afiliaciones propias |
| `/api/v1/subscriptions/{id}` | PATCH, DELETE | Sesión; solo las propias |
| `/api/v1/subscriptions/{id}/events` | GET | Sesión; eventos que alcanzaron esa ubicación |
| `/api/v1/events` | GET | Sesión; catálogo sísmico ingerido |
| `/api/v1/alerts/mine` | GET | Sesión; avisos decididos para la propia cuenta |
| `/api/v1/alerts/queue` | GET | Operador/admin; cola en orden de reparto |
| `/api/v1/alerts/entitlements/{owner}` | PATCH | Admin; concede o retira un plan |
| `/api/v1/destinations?latitude=&longitude=&hazard=earthquake` | GET | Sesión; candidatos por amenaza |
| `/api/v1/subscriptions/{id}/destinations?hazard=flood` | GET | Sesión; igual, desde una afiliación propia |
| `/api/v1/push-public-key` | GET | Sesión; clave VAPID para `pushManager.subscribe` |
| `/api/v1/push-devices` | GET, POST | Sesión; navegadores propios |
| `/api/v1/push-devices/{id}` | DELETE | Sesión; solo los propios |
| `/api/v1/lessons` | GET | Sesión; publicadas. `?drafts=true` exige operador |
| `/api/v1/lessons/bundle?locale=es-CO` | GET | Sesión; todo el contenido con `ETag`, para guardar sin red |
| `/api/v1/lessons/{id}` | PATCH | Operador/admin; revisa y publica o retira |
| `/api/v1/legal/{id}` | GET | Sesión; documento vigente y si lo aceptó |
| `/api/v1/legal/{id}/accept` | POST | Sesión; acepta una versión concreta |
| `/api/v1/legal/{id}` | PATCH | Admin; revisa y publica o retira |
| `/api/v1/billing/plans`, `/billing/me` | GET | Sesión |
| `/api/v1/billing/checkout` | POST | Sesión; exige términos aceptados |
| `/api/v1/billing/webhook` | POST | Pasarela; firma HMAC, sin sesión ni `Origin` |
| `/api/v1/dashboard` | GET | Operador/admin |
| `/api/v1/admin/users` | GET | Admin, sin hashes ni identidad Google |
| `/api/v1/admin/users/{id}` | PATCH | Admin |
| `/api/v1/admin/audit` | GET | Admin |

Reportes POST: `{ "text":"Necesitamos agua", "zone":"Cali", "latitude":3.45, "longitude":-76.53 }`; coordenadas opcionales, ambas o ninguna. Estados: `new`, `reviewing`, `resolved`, `dismissed`. Trabajo POST: `{ "report_id":"UUID", "language":"es", "request_key":"UUID generado por el cliente" }`. Albergue POST/PATCH: `{ "name":"Colegio", "zone":"Cali", "capacity":100, "occupied":20 }`. Permisos PATCH: `{ "role":"operator", "disabled":false }`. Las rutas que listan aceptan `?page=0`. Afiliación POST: `{ "label":"Casa", "latitude":3.45, "longitude":-76.53, "channels":{"push":true,"sms":false}, "min_level":"info", "consent":true }`; el consentimiento es obligatorio, el punto debe caer dentro del territorio y activar `sms` exige un teléfono en formato E.164. Las respuestas devuelven solo los cuatro últimos dígitos del teléfono y la auditoría registra el identificador de la afiliación, nunca las coordenadas.

## Avisos de emergencia

**Esto no es alerta temprana.** Los catálogos sísmicos oficiales publican un
evento entre uno y cinco minutos después de que ocurre, así que lo que entra por
la ingesta es siempre posterior al sismo. El valor está en la decisión que viene
después —qué hacer y a qué albergue ir, evitando los que están en zona inundable
o de suelo licuable—, no en la velocidad del aviso. La detección en segundos
requiere instrumentación de ondas P, y en Android el sistema de alertas sísmicas
de Google ya cubre ese hueco sin coste.

### Fuentes

`hazards::EventSource` abstrae el catálogo para que añadir un país sea añadir una
implementación. **USGS** es el piso universal: servicio FDSN documentado, estable
y sin credenciales. El **SGC** es la autoridad para Colombia y tiene precedencia,
pero no publica una API: su portal carga los sismos con un `POST ajax/query` de
Django protegido por CSRF y sin contrato documentado, y `www.sgc.gov.co` responde
403 a clientes que no se presentan como navegador. La fuente queda implementada y
conectada, desactivada hasta que `TP_SGC_URL` apunte a un servicio compatible con
FDSN. El camino correcto es solicitar acceso al SGC, no raspar una página que
puede cambiar sin aviso justo cuando el portal esté bajo carga por un sismo.

### Revisiones

Un mismo sismo se publica varias veces: el evento de Panamá que ya estaba en los
datos del proyecto pasó de M8.0 a M7.7 al revisarse. Cada observación se guarda
por `(fuente, identificador)` y las del mismo sismo se agrupan bajo un
`canonical_id`, con una ventana de 90 segundos y 100 km. Reingerir el mismo feed
no duplica nada. `notified_level` conserva el nivel máximo ya avisado para que una
revisión no vuelva a notificar salvo que **suba** de nivel.

### Umbrales

`hazards::RULES` define el nivel de aviso —`info`, `prepare`, `evacuate`— por
bandas de magnitud y distancia epicentral. **No son un modelo de intensidad
macrosísmica**: no incorporan tipo de suelo, profundidad, mecanismo focal ni
atenuación regional. Son umbrales operativos escritos para que un especialista
los lea y los corrija de un vistazo, y por eso cada evento conserva
`validation_level: "automatic"` y el sistema opera en modo informar y recomendar,
sin convenio con la autoridad de gestión del riesgo.

El nivel depende de la persona y no del sismo: un M7 es `evacuate` a 50 km,
`prepare` a 400 km e `info` a 900 km. `reach_km` guarda con cada evento la
distancia máxima a la que produce algún aviso, lo que permite recuperar a los
afectados con una sola consulta geoespacial indexada en lugar de evaluar la
magnitud contra cada afiliación.

### Reparto

El motor decide y encola; **no envía**. El transporte por Web Push y SMS es un
módulo aparte (ver [Entrega](#entrega)) que consume la colección `notifications`.
Separarlo así permite verificar el criterio antes de tocar el canal, en lugar de
descubrir un error de juicio cuando ya se mandó el mensaje.

Cada aviso tiene un `_id` determinista `canónico:afiliación:canal` con clave
única, y la escritura se filtra por `level_rank` menor que el nuevo. De ahí salen
las tres ramas: la primera vez se inserta; una revisión que **sube** de nivel
actualiza y vuelve a dejarlo pendiente; una revisión que baja o repite choca con
la clave y se cuenta como omitida. Esa última es deliberada: el sismo de Panamá
se publicó como M8.0 y se revisó a M7.7, y mandar un segundo mensaje para decir
que fue algo menos fuerte solo gastaría la confianza de la gente y, por SMS,
dinero. Las dos reglas —no bajar de nivel y no duplicar entre catálogos— están
cubiertas por pruebas de integración verificadas con mutación del código.

Cuando dos catálogos publican el mismo sismo, solo reparte el de mayor
`authority_rank` del grupo canónico; el otro se descarta sin avisar. Sin esa
regla, cada persona recibiría un aviso de USGS y otro del SGC por un solo sismo.

### Escala del reparto

Los afiliados se recuperan con `$geoNear` sobre el índice `2dsphere`, acotado al
alcance del evento. Devuelve **del más cercano al más lejano**, y ese orden se
conserva en la cola: quien está más cerca del epicentro tiene menos tiempo y más
necesidad. El cursor se consume por tramos y las escrituras de cada tramo se
emiten con concurrencia acotada (`TP_ALERT_CONCURRENCY`); de forma secuencial, un
reparto de cientos de miles de avisos tardaría minutos.

El SMS tiene presupuesto por evento (`TP_ALERT_SMS_BUDGET_PER_EVENT`). Al
agotarse, el resto se marca como omitido por presupuesto en lugar de gastarse en
silencio, y como el recorrido va del más cercano al más lejano, el presupuesto se
gasta en quien más lo necesita. `territorio-api alerts-pass` fuerza una pasada sin
esperar al temporizador.

### Planes

El plan gratuito recibe **todos los niveles por Web Push**. Dejar sin aviso a
quien no paga, cuando avisarle no cuesta nada, sería indefendible si alguien
resultara herido en una emergencia. Lo que añade el plan de pago es el **SMS en
nivel `evacuate`**: el canal que sigue funcionando cuando el teléfono no tiene
datos, que es justo lo que pasa en una emergencia y lo que cuesta por envío. Eso
acota el coste variable y justifica la cuota por algo real.

Mientras no haya pasarela de pago, un administrador concede el plan desde
`/api/v1/alerts/entitlements/{owner}`, con la concesión auditada y un campo para
la referencia del pago. Al integrar la pasarela, su webhook escribirá en la misma
colección y el motor no cambiará.

### A dónde ir

`docs/DATASETS.md` documenta el hallazgo que da sentido al proyecto: tras el
sismo del 10 de agosto de 2026 se montaron albergues autogestionados en los
parques de Chiminangos y Calimio, que tienen amenaza **alta** de inundación,
están sobre suelo **licuable** y quedan a entre 72 y 152 m del dique del río
Cauca. La gente huyó del sismo hacia el lugar de mayor riesgo de inundación,
porque era el espacio abierto que tenía al lado.

`destinations.rs` convierte eso en una respuesta: no «el espacio abierto más
cercano» sino «el más cercano que además sirve para esta amenaza». Y cuando
descarta algo más cercano **lo nombra con su motivo literal del inventario**,
porque si alguien va a caminar casi un kilómetro en vez de cruzar la calle
necesita saber por qué.

Ejecutado sobre el inventario real, parado en el parque de Chiminangos I donde se
montó el albergue, el resultado es:

```text
Amenaza: earthquake | comprobado: ['liquefaction', 'flood'] | sin verificar: []

DESCARTADOS (más cerca que el recomendado):
   0.0 km  13804 m²  Parque · Chiminangos I            → liquefaction+flood
  0.08 km   6525 m²  Parque · Metropolitano del Norte  → liquefaction+flood
  ... 77 descartados en total

RECOMENDADOS:
  0.93 km   2562 m²  hasta 56 pers.  Parque · Los Parques Barranquilla
```

Tras un sismo la gente se queda en el destino durante días, así que un destino
sísmico se comprueba contra licuación **y** contra amenaza alta de inundación.
Para una inundación el criterio es más estricto y la amenaza media también
descarta.

### Ausencia de dato no es ausencia de riesgo

Las propiedades `flood` y `seismic` del inventario son intersecciones con las
capas cargadas, así que un arreglo vacío significa «no intersecta con lo que
tenemos», nunca «es seguro». `PLANNING.md` ya fija ese principio y la
recomendación lo respeta en dos niveles: cada espacio se declara como
`sin_amenaza_declarada` en lugar de seguro, y si al territorio le falta la capa
que la amenaza necesita, esa dimensión sale en `unverified`. Sin lo segundo, una
ciudad sin capa de inundación daría arreglos vacíos indistinguibles de los de una
ciudad donde sí se comprobó, y la recomendación mentiría sin que nadie lo notara.

La distancia es **en línea recta**, no una ruta a pie: no sabe de puentes, del
dique, de calles bloqueadas ni de un paso elevado caído, que es justo lo que
cambia tras un sismo. La capacidad es una cota superior por área según la
referencia de ACNUR de 45 m² por persona; para una propuesta con distribución,
exclusiones y recursos está `/api/v1/planning/preview`.

### Entrega

El motor de alertas decide y encola; `delivery` entrega. Esa frontera permitió
verificar el criterio antes de que existiera el canal, y permite cambiar de
proveedor de SMS sin volver a razonar sobre umbrales.

**Web Push** es el canal sin coste por mensaje: el servidor cifra y firma el
aviso y lo entrega al servicio de push del propio navegador, sin intermediario
que cobre. El cifrado RFC 8291 y la firma VAPID RFC 8292 los construye
`web-push-native`, en Rust puro. Se eligió frente al crate `web-push`, más
conocido, porque aquel arrastra `ece` con OpenSSL y este proyecto es
deliberadamente libre de OpenSSL; el envío va con reqwest y rustls como todo lo
demás.

Para arrancar el canal:

```bash
./scripts/run-local.sh vapid-keys
```

Imprime `TP_VAPID_PRIVATE_KEY` para la configuración y la clave pública que el
navegador pasa a `pushManager.subscribe`. La misma clave pública se sirve en
`GET /api/v1/push-public-key`. `TP_VAPID_CONTACT` es obligatorio: RFC 8292 lo
exige en la reclamación `sub` y es cómo el servicio de push avisa al operador si
algo va mal con sus envíos.

**SMS** se configura contra el proveedor propio, porque el coste por mensaje
varía mucho y atar el código a un proveedor obligaría a tocar Rust para cambiar
de tarifa:

| Variable | Qué es |
|---|---|
| `TP_SMS_URL` | Extremo del proveedor. Vacío desactiva el canal |
| `TP_SMS_AUTH_HEADER` | `Nombre: valor`. Es un secreto y no se registra |
| `TP_SMS_CONTENT_TYPE` | `json` o `form` |
| `TP_SMS_BODY` | Plantilla con los marcadores `{{to}}` y `{{text}}` |

No se incluyen plantillas de proveedores concretos: no están verificadas contra
sus servicios y una plantilla equivocada que parezca oficial es peor que no tener
ninguna. Se escribe leyendo la documentación del proveedor que se contrate. La
sustitución escapa cada marcador según el tipo de contenido, así que el texto del
aviso —que lleva comillas y acentos— no puede romper el cuerpo ni inyectar
parámetros.

Producción exige https en los dos transportes. En desarrollo se admite http en
loopback, igual que ya hace `config.rs` con `TP_ORIGIN`, para poder probar el
canal contra un servidor local sin montar certificados.

### Reintentos y limpieza

Cada resultado es una de cuatro decisiones, y la distinción importa:

| Resultado | Qué pasa |
|---|---|
| `Sent` | Entregado al servicio o al proveedor. **No** significa que se haya leído: ninguno de los dos canales lo garantiza |
| `Retry` | Red, 429 o 5xx. Vuelve a pendiente con espera que se duplica por intento, hasta una hora |
| `Permanent` | Cuerpo rechazado, firma inválida, credenciales mal. Reintentar solo gastaría intentos |
| `Gone` | 404 o 410: el navegador canceló la suscripción. El dispositivo **se borra**, porque un registro muerto consume un envío en cada emergencia |

El arrendamiento es por lotes y no de uno en uno como en `ai::worker`: allí cada
trabajo tarda segundos de CPU y tomarlos uno a uno es correcto, aquí hay que
mover cientos de miles de mensajes y un viaje a la base de datos por mensaje para
reclamarlo sería el cuello de botella. `territorio-api deliver-pass` fuerza un
lote sin esperar al temporizador.

### El texto del aviso

Es lo único que una persona va a leer, posiblemente de madrugada y asustada, así
que dice **que ya pasó** («Sismo M6.5 hace 2 min») y no «alerta de sismo»; dice a
dónde ir y por qué no al de al lado; y recomienda en lugar de ordenar, nombrando
su fuente. La búsqueda de destino solo se hace para `evacuate` —en `info` nadie se
mueve— y se memoriza por manzana, redondeando a unos 100 m, para que un sismo que
alcanza a un barrio completo haga unas pocas consultas geoespaciales en lugar de
una por afiliado.

### Contenido educativo

En una emergencia la red de datos es lo primero que se degrada, así que un
contenido cuyo único soporte sea un video de decenas de megabytes es inútil
exactamente cuando hace falta. Por eso cada lección tiene **pasos en texto** como
contenido obligatorio y el video como complemento, y por eso existe
`GET /api/v1/lessons/bundle`: devuelve todo lo publicado en una sola respuesta con
`ETag`, para que la aplicación lo guarde **antes** de la emergencia y lo sirva sin
red. Revalidar cuesta un 304 sin cuerpo.

El aviso push lleva `hazard_type`, así que el cliente puede abrir la lección que
corresponde desde el paquete que ya tiene guardado, sin pedir nada a la red en ese
momento.

### Nada se publica sin revisión humana

Estas instrucciones pueden determinar si alguien se queda bajo un marco de puerta
o corre hacia la calle. No es contenido que pueda generar un sistema automático ni
redactar quien programa el backend. Publicar exige tres cosas, **comprobadas por
la API** y no por costumbre:

1. Pasos para la fase `during`, que es la que decide. Una lección que solo explica
   cómo prepararse no sirve cuando el suelo ya se está moviendo.
2. Al menos una fuente citada.
3. Una persona identificada en `reviewed_by`. **Puede ser alguien del equipo**: la
   barrera no exige una entidad externa ni un convenio, solo que alguien quede
   como responsable del texto que la gente va a seguir.
4. Si hay video, que su dirección sea `https`.

```bash
./scripts/run-local.sh import-lessons lessons/co-es.json
```

Importar **siempre** deja el contenido en borrador, sin importar lo que diga el
archivo, y nombra en el registro qué le falta a cada lección. Reimportar
despublica: un cambio de redacción en una instrucción de emergencia merece la
misma revisión que la original.

`lessons/co-es.json` es un **borrador de partida**, no contenido aprobado. Recoge
la guía pública y establecida de las entidades citadas —UNGRD, Servicio Geológico
Colombiano, CVC y Cruz Roja Colombiana—, pero no lo redactó un especialista de
este equipo: `reviewed_by` está vacío y la referencia exacta de cada fuente queda
pendiente. Quien revise debe contrastarlo con la publicación vigente de cada
entidad y adaptarlo al contexto local antes de publicar.

### Cobro

El diseño descansa en una decisión: **nunca se confía en que la pasarela diga de
quién es el pago.** `POST /billing/checkout` genera aquí una referencia propia con
la cuenta y el plan ya atados a ella, y la pasarela solo informa de que *esa
referencia* se pagó. Si la cuenta viniera en el cuerpo del webhook, falsificar un
evento otorgaría el plan a cualquiera. Por el mismo motivo se compara el importe:
sin eso, pagar mil pesos de un plan de veinte mil daría el plan igual.

El webhook no se autentica por cookie —lo llama un servidor, no un navegador—
sino con HMAC-SHA256 sobre el cuerpo **crudo**, en comparación de tiempo
constante, y falla cerrado: sin `TP_BILLING_WEBHOOK_SECRET` responde 503 en lugar
de aceptar lo que llegue. Es además la única ruta exenta de la comprobación de
`Origin` de `security::guard`, y la exención es segura porque esa comprobación
protege contra CSRF —un sitio ajeno haciendo que el navegador emita una escritura
autenticada por cookie— y aquí no hay sesión que aprovechar.

Tres defensas más, cada una contra un ataque distinto:

| Defensa | Qué impide |
|---|---|
| Ventana temporal | Reenviar un evento antiguo con su firma válida |
| Idempotencia por identificador de evento | Que un reintento de la pasarela sume otro mes |
| Libro de eventos | Quedarse sin rastro para conciliar un cobro disputado |

Lo que es configurable es el proveedor: cada pasarela firma distinto y nombra sus
campos distinto, y la elección —Mercado Pago, dLocal, Wompi, ePayco— depende de
los países que se cubran. Lo que **no** es configurable es ninguna de las defensas
de arriba. No hay plantillas de proveedores concretos: no están verificadas, y en
una firma de webhook una plantilla equivocada significa o rechazar cobros reales o
aceptar eventos falsificados.

### Términos, y por qué van antes del cobro

El proyecto opera en modo informar y recomendar, sin convenio con la autoridad de
gestión del riesgo. En cuanto se cobra una cuota, esa postura deja de ser una
decisión interna y pasa a ser una afirmación comercial: hay que poder demostrar
**qué** se vendió y **que la persona lo aceptó**. Por eso `checkout` exige que la
versión vigente de los términos esté aceptada, y la aceptación se registra contra
esa versión concreta: cambiar el texto obliga a volver a aceptarlo.

```bash
./scripts/run-local.sh import-legal legal/co-es.json
```

`legal/co-es.json` es un **borrador y no es asesoría legal**. Recoge las
decisiones de producto ya tomadas —no es alerta temprana; se cobra el monitoreo y
el contenido, no la entrega— para que un abogado trabaje sobre algo concreto. Le
faltan al menos la razón social y el NIT del responsable, el régimen de
protección de datos aplicable, el canal de atención al consumidor, la política de
retracto y reembolso, y la jurisdicción. La API no lo publica hasta que alguien
quede identificado como revisor.

### Datos personales

La ubicación de la vivienda de alguien es dato sensible. El consentimiento es
explícito y queda registrado con la versión del texto aceptado; las coordenadas
no se escriben nunca en el registro de la aplicación; borrar una afiliación
elimina el documento en lugar de marcarlo como inactivo; y ningún rol puede
listar las afiliaciones de otra persona por la API.

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

## Aplicación ciudadana

`web/` tenía solo la consola de operación. La aplicación que usa una persona
afiliada vive en `/app/alerta.html`, sin dependencias ni paso de compilación como
el resto, y está pensada primero para un teléfono: objetivos de toque grandes,
barra de pestañas al alcance del pulgar y el nivel del aviso legible de un
vistazo. El color nunca es el único portador de significado —cada nivel lleva su
palabra— porque hay quien no distingue rojo de verde y porque a pleno sol la
pantalla pierde saturación.

| Archivo | Qué hace |
|---|---|
| `web/alerta.html`, `alerta.js`, `alerta.css` | Acceso, lugares, destinos, guía y plan |
| `web/sw.js` | Recibe los avisos de push, y guarda la guía y el armazón |
| `web/manifest.webmanifest` | Instalable como aplicación |

El trabajador de servicio es lo que hace que un aviso llegue con la aplicación
cerrada. Guarda el contenido educativo y el armazón, y **no guarda ninguna otra
respuesta de la API**: servir una recomendación de albergue vieja sería peor que
no servir nada, porque mandaría a alguien a un sitio elegido con datos de otro
momento.

La guía se guarda desde la propia aplicación y no se confía en que lo haga el
trabajador de servicio: en la primera visita este todavía no controla la página
cuando se pide el paquete, y el contenido tiene que estar en el dispositivo
**antes** de la emergencia.

El permiso de notificaciones se pide solo tras un toque explícito. Pedirlo al
cargar penaliza el sitio en los navegadores, y alguien que lo rechaza por
sorpresa es alguien a quien ya no se podrá avisar.

### No se ofrece lo que no se puede entregar

`GET /api/capabilities` informa de los canales **realmente** configurados y de si
hay pasarela. La aplicación se ajusta a eso: sin proveedor de SMS la casilla no
aparece, y sin pasarela el plan de pago dice que no se puede contratar todavía.
El backend lo refuerza y rechaza una afiliación a un canal sin proveedor, en
lugar de aceptarla y descubrirlo durante la emergencia.

### Video

Un archivo de video propio —por ejemplo en R2— se reproduce dentro de la
aplicación y el navegador puede guardarlo. Un video de una plataforma se abre
fuera, con una nota de que necesita conexión: incrustarlo metería el rastreo de
la plataforma dentro de la aplicación y no funcionaría sin red. La diferencia la
decide la extensión del archivo, no una configuración.

`import-lessons` comprueba que cada video declarado responde, y nombra el que no.
Eso detecta un dominio caído, un archivo borrado y una dirección mal escrita.
**No** detecta que una plataforma haya retirado el video, porque sigue
devolviendo una página con código 200. Por eso alojar los videos propios es mejor
en los tres aspectos que importan: se reproducen dentro, se guardan para verlos
sin red, y nadie externo puede retirarlos.

Para desarrollo, recuerda que el armazón se sirve desde la copia guardada: tras
cambiar `alerta.js` o `alerta.css` hay que recargar dos veces, o limpiar la caché
desde las herramientas del navegador.

## Verificación reproducible

```bash
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test --locked
# Crea y borra únicamente su propia DB tp_test_<UUID> en MongoDB:
cargo test --locked --test workflow -- --ignored
node --check web/app.js web/alerta.js web/sw.js
```

La prueba integral comprueba origen, segundo factor, bloqueo de OTP, roles, aislamiento por propietario, validaciones, idempotencia secuencial, persistencia tras reinicio, recuperación y revocación de sesiones. No sustituye la prueba con proveedores reales ni una prueba de carga.

Comprueba además la afiliación con su resolución de sector, la ingesta y
agrupación de eventos, el reparto de avisos con su deduplicación y presupuesto,
la recomendación de destino sobre el caso real de Chiminangos, la entrega por Web
Push y SMS contra un servidor de pruebas —con cifrado y firma reales—, la barrera
de revisión del contenido educativo y las defensas del webhook de cobro.

Las reglas que más importan se verificaron **mutando el código** para confirmar
que la prueba las detecta: que una revisión a la baja no vuelve a avisar, que dos
catálogos no duplican el aviso, que un destino sísmico comprueba el suelo
licuable, que una suscripción cancelada se borra, que no se publica contenido sin
revisor, y que el webhook rechaza una firma inválida y un importe que no cuadra.

### Avisos de seguridad conocidos

`cargo audit` reporta cinco vulnerabilidades, todas transitivas y ninguna con
arreglo disponible en la cadena actual:

| Crate | Vía | Evaluación |
|---|---|---|
| `rsa` 0.9 | `openidconnect` | RUSTSEC-2023-0071, ataque Marvin. Se usa en el inicio de sesión con Google |
| `zip` 7.4 (cuatro avisos) | `candle-core` | Extracción insegura de ZIP. Solo afecta si se extraen archivos no confiables; los pesos de los modelos los coloca el operador en `TP_MODEL_DIR`. Se vuelve crítico si algún día se permite subir un modelo |
| `crypto-bigint` 0.5 | `web-push-native` → `jwt-simple` → `p256` 0.13 | RUSTSEC-2026-0387, variabilidad temporal en operaciones de tiempo constante. Afectaría a la firma VAPID |

Sobre el último: explotarlo exigiría medir con precisión el tiempo de nuestras
operaciones de firma, y un atacante no tiene oráculo para pedirnos firmas a
demanda. Aunque la clave VAPID se filtrara, no permitiría enviar avisos: hacen
falta además el `endpoint`, el `p256dh` y el `auth` de cada suscripción para poder
cifrar. Elegir el crate `web-push` en su lugar no lo habría evitado —comparte la
misma cadena de `jwt-simple`— y además habría añadido OpenSSL. El arreglo depende
de que `jwt-simple` pase a la línea `elliptic-curve` 0.14.

Referencias de implementación: [driver Rust de MongoDB](https://www.mongodb.com/docs/drivers/rust/current/), [OpenID Connect de Google](https://developers.google.com/identity/openid-connect/openid-connect), [Candle](https://github.com/huggingface/candle), [modelo BERT original](https://huggingface.co/AventIQ-AI/Bert-Disaster-SOS-Message-Classifier).

## Evidencia de esta entrega

[Verificación local del 31 de octubre de 2026](../docs/evidence/rust-backend-2026-10-31.json): tres pruebas unitarias, un escenario integral HTTP/MongoDB, compilación optimizada, Clippy y flujo de navegador con inferencia real en español. BERT nativo se comparó con el motor Python anterior para un mensaje en inglés: diferencia absoluta máxima de 0,000000578 en 36 puntuaciones. Es paridad de implementación para un ejemplo, no una evaluación de exactitud del modelo. La cuenta administrativa temporal y sus registros fueron eliminados al finalizar la prueba.
