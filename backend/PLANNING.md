# Planeación territorial: primera implementación

Abrir `/app/planner.html` con una sesión activa. La aplicación principal y el explorador público enlazan **Planificar albergue**. Esta entrega implementa cálculo geoespacial determinista y una distribución propuesta. No incorpora todavía segmentación SAM 2.1/SAM 3: no se ha suministrado una ortofoto georreferenciada para ese flujo y no se simula su ejecución.

## Datos y módulos

`src/planning/catalog.rs` carga el archivo local `frontend/dist/data/spaces.json` y registra su SHA256. Se conservan los 2.991 registros: 1.970 polígonos y 1.021 puntos; solo las huellas poligonales que pasan validación geométrica son calculables. En esta revisión pasan 1.967 y se rechazan tres polígonos inválidos. No se inventan polígonos a partir de puntos ni de un área declarada. El área del inventario se muestra por separado de la recalculada.

`geometry.rs` convierte Polygon/MultiPolygon WGS84 a una proyección equirectangular local, con radio 6.371.008,8 m y origen en la primera coordenada. Es una aproximación de planeación para Cali, no una medición topográfica. Valida coordenadas en el ámbito [-77,-76] × [3,4], anillos cerrados, autocruces y solapes. `geo` realiza diferencia, intersección y contención de polígonos completos, incluyendo huecos y componentes separados.

`engine.rs` contiene reglas, distribución y pruebas. `mod.rs` compone rutas, permisos, límites, concurrencia y persistencia. La app visual vive en `web/planner.*`; `frontend/simulation-source/planning-view.js` conecta el resultado persistido con el Three.js existente. El generador del simulador vuelve a instalar esa integración sin modificar el original suministrado.

## Cálculo

1. Descontar la unión de exclusiones declaradas, limitada a la huella. Dos obstáculos solapados no se descuentan dos veces; un polígono exterior no aumenta el área excluida.
2. Reservar el porcentaje indicado en una franja al sur de la superficie utilizable. Se localiza el corte por área con 28 pasos de bisección. Esta elección no acredita que la ubicación sea adecuada para servicios reales.
3. Probar retículas de módulos de 10 × 7 m en orientaciones 0° y 90°. Cada celda tiene 1,5 m de margen por lado: 3 m entre módulos. Su envolvente completa debe estar contenida en el área restante. Se elige la orientación con más posiciones. No es un optimizador global y puede desaprovechar espacios que admitirían otra disposición. Máximo 40.000 celdas examinadas por orientación y 1.500 módulos.
4. Límite por área: perfil de asentamiento, `floor(área utilizable / 45)`; incluye la reserva de servicios en el denominador territorial, evitando descontarla dos veces. Perfil de superficie cubierta, `floor(área de distribución / 3,5)`; requiere confirmación explícita de que la huella representa superficie cubierta medida.
5. La capacidad geométrica es el mínimo entre el límite de área y las posiciones de alojamiento, a 20 personas por módulo de 70 m².
6. Aplicar restricciones conocidas: `floor(agua diaria / 15)`, `baños × 20`, `kits × 20` y capacidad de evacuación declarada. Un valor nulo queda pendiente; cero limita la propuesta a cero. La capacidad de evacuación no se estima de anchos o salidas que no estén verificados.
7. Proponer el mínimo entre esos límites y la población objetivo, si fue declarada. Se coloca solo el número de módulos requerido; el último puede tener ocupación parcial.

Referencias de área: [ACNUR, planeación de asentamientos](https://emergency.unhcr.org/emergency-assistance/settlement-and-shelter/guidance-settlements/principles-standards-settlement-planning) y [soluciones de alojamiento](https://emergency.unhcr.org/emergency-assistance/settlement-and-shelter/guidance-shelter/emergency-shelter-solutions-and-standards). El perfil de 45 m² corresponde al conjunto del asentamiento, no a superficie cubierta. Los valores no son un aforo universal para un parque, cancha o edificio.

La dotación sigue las referencias ya usadas por el proyecto: un kit por cada 20 personas, un baño por cada 20, 15 L/persona/día y una plaza para dormir por persona. Tres raciones por persona/día es un **supuesto del ejercicio**, sin equivalencia a evaluación nutricional. Se muestran necesidades para la propuesta y para el objetivo por separado, con faltantes respecto a recursos declarados. Agua, saneamiento y demás referencias deben adaptarse a contexto y protocolos; no constituyen diseño técnico ni certificación.

`operational_capacity` permanece nulo y `status=proposal_requires_review`. No hay botón que autorice automáticamente un albergue. La población presente, damnificada y demandante no se deduce de los metros cuadrados. Las alertas del inventario son intersecciones declaradas: sin geometrías de amenazas verificadas no se inventa un área afectada ni se declara seguridad por ausencia de alertas.

## Uso

1. Buscar un espacio por nombre, barrio, ID o comuna. El ejemplo inicial es `epou-8413`, Parque Vipasa.
2. Declarar objetivo del ejercicio, duración y reserva de servicios; completar solo disponibilidades conocidas.
3. Dibujar exclusiones sobre el plano local y cerrar cada polígono. Máximo 20 exclusiones y 500 vértices en total. No se usa una captura de pantalla como medición de terreno.
4. Calcular y revisar límites, necesidades, faltantes, alertas y verificaciones pendientes. Editar entradas invalida la vista previa anterior y obliga a recalcular.
5. Operadores y administradores pueden guardar. Se congelan parámetros, huella, hash de fuente, versión del motor y resultado; cambiar el inventario no recalcula propuestas anteriores.
6. **Ver propuesta guardada en 3D** abre `/simulation.html?plan=<UUID>`. El visor usa la misma sesión, carga el resultado protegido y sitúa los módulos en sus coordenadas reales sobre el relieve ilustrativo existente. Dos mallas instanciadas representan cubiertas y postes, con límites actualizados y recursos propios liberados al abandonar la página. No modifica los modelos de amenazas ni acredita pendientes, cimentación o acceso seguro.

El plano permite elegir imágenes satelitales, calles o vectores locales. El diseño 3D se puede revisar dentro de la misma pantalla; ver [diseño y mapas](DESIGN_AND_MAPS.md). Los puntos del inventario quedan visibles como no calculables. La descarga JSON incluye solicitud y resultado para revisión; no es un informe aprobado.

## API

Todas las rutas parten de `/api/v1/planning`, requieren sesión y las escrituras conservan validación de `Origin`.

| Ruta | Método | Permiso |
|---|---|---|
| `/spaces?q=Vipasa&commune=02&page=0&measurable=true` | GET | Sesión; páginas de 40 |
| `/spaces/{id}` | GET | Sesión; geometría y metadatos |
| `/preview` | POST | Sesión; 15 cálculos/minuto/usuario |
| `/plans` | POST | Operador/admin; 40 guardados/hora |
| `/plans?page=0` | GET | Operador/admin; páginas de 20, sin geometrías completas |
| `/plans/{id}` | GET | Operador/admin; instantánea completa |

Ejemplo de solicitud:

```json
{
  "space_id": "epou-8413",
  "profile": "camp",
  "days": 3,
  "target_people": 120,
  "service_reserve_percent": 30,
  "water_liters_per_day": 1500,
  "toilets_available": 5,
  "kits_available": 10,
  "evacuation_capacity": 90,
  "covered_footprint_confirmed": false,
  "exclusions": [],
  "request_key": "UUID del cliente para guardar"
}
```

Guardar exige clave de idempotencia: repetir la solicitud recupera la misma instantánea; reutilizarla con otros parámetros devuelve 409. Los índices únicos se crean al iniciar. Se audita intención de guardado, conforme al funcionamiento de la auditoría general. Los cálculos se ejecutan fuera del executor HTTP, con dos plazas por proceso y presupuesto geométrico limitado. MongoDB comparte propuestas y límites entre réplicas; cada instancia carga su catálogo local y debe desplegar la misma revisión de datos.

## Verificación

`cargo test --locked` incluye geometrías con huecos, contornos cóncavos, componentes separados, exclusiones solapadas/exteriores, sitio completamente excluido, objetivo cero, agotamiento de agua, perfiles distintos, puntos/autocruces inválidos, contención completa, ausencia de solapes interiores de módulos y determinismo.

`cargo test --locked --test workflow -- --ignored` prueba MongoDB real en una base temporal: catálogo, vista previa, permisos, rechazo de puntos y exclusiones abiertas, guardado idempotente, conflicto de parámetros y recuperación exacta de la instantánea tras reiniciar la API. Los controles anteriores de autenticación y sesiones siguen incluidos.

[Evidencia de ejecución local](../docs/evidence/planning-2026-10-31.json): ejemplo de Vipasa con objetivo de 120 personas y propuesta limitada a 90, cinco módulos cargados en 3D, persistencia tras reinicio y vista móvil sin desbordamiento horizontal. Las disponibilidades de ese ejemplo son hipotéticas para demostración, no recursos confirmados en el sitio.
