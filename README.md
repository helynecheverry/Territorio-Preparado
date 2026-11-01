# Territorio Preparado

Prototipo territorial para preparar espacios y coordinar ayudas ante emergencias en Cali.

Equipo: William Ortiz, Herlin Echeverry, Pablo Arango y Bryan Martinez Villamarin.

## Aplicación con backend Rust y MongoDB

La aplicación operativa nueva se ejecuta desde `backend` con `./scripts/run-local.sh` y se abre en **http://127.0.0.1:4180/app/**. Incluye acceso con segundo factor por correo, Google configurable, roles, reportes, albergues, administración y cola durable de IA. BERT y traducción Marian usan inferencia Rust local; MongoDB conserva usuarios, sesiones, reportes y resultados. [Instalación, seguridad, API y despliegue](backend/README.md).

El [planificador de albergues](backend/PLANNING.md) está en `/app/planner.html`: calcula sobre huellas del inventario, permite exclusiones, compara recursos y conserva propuestas que se abren en el simulador 3D. Es cálculo geométrico sujeto a revisión; la segmentación visual SAM permanece pendiente.

El planificador incorpora ahora diseño 3D con interiores y servicios ilustrativos, exportaciones PNG/GLB y revisiones persistentes. Hay mapa satelital de referencia en el explorador y en el planificador; [fuentes, uso y alcance](backend/DESIGN_AND_MAPS.md).

El prototipo y los servidores Python descritos abajo se conservan como versiones experimentales. La evaluación satelital xView2 aún pertenece a esa implementación anterior. No ejecutar ambos servidores en el mismo puerto.

## Encender el equipo y volver a ingresar

En este equipo, Territorio Preparado arranca como servicio al iniciar sesión en Linux. Los accesos del escritorio permiten abrir la aplicación, consultar la cuenta preparada y obtener el segundo factor vigente. La cuenta local es `ijoan@territorio.local`; la contraseña permanece en el archivo privado de acceso, fuera de Git.

[Guía de inicio y acceso en PDF](docs/GUIA_INICIO_Y_ACCESO.pdf) · [Versión HTML para consultar sin conexión](docs/GUIA_INICIO_Y_ACCESO.html). Desde la raíz también se puede usar `./territorio abrir`.

## Documento actualizado

- [Documento PDF](Territorio_Preparado_Actualizado.pdf)
- [Documento editable Word](Territorio_Preparado_Actualizado.docx)
- [Documento fuente](Territorio_Preparado_Actualizado.md)

## Ejecutar el prototipo

Desde la raíz del repositorio, con Python 3 instalado:

```bash
python3 -m http.server 4173 --bind 127.0.0.1 --directory frontend/dist
```

Abrir http://127.0.0.1:4173/ y usar **06 Simulación** para acceder al laboratorio.

La entrega contiene la app compilada; no requiere instalar Node para ejecutarla. Los fuentes originales y la configuración de compilación React no están disponibles en esta copia. Los scripts de mantenimiento se conservan en `frontend/scripts` y el laboratorio en `frontend/simulation-source`.

Los escenarios son ejercicios de preparación; sus límites y fuentes aparecen en la interfaz y en `frontend/dist/simulation-method.md`.

## GitHub Pages

Dirección pública: https://bryanmavi.github.io/Territorio-Preparado/

Un administrador debe seleccionar **Settings → Pages → Build and deployment → Source → GitHub Actions**. El workflow `.github/workflows/pages.yml` publica `frontend/dist` cada vez que se actualiza `main`. El script `frontend/scripts/prepare_pages.py` adapta rutas de datos, imágenes, módulos y simulación al prefijo del repositorio, conservando la ejecución local original.

## Simulación v3

Actualizada el 8 de octubre de 2026 con el paquete suministrado `simulacion_ciudad_cali_v3.zip`. Incluye las pestañas Simulación y Albergues y nuevos controles de escenarios. El original y sus fuentes se conservan en `frontend/simulation-source`.

La raíz incluye un acceso a `frontend/dist/` para funcionar también si Pages publica la rama `main` directamente. Las rutas de la aplicación son relativas y los módulos diferidos comparten la misma instancia de React; no añadir parámetros de versión al módulo principal en `index.html`.

<<<<<<< HEAD
## IA para desastres · BERT SOS y xView2

La aplicación incorpora **07 Centro de operaciones** y el [visor de IA para desastres](https://bryanmavi.github.io/Territorio-Preparado/disaster-ai.html).

- **BERT SOS:** clasificación multietiqueta de mensajes, traducción local español–inglés y persistencia en SQLite. Las salidas requieren revisión humana; no se ha verificado la correspondencia semántica de las 36 etiquetas.
- **xView2:** adaptador de los pesos oficiales para imágenes antes/después y exportación GeoJSON. Requiere provisionar su runtime; las pruebas técnicas no constituyen validación de precisión.
- **GitHub Pages:** publica la interfaz y el demostrador de conteo sintético. No ejecuta Python ni aloja inferencias; la interfaz indica cuando el servidor de IA no está disponible.

Para ejecutar las inferencias, sigue [instalación, modelos y pruebas](servidor/ia/README.md). Con el entorno y los pesos instalados:

```bash
servidor/ia/.venv/bin/python -m uvicorn servidor.ia.app:app --host 127.0.0.1 --port 4180
```

Abre http://127.0.0.1:4180/disaster-ai.html. El puerto 4173 sirve únicamente la aplicación estática. Los pesos, entornos virtuales, bases de datos y archivos de instrucciones para asistentes no forman parte de esta publicación.

[Arquitectura del servidor propio](docs/SERVIDOR_PROPIO_IA_Y_VIGILANCIA.md) · [registro técnico anterior de modelos y pruebas](docs/evidence/ia-2026-10-16.json).
=======
## Centro de operaciones: servidor propio e IA local

Nueva sección **07 Centro de operaciones**: arquitectura privada, consolidación de reportes sintéticos, vigilancia oficial y expansión territorial. [Propuesta técnica y comercial](docs/SERVIDOR_PROPIO_IA_Y_VIGILANCIA.md).

```bash
python3 servidor/server.py --port 4180
```

Abrir http://127.0.0.1:4180/. La consolidación local registra agregados sintéticos en SQLite. El modelo local es un conector opcional configurable; el despliegue privado 24/7 corresponde a la siguiente etapa. El caso Panamá es una captura oficial con fuente y fecha de corte.

## Conocimiento territorial y preparación 3D

Abrir `Obsidian Vault` como vault y comenzar por [Inicio](Obsidian%20Vault/00%20Inicio/Inicio.md). Documentación organizada por SICC, actores, territorio, datos, simulación y operación; notas originales y adjuntos conservados.

[Auditoría de simulación](docs/AUDIT_SIMULACION_3D.md), [supuestos](docs/ASSUMPTIONS.md) y [modelos](docs/MODELS.md). El [pipeline Rust inicial](pipeline/README.md) verifica hashes y exporta límites reales por zona. El visor existente ya integra zonas y teselas catastrales locales bajo demanda; ver [fuentes de integración](frontend/simulation-source/README.md). La ciudad nueva con OSM/GLO-30 y motores WASM sigue pendiente; el índice de límites no equivale a ese pipeline terminado.

## IA para desastres: BERT SOS y xView2

Backend FastAPI, mensajes persistentes y visor bilingüe en `disaster-ai.html`. [Instalación, pruebas y estado real de los modelos](servidor/ia/README.md). La inferencia xView2 requiere provisionar su runtime; las etiquetas semánticas BERT requieren verificación antes de priorizar automáticamente.

## Si el simulador indica «Error creating WebGL context»

Ejecutar `python3 scripts/abrir_simulador.py` o abrir `Simulador compatible.desktop`. Inicia Chrome con perfil separado y WebGL2 por software (SwiftShader), calidad ligera y servidor local. No cambia las preferencias del navegador habitual. Validado con escena y edificios visibles; ver `docs/evidence/simulation-software-renderer.json`.

## Documento integral del desarrollo

- [Informe PDF](docs/desarrollo/Territorio_Preparado_Desarrollo_Integral.pdf)
- [Word editable](docs/desarrollo/Territorio_Preparado_Desarrollo_Integral.docx)
- [Modelo financiero con fórmulas](docs/desarrollo/Modelo_Financiero_Territorio_Preparado.xlsx)

Sigue la estructura del ejemplo de Algoritmia, con encuesta de 67 respuestas, ocho capítulos, hardware, servidor propio, arquitectura IA, operación, marco legal y evaluación económica/financiera. Los presupuestos son supuestos de planeación, no cotizaciones aceptadas.

Reproducción documental: instalar `scripts/requirements-documento.txt` en un entorno aislado; ejecutar `scripts/generar_documento_desarrollo.py`, actualizar índices/PDF con el Python del sistema y `scripts/actualizar_indices_documento.py docs/desarrollo/Territorio_Preparado_Desarrollo_Integral.docx` (requiere LibreOffice y python3-uno), y comprobar con `scripts/verificar_documento_desarrollo.py`. El Excel se genera con `scripts/generar_modelo_financiero.py` y debe recalcularse en Calc/Excel antes de validar sus valores almacenados.
>>>>>>> 167ba5d (feat(backend): agregar estructura base y conexion a mongo para modulos de IA)
