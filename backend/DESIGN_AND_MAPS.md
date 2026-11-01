# Diseño 3D, imágenes y revisión de propuestas

El planificador `/app/planner.html` presenta **Diseño 3D** y **Mapa y satélite**. Ambas vistas usan la misma huella y los módulos devueltos por el motor Rust; no se requiere guardar para visualizar o exportar. Los cambios de parámetros invalidan la propuesta anterior. Una propuesta guardada mantiene su URL `?plan=<UUID>` al recargar.

## Diseño

`web/planner-3d/geometry.js` contiene proyección local, comprobaciones de contención y selección limitada de imágenes de contexto. `model.js` construye cubiertas a dos aguas, postes, vigas, pisos y plazas interiores; representa la ocupación propuesta de cada módulo y el último módulo parcial. Agrupa partes repetidas en mallas instanciadas, con un ID por instancia para consulta de plazas. Los interiores se detallan en hasta 80 módulos; las cubiertas y estructuras representan todos los módulos calculados.

En la reserva de servicios propone posiciones para cabinas de baño y depósitos de 1.000 L nominales. La cantidad de depósitos ilustra el agua total requerida durante el escenario, no capacidad instalada ni abastecimiento garantizado. Las envolventes de 2,3 × 2,3 m se prueban dentro de la reserva, sin cruzar bordes ni huecos; máximo 12.000 posiciones examinadas y 100 elementos ilustrados. Si no caben todos, la pantalla informa cuántos faltan por distribuir. Estas posiciones visuales no sustituyen el diseño sanitario, accesibilidad, protección ni instalación técnica. No modifican la capacidad calculada por Rust.

`view.js` gestiona cámara, controles orbitales, selección, luces, sombras, contexto y exportaciones. Se puede encuadrar el sitio, acercarse a los módulos, mirar desde arriba, girar con botones o gestos y ocultar cubiertas para ver el interior. El suelo es plano de referencia; no se deriva relieve, altura ni estado actual desde la imagen. Se renderiza por demanda y se pausa fuera de vista. Al reemplazar modelos se liberan geometrías, materiales, texturas y recursos de instancias de propiedad de esa vista.

Se usa Three.js **0.180.0**, misma revisión del visor existente, servido desde `/assets/vendor/three-0.180.0/`, sin CDN de ejecución. Contiene licencia MIT y hashes/origen en `PROVENANCE.json`. Las importaciones ESM oficiales se adaptan a rutas relativas; se conservan sus avisos. No hay compilación React ni npm en tiempo de ejecución.

## Mapas e imágenes

`frontend/design-source/map-basemaps.js` define las fuentes compartidas y sus créditos. El explorador público incorpora Mapa de calles / Satélite; el planificador ofrece además Plano local. `frontend/scripts/integrate_basemaps.py` instala un parche acotado y reproducible sobre el componente de teselas del bundle suministrado. `integrate_design.py` lo reinstala tras generar las entradas. Si cambia el bundle de origen, el script falla ante un ancla inesperada y exige inspeccionarla.

- Calles: `https://tile.openstreetmap.org/{z}/{x}/{y}.png`, con atribución OpenStreetMap y política de referente que identifica el origen.
- Satélite/aérea: [Esri World Imagery](https://www.arcgis.com/home/item.html?id=10df2279f9684e4a9f6a7f08febac2a9), mediante su servicio público de visualización de teselas. Créditos del servicio consultado: Esri, Vantor, Earthstar Geographics y la comunidad GIS. Su uso está sujeto a [las condiciones del proveedor](https://www.arcgis.com/sharing/rest/content/items/10df2279f9684e4a9f6a7f08febac2a9?f=json), no se presenta como cartografía de licencia libre.
- Plano local: geometría inventariada, sin solicitudes externas de imágenes.

Las solicitudes del mapa son las del encuadre visible; no hay descarga masiva ni exportación de teselas para uso offline. Se respeta la caché HTTP del navegador. En la app autenticada, cada imagen define `referrerPolicy=strict-origin-when-cross-origin`: se envía solo el origen al proveedor, sin tokens ni rutas de planes. La política general de la app sigue siendo `no-referrer`.

El suelo satelital del 3D es opcional y solicita hasta nueve teselas que cubren la huella; las texturas usan CORS y espacio de color sRGB. Se cancelan lógicamente las cargas anteriores al cambiar de sitio y se liberan sus texturas. Se muestran fuente, créditos y advertencia sobre fecha/resolución variables. Si falla la red, el diseño sigue disponible con superficie local.

Las imágenes **no son en vivo**, no se conoce automáticamente su fecha de adquisición por sitio y no certifican la presencia/ausencia actual de obstáculos. Se usan para contexto visual: no se han convertido en datos de entrenamiento ni se ha ejecutado SAM. Esa integración exige imágenes georreferenciadas con derechos de análisis y evaluación local.

## Exportación

- PNG: captura la vista y añade un pie con alcance conceptual y créditos si utiliza imágenes externas. Las capturas con imágenes siguen sujetas a condiciones del proveedor.
- GLB: descarga el diseño vectorial/3D, con estructuras, interiores e instancias (`EXT_mesh_gpu_instancing`). Se incluyen cubiertas aunque el usuario esté viendo el interior. Incluye la superficie local incluso si el suelo satelital está activo; las imágenes externas quedan fuera del archivo. No representa un plano constructivo ni una instalación certificada.

## Revisiones persistentes

Rutas nuevas, con sesión de operador/admin:

| Ruta | Método | Función |
|---|---|---|
| `/api/v1/planning/plans/{id}/reviews` | GET | Últimas 20 revisiones, más reciente primero |
| `/api/v1/planning/plans/{id}/reviews` | POST | Registrar observaciones y estado |

Solicitud POST: `{"status":"needs_changes","notes":"Observaciones de revisión...","request_key":"UUID"}`. Estados permitidos: `needs_changes` y `reviewed`; entre 10 y 2.000 bytes de notas, 40 registros/hora/usuario. MongoDB conserva actor, plan, fecha, texto y clave de idempotencia. Repetir la misma solicitud devuelve el mismo registro; otra observación con la misma clave devuelve 409. Se registra intención en la auditoría general.

Una revisión no cambia los parámetros, geometría o resultados guardados, no asigna un aforo autorizado y no abre un albergue. La UI registra observaciones con texto literal, sin interpretar HTML.

## Verificación

- `cargo test --locked`: 12 pruebas unitarias.
- `cargo test --locked --test workflow -- --ignored`: HTTP/MongoDB real, permisos e idempotencia de revisiones, recuperación de estas y de la propuesta tras reiniciar; base temporal eliminada.
- `cargo clippy --all-targets -- -D warnings`.
- `node --test web/planner-3d/geometry.test.js`: proyección, contención de servicios en polígonos cóncavos/con huecos, cero servicios y cobertura de imágenes limitada.
- Navegador: diseño, interiores, capas satelitales, descargas, revisiones y móvil. Artefactos en `output/playwright/`.

También se ejecutó un nuevo trabajo real de traducción español-inglés y BERT en el worker Rust, con 36 puntuaciones persistidas y revisión humana obligatoria. BERT analiza mensajes: no es el detector de espacios ni calibra el simulador.

[Evidencia de esta entrega](../docs/evidence/design-and-maps-2026-10-31.json). El ejemplo de Vipasa utiliza disponibilidades hipotéticas para demostración.
