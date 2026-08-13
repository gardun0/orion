# Auditoría de fase 1

Fecha de corte: 2026-08-02.

## Alcance y criterio

Esta auditoría describe el código presente en el worktree al comienzo de la fase. Distingue entre una interfaz dibujada, un cambio en el modelo de sesión y una operación efectiva sobre PipeWire. Los únicos estados usados en la tabla son `implementado`, `parcial`, `ausente` e `incorrecto`.

La revisión fue estática. El repositorio contiene 9 tests: 6 en `src/state.rs` y 3 en `src/device/monitor.rs`. En la sesión principal esos 9 tests se habían ejecutado con éxito antes de esta fase; no se atribuyen aquí resultados adicionales ni se considera que cubran integración real con GPUI o con un servidor PipeWire.

## Baseline del worktree

El punto de partida es la rama `develop` con un worktree sucio. Esto forma parte del baseline y no se presupone que los cambios deban descartarse:

- Modificados: `Cargo.lock`, `Cargo.toml`, `LICENSE`, `README.md`, `src/device/descriptor.rs`, `src/device/mod.rs`, `src/main.rs`, `src/platform/linux.rs`, `src/state.rs`, `src/ui/channel_strip.rs`, `src/ui/footer.rs`, `src/ui/header.rs`, `src/ui/matrix/matrix_view.rs`, `src/ui/matrix/mod.rs`, `src/ui/root.rs` y `src/ui/theme.rs`.
- Eliminados: `src/device/enumerator.rs` y `src/ui/matrix/matrix_cell.rs`.
- Sin seguimiento: `.cargo/`, `assets/`, `bacon.toml`, `src/assets.rs` y `src/device/monitor.rs`.
- El diff rastreado inicial afecta 18 archivos, con 3.025 inserciones y 1.226 eliminaciones según `git diff --stat`.
- `target/` existe localmente pero está ignorado por `.gitignore` (`.gitignore`, líneas 4-7).

No hay un directorio `.github/` ni definiciones de CI en el árbol inspeccionado.

## Estructura compilada

`Cargo.toml` define un único paquete llamado `orion` y un único binario explícito, también llamado `orion`, con entrada en `src/main.rs` (`Cargo.toml`, líneas 1-10). No existe una sección `[workspace]`, no existe `src/lib.rs` y no hay un target de biblioteca.

El grafo de módulos del binario nace de cuatro declaraciones (`src/main.rs`, líneas 1-4):

- `assets`: proveedor GPUI de iconos embebidos (`src/assets.rs`).
- `device`: descriptor y monitor PipeWire (`src/device/mod.rs`, líneas 1-2).
- `state`: modelo mutable de la sesión y presets (`src/state.rs`).
- `ui`: raíz GPUI, cabecera, pie, tiras, matriz y tema (`src/ui/mod.rs`, líneas 1-8).

Los directorios `src/engine/`, `src/model/` y `src/platform/` están presentes, pero `src/main.rs` no declara `mod engine`, `mod model` ni `mod platform`. Por tanto, esos módulos no forman parte del crate compilado y su contenido no participa en la aplicación actual:

- `engine` contiene un hilo inactivo, mensajes, un mezclador stub y wrappers de `rtrb` (`src/engine/engine.rs`, líneas 13-60; `src/engine/mixer.rs`, líneas 8-18).
- `model` contiene otro modelo de canales y rutas que no está conectado a `AppState`. Además, `src/model/routing_matrix.rs`, líneas 23 y 29, intenta leer `DeviceDescriptor.is_input` e `is_output` como campos, aunque el descriptor activo expone métodos (`src/device/descriptor.rs`, líneas 29-36). Activar ese módulo sin corregirlo produciría un error de compilación.
- `platform` sólo contiene TODOs para dispositivos virtuales. Incluso los `cfg` de `src/platform/mod.rs`, líneas 1-8, son irrelevantes mientras el módulo raíz no se declare. El archivo Linux no registra sinks ni sources (`src/platform/linux.rs`, líneas 1-3).

La dependencia `pipewire = 0.10` es incondicional para Linux (`Cargo.toml`, líneas 24-25). La feature `virtual-devices` está declarada pero vacía (`Cargo.toml`, líneas 27-29); no habilita ni deshabilita esa dependencia. Esto contradice la explicación de feature opcional en `README.md`, líneas 36-42. La descripción de descubrimiento mediante CPAL en `README.md`, línea 13, también está desactualizada frente al monitor PipeWire activo.

## Inicialización y ciclo GPUI

1. `main` inicializa `env_logger`, construye la aplicación de `gpui_platform` y registra `assets::Assets` (`src/main.rs`, líneas 11-15).
2. Dentro de `run`, configura identidad, carga dos fuentes embebidas y decodifica el PNG de 512 px usado como icono (`src/main.rs`, líneas 16-32).
3. Abre una ventana centrada de 1440x860, con mínimo 1100x680, título, `app_id` e icono (`src/main.rs`, líneas 33-48), y crea `RootView` como entidad GPUI.
4. `RootView::new` crea `AppState` sin dispositivos y arranca `DeviceMonitor` (`src/ui/root.rs`, líneas 23-45). El estado inicial contiene fuentes, buses y escenas predefinidos, no derivados completamente de PipeWire (`src/state.rs`, líneas 126-295).
5. La vista lanza una tarea GPUI separada que espera 200 ms, drena los eventos disponibles del monitor y llama a `cx.notify()` si hubo cambios (`src/ui/root.rs`, líneas 24-40 y 48-59). Es polling periódico, no una suscripción que despierte GPUI directamente.
6. `Render` elige una de las cinco vistas desde `AppState.active_view` y vuelve a construir el árbol visual (`src/ui/root.rs`, líneas 503-538).

## Matriz de capacidades

| Capacidad | Estado | Ubicación | Evidencia | Problemas | Acción |
|---|---|---|---|---|---|
| Paquete y target Cargo | implementado | `Cargo.toml`, líneas 1-10 | Un paquete y un `[[bin]]` apuntando a `src/main.rs` | No hay workspace ni biblioteca, lo cual limita reutilización pero coincide con el ejecutable actual | Mantener la topología mientras sólo exista una aplicación |
| Módulos activos | implementado | `src/main.rs`, líneas 1-4 | Sólo compila `assets`, `device`, `state` y `ui` | La arquitectura real es menor que la sugerida por el árbol y README | Documentar el grafo real junto al layout |
| `engine`, `model` y `platform` | incorrecto | `src/engine/`, `src/model/`, `src/platform/` | No están declarados desde el crate raíz; `routing_matrix.rs` usa campos inexistentes | El scaffolding puede confundirse con funcionalidad disponible y `model` no compilaría al activarse | Corregir o retirar el código dormido antes de incorporarlo al crate |
| Arranque GPUI | implementado | `src/main.rs`, líneas 11-50 | Identidad, assets, fuentes, icono, ventana y `RootView` | Los fallos de recursos o ventana terminan mediante `expect`/`unwrap` | Convertir fallos de arranque en diagnóstico explícito si se requiere recuperación |
| Estado raíz y navegación | implementado | `src/state.rs`, líneas 16-43 y 110-124; `src/ui/root.rs`, líneas 109-175 | Cinco vistas comparten una instancia de `AppState` | El estado mezcla presentación, sesión y estado de backend | Separar responsabilidades cuando se conecte control de audio |
| Polling de eventos | implementado | `src/ui/root.rs`, líneas 24-59 | Drena `DeviceEvent` cada 200 ms y notifica a GPUI | Añade hasta unos 200 ms de latencia visual y mantiene una tarea periódica aun sin eventos | Sustituirlo por señalización al executor si GPUI y el canal lo permiten |
| Descubrimiento de endpoints PipeWire | implementado | `src/device/monitor.rs`, líneas 79-224 | `pipewire-rs` 0.10, `MainLoopRc`, Registry y globals de tipo `Node` con clases `Audio/Source*` o `Audio/Sink*` | Sólo usa propiedades publicadas en el global; no enlaza el Node para observar parámetros negociados | Mantener el snapshot y añadir listeners de objetos cuando se necesiten datos dinámicos |
| Endpoint por defecto | implementado | `src/device/monitor.rs`, líneas 14-16 y 130-167 | Enlaza `Metadata` llamado `default` y parsea `default.audio.source`/`default.audio.sink` | `AppState::set_devices` toma el primer endpoint ordenado y no prioriza `is_default`; quitar Metadata no limpia defaults ya marcados | Ordenar/seleccionar por `is_default` y limpiar el estado al retirar Metadata |
| Hotplug | implementado | `src/device/monitor.rs`, líneas 120-175 | `global` inserta y `global_remove` elimina por object id, publicando un snapshot | Cada cambio clona y envía el conjunto completo; no modela puertos o enlaces asociados | Conservar snapshots para esta escala y medir antes de optimizar |
| Shutdown del monitor | implementado | `src/device/monitor.rs`, líneas 39-76 y 83-87 | `Drop` envía `Stop`, ejecuta `MainLoopRc::quit` y hace `join` del thread | Se ignoran errores de envío y join; no hay timeout ni estado final | Registrar fallos de cierre y evitar bloqueo indefinido si aparece en práctica |
| Reconexión PipeWire | ausente | `src/device/monitor.rs`, líneas 96-110 y 177-180 | Un error del core con id 0 detiene el main loop; el thread termina | No hay reintento, backoff, evento desconectado ni recreación de Context/Core/Registry | Añadir un supervisor de conexión con cancelación y backoff |
| Objetos PipeWire `Device` | ausente | `src/device/monitor.rs`, líneas 182-224 | `endpoint_from_global` acepta exclusivamente `ObjectType::Node` | No hay perfiles, rutas de dispositivo, información ALSA agregada ni cambios de perfil | Enlazar `Device` sólo para capacidades que la UI vaya a exponer |
| Objetos PipeWire `Port` | ausente | `src/device/monitor.rs` | No hay filtro, binding ni listener de puertos | No se conocen canales, direcciones o formatos por puerto | Incorporar puertos para routing y formato reales |
| Objetos PipeWire `Link` | ausente | `src/device/monitor.rs` | No hay representación ni operaciones de links | Los botones de routing no reflejan ni modifican el grafo PipeWire | Crear, destruir y observar links desde una capa de backend |
| Parámetros `Props` de Node | ausente | `src/device/monitor.rs`, líneas 189-223 | Se leen propiedades del global, pero no se enumeran ni escriben parámetros SPA `Props` | No hay volumen, mute o controles de canal efectivos ni seguimiento de cambios externos | Suscribirse a parámetros y mapear controles con confirmación del backend |
| Faders | parcial | `src/ui/root.rs`, líneas 61-107; `src/state.rs`, líneas 309-324; `src/ui/channel_strip.rs`, líneas 390-446 | Drag, ajuste fino con Shift, doble clic a 0 dB y clamp de -60 a +10 dB | Sólo cambia `gain_db` en memoria; no envía volumen a PipeWire ni a un motor | Enviar comandos al backend y reconciliar valor solicitado y valor aplicado |
| Mute por tira y bus | parcial | `src/ui/channel_strip.rs`, líneas 130-170 y 345-384 | Alterna flags y marca la escena como modificada | No cambia audio ni `Props`; no hay confirmación o error del backend | Conectar el flag a control efectivo y mostrar el resultado aplicado |
| Mute maestro | parcial | `src/ui/footer.rs`, líneas 72-105 | Alterna `master_muted` y fuerza visualmente los medidores a cero | No silencia buses reales y no marca `dirty` | Definir semántica de sesión y ejecutar mute en el backend |
| Routing en tiras y matriz | parcial | `src/state.rs`, líneas 297-307; `src/ui/channel_strip.rs`, líneas 184-224; `src/ui/matrix/matrix_view.rs`, líneas 138-197 | Ambas vistas alternan el mismo vector `routes` | No crea/elimina `Link`, no mezcla audio y depende de índices paralelos | Traducir rutas estables a operaciones PipeWire o del motor |
| Selectores de source/output | incorrecto | `src/ui/channel_strip.rs`, líneas 68-88 y 282-302 | Muestran detalle y una `v` con apariencia de selector | No tienen id de selección ni handler; el botón “Add source” sólo agrega un placeholder | No presentarlos como selectores hasta implementar elección real de Node/Port |
| Selector y tarjetas de escenas | parcial | `src/ui/header.rs`, líneas 43-74; `src/ui/root.rs`, líneas 321-409 | Permiten cambiar `selected_scene` y actualizan el mensaje | Cambiar escena no aplica un snapshot distinto y además limpia `dirty`, pudiendo ocultar cambios no guardados | Hacer que selección y confirmación carguen datos reales sin perder cambios silenciosamente |
| Guardado de escenas | incorrecto | `src/ui/header.rs`, líneas 87-118 | El botón sólo pone `dirty = false` y dice “saved in this session” | No captura una escena separada, no escribe almacenamiento y no restaura valores | Implementar snapshot y persistencia antes de denominarlo guardado |
| Medidores visuales | parcial | `src/ui/channel_strip.rs`, líneas 449-494 | Dibuja 18 segmentos estéreo con colores por rango | Es sólo el componente de presentación | Alimentarlo desde telemetría de backend a una frecuencia acotada |
| Medición de señal | ausente | `src/state.rs`, líneas 53-83 y 150-265 | Todos los `meter_l`/`meter_r` nacen en 0.0 y no existe ninguna escritura posterior | No hay peak, RMS, decay ni clipping; los medidores permanecen apagados | Producir niveles en audio y publicar snapshots ligeros hacia GPUI |
| Presets de sample rate y buffer | parcial | `src/state.rs`, líneas 3-14 y 343-361; `src/ui/root.rs`, líneas 412-477 | Valida listas y actualiza el estado de sesión | No configura PipeWire ni un motor; la propia UI indica que son solicitudes futuras | Aplicar y validar contra el endpoint seleccionado o etiquetar como no operativo |
| Descubrimiento mostrado en mixer | parcial | `src/state.rs`, líneas 374-401 | Actualiza el primer source y los dos primeros buses físicos desde snapshots | Ignora `is_default`, no conserva selección estable y no crea tiras por todos los endpoints | Mapear selección por clave estable y separar inventario de asignación |
| Audio real y mezcla | ausente | Grafo activo; `src/engine/mixer.rs`, líneas 8-18 fuera del crate | No existe stream o proceso de audio compilado | Faders, mute y rutas no afectan muestras | Incorporar un backend de audio probado antes de anunciar mezcla funcional |
| Dispositivos virtuales | ausente | `Cargo.toml`, líneas 27-29; `src/platform/linux.rs`, líneas 1-3 fuera del crate | La feature está vacía y Linux contiene sólo comentarios TODO | B1/B2 y Virtual In son placeholders offline | Implementar registro real o retirar temporalmente la affordance operativa |
| Persistencia | ausente | `src/state.rs`; `src/ui/header.rs`, líneas 104-110 | Todo vive en memoria; `serde_json` sólo parsea Metadata PipeWire | Cerrar la aplicación pierde escenas, rutas, ganancias, mute, fuentes y presets | Definir formato versionado y escritura atómica con manejo de corrupción |
| Logging | parcial | `src/main.rs`, línea 12; `Cargo.toml`, líneas 18-19 | `env_logger::init()` instala un logger | No hay llamadas `log::error!`, `warn!`, `info!`, `debug!` o `trace!` en `src/` | Añadir logs en conexión, hotplug, errores y shutdown sin usar el hilo de audio crítico |
| Manejo de errores | parcial | `src/device/monitor.rs`, líneas 45-56 y 79-110; `src/state.rs`, líneas 364-372 | Errores de setup/core llegan a `DeviceEvent::Error` y se muestran en UI | Hay `expect`/`unwrap` al arrancar; muchos envíos y el join ignoran errores; no hay recuperación | Tipar errores por dominio y definir qué es fatal, reintentable o sólo informativo |
| Tests unitarios | parcial | `src/state.rs`, líneas 404-470; `src/device/monitor.rs`, líneas 267-305 | Existen 9 tests para rutas, faders, presets y helpers de clasificación/Metadata | No cubren `RootView`, assets, hotplug real, shutdown, reconexión ni servidor PipeWire | Añadir pruebas de transición y adaptadores; aislar integración que requiera PipeWire |
| CI | ausente | Raíz del repositorio | No existe `.github/` ni otra configuración de pipeline detectada | Build, format, lint y tests no tienen ejecución automatizada versionada | Añadir CI reproducible para `fmt`, `clippy`, build y los 9 tests |
| Assets embebidos | implementado | `src/assets.rs`, líneas 5-60; `src/main.rs`, líneas 17-31; `assets/` | Diez SVG se sirven por `AssetSource`; dos TTF y el PNG de 512 px se incluyen en el binario | Los assets están sin seguimiento en el baseline, por lo que otra copia del repositorio no los recibe todavía | Versionar los recursos requeridos junto con sus licencias |
| Assets de distribución Linux | parcial | `assets/linux/`; `assets/app-icon/` | Hay desktop entry, SVG y PNG de 32 a 512 px | No hay script, paquete o target de instalación que los coloque en rutas del sistema | Integrarlos en el mecanismo de empaquetado cuando exista |

## Integración PipeWire actual

### Qué está conectado

`DeviceMonitor::start` crea dos canales y un thread llamado `pipewire-device-monitor` (`src/device/monitor.rs`, líneas 45-63). Los eventos hacia UI usan un canal `crossbeam_channel::unbounded`; el comando de parada usa `pipewire::channel`, que se adjunta al loop (`src/device/monitor.rs`, líneas 47-49 y 83-87).

Dentro del thread se crea `pipewire-rs` 0.10 `MainLoopRc`, después `ContextRc`, Core y Registry (`src/device/monitor.rs`, líneas 83-94). El listener del Registry cubre dos tipos de global:

- `Node`: sólo se aceptan clases cuyo `media.class` comienza por `Audio/Source` o `Audio/Sink`. Se extraen nombre, descripción, device id/name, rate y channels desde el diccionario de propiedades del global (`src/device/monitor.rs`, líneas 182-223). No se hace bind del Node.
- `Metadata`: sólo se enlaza el objeto con `metadata.name=default`. Sus propiedades JSON `default.audio.source` y `default.audio.sink` marcan el Node cuyo `node.name` coincide (`src/device/monitor.rs`, líneas 130-167 y 226-240).

El hotplug se implementa con `global` y `global_remove`. Los endpoints se guardan por object id, se ordenan por dirección/nombre/node name y se publica una copia completa (`src/device/monitor.rs`, líneas 120-175 y 256-265).

El shutdown sí tiene un camino explícito: al destruir `DeviceMonitor`, se envía `Stop`, el callback llama a `quit()` y el propietario espera el thread con `join` (`src/device/monitor.rs`, líneas 70-76 y 83-87).

### Qué no está conectado

- No hay bind/listeners de `Device`, `Node`, `Port` o `Link`; únicamente se enlaza Metadata.
- No hay enumeración ni suscripción a parámetros SPA, incluidos `Props`, formatos o buffers.
- No hay escritura de volumen/mute, creación de links, streams de captura/reproducción ni proceso de muestras.
- No se observan aplicaciones como `Stream/Input/Audio` o `Stream/Output/Audio`; el filtro las rechaza deliberadamente (`src/device/monitor.rs`, líneas 242-250 y test de líneas 287-295).
- No existe reconexión. Un error fatal del core cierra el loop y deja el monitor terminado (`src/device/monitor.rs`, líneas 96-110).
- No hay evento de desconexión diferenciado ni estado que vuelva de `Connected` a `Connecting`.

## Flujo PipeWire → estado → GPUI

```text
PipeWire Registry (thread dedicado, MainLoopRc)
  ├─ global Node Audio/Source|Sink
  ├─ global_remove
  └─ Metadata "default"
          │
          ▼
HashMap<object_id, DeviceDescriptor> + DefaultEndpoints
          │ publish_snapshot / Connected / Error
          ▼
crossbeam_channel::Receiver<DeviceEvent>
          │ polling cada 200 ms
          ▼
RootView::apply_device_events
  ├─ AppState::set_device_monitor_connected
  ├─ AppState::set_devices
  └─ AppState::set_device_monitor_error
          │ cx.notify()
          ▼
GPUI Render
  ├─ página Devices: inventario completo
  ├─ mixer: primer input y dos primeros outputs
  └─ footer: status_message
```

`AppState::set_devices` reemplaza el inventario, elige el primer input y los dos primeros outputs del snapshot y actualiza sus labels/flags `online` (`src/state.rs`, líneas 374-401). El snapshot ya viene ordenado alfabéticamente, no por endpoint por defecto (`src/device/monitor.rs`, líneas 256-263). La página Devices sí muestra todos los descriptors y la marca `is_default` (`src/ui/root.rs`, líneas 274-319 y 627-711).

No existe flujo inverso de GPUI hacia PipeWire. Los handlers de fader, mute, routing, escenas y presets terminan en `AppState`; `DeviceMonitor` sólo acepta `Stop`. Por ello, “PipeWire → estado → GPUI” funciona para descubrimiento, pero “GPUI → PipeWire” está ausente.

## Persistencia, observabilidad y soporte

### Persistencia

No hay lectura o escritura de configuración, formato serializable de escena, migraciones ni ubicación de datos de usuario. `serde_json` se usa exclusivamente para interpretar el valor JSON que PipeWire guarda en Metadata (`src/device/monitor.rs`, líneas 226-232). El botón de guardado sólo limpia el flag `dirty` en memoria (`src/ui/header.rs`, líneas 104-110).

### Logging y errores

El logger se instala, pero el código activo no emite registros. La UI sí recibe errores del setup y del Core PipeWire. La creación del thread, fuentes, icono y ventana usa terminación inmediata mediante `expect` o `unwrap` (`src/device/monitor.rs`, línea 56; `src/main.rs`, líneas 25, 30 y 48). `thiserror` está declarado como dependencia, pero no se usa en el código activo.

### Tests y CI

Los 9 tests existentes son unitarios y no necesitan demostrar una conexión real. Los seis de estado comprueban dirty/routing, límites de fader, alta de fuentes, formato y presets (`src/state.rs`, líneas 408-470). Los tres del monitor comprueban clasificación de `media.class` y parseo de Metadata (`src/device/monitor.rs`, líneas 271-305). No hay fixtures ni pruebas de eventos Registry, hotplug, orden de snapshots, default efectivo, lifecycle del thread o render GPUI. No hay CI versionada.

### Assets

La UI embebe los recursos necesarios en compilación. `Assets` conoce diez iconos SVG (`src/assets.rs`, líneas 5-27), y `main` embebe Inter Variable, JetBrains Mono Variable y el icono PNG (`src/main.rs`, líneas 17-31). El árbol también incluye licencias OFL, un SVG de aplicación, PNG de 32/48/64/128/256/512 px y un desktop entry. La presencia local no sustituye su versionado: todo `assets/` aparece sin seguimiento en el baseline.

## Riesgos principales

1. **Controles con apariencia operativa sin audio real.** Faders, mute y routing responden visualmente pero no afectan PipeWire ni muestras. Esto puede llevar a una interpretación peligrosa del estado de monitorización.
2. **Pérdida silenciosa de intención de escena.** Seleccionar otra escena limpia `dirty` sin guardar ni restaurar una configuración (`src/ui/header.rs`, líneas 62-70; `src/ui/root.rs`, líneas 351-356).
3. **Monitor sin recuperación.** Una caída del Core detiene el thread y la aplicación no vuelve a conectar. Un servidor PipeWire reiniciado durante la sesión deja el inventario congelado o en error.
4. **Default detectado pero no aplicado al mixer.** La UI de Devices puede marcar correctamente el default mientras Mic 1/A1 usan el primer elemento alfabético.
5. **Código dormido divergente.** `engine`, `model` y `platform` parecen una arquitectura disponible, pero no se compilan; `model` ya no coincide con `DeviceDescriptor`. Integrarlos de golpe añade fallos de compilación y dos modelos de routing incompatibles.
6. **Persistencia nominal engañosa.** “SAVE SCENE” no guarda un snapshot ni datos en disco. Cerrar el proceso pierde todos los cambios.
7. **Datos de formato débiles.** `AUDIO_RATE` y `AUDIO_CHANNELS` se leen sólo de propiedades del global Node; pueden faltar o no representar el formato negociado. No existen listeners de parámetros.
8. **Selectores no funcionales.** Los controles con una `v` sugieren elección de endpoint, pero no reciben clicks ni mantienen una identidad seleccionada.
9. **Observabilidad insuficiente.** Hay logger pero no logs, y varios errores de canales/join se descartan. Diagnosticar fallos intermitentes de PipeWire dependerá sólo del último mensaje de UI.
10. **Baseline no reproducible fuera del worktree.** El monitor activo y todos los recursos requeridos están sin seguimiento; un checkout que sólo contenga lo versionado en el baseline no representa la aplicación inspeccionada.
11. **Documentación desalineada.** README aún atribuye el descubrimiento a CPAL y presenta PipeWire como feature opcional, mientras el código usa PipeWire de forma incondicional en Linux.

## Conclusión

La fase actual implementa una aplicación GPUI de un solo binario con navegación, controles de sesión y descubrimiento vivo de Nodes source/sink de PipeWire. El tramo PipeWire → snapshots → `AppState` → GPUI está presente, incluido hotplug, Metadata de defaults y shutdown ordenado del monitor. La mezcla, el control de audio, los parámetros `Props`, Ports, Links, Devices, reconexión, persistencia y telemetría de medidores no están implementados en el grafo compilado. Los módulos que sugieren motor, modelo alternativo y backend de plataforma permanecen fuera del crate y no deben contarse como capacidad actual.
