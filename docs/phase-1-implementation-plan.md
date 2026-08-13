# Plan de implementacion de la fase 1

Estado: propuesto para ejecucion incremental y revision tecnica.

Objetivo: convertir la interfaz GPUI actual en un mezclador PipeWire funcional, persistente y recuperable, sin mezclar responsabilidades de presentacion, dominio, control, audio en tiempo real y E/S del sistema.

## 1. Resultado esperado

Al terminar la fase 1, Orion debe:

- descubrir dinamicamente el grafo PipeWire y mantener inventario de `Device`, `Node`, `Port`, `Link`, `Metadata` y `Factory`;
- capturar fuentes, mezclar y reproducir audio F32 planar mediante streams PipeWire y enlaces explicitos;
- ofrecer ganancia, mute, pan/balance, mezcla y medicion peak/RMS/hold/clipping sin asignaciones, bloqueos ni destruccion de memoria en el camino de tiempo real;
- administrar al menos dos entradas virtuales y dos salidas virtuales, recrearlas tras una reconexion y mostrarlas en una seccion `Virtual Devices`;
- conservar rutas y selecciones de endpoints mediante identidades persistentes, no mediante IDs efimeros de PipeWire;
- persistir un esquema v1 con escritura atomica y copia de respaldo;
- recuperar el servicio despues de desconexiones, reinicios de PipeWire y hot-plug sin reiniciar la aplicacion;
- emitir errores y logs estructurados que permitan reconstruir que operacion fallo, sobre que entidad y si se recupero.

La fase no se considera terminada por mostrar controles o nodos ficticios. El audio, el estado visible y el estado persistido deben provenir del mismo `AudioGraph` validado.

## 2. Situacion de partida

La linea base observada en el repositorio es:

- el paquete raiz `orion` contiene el binario, la aplicacion GPUI, los assets y la integracion de plataforma;
- `src/device/monitor.rs` esta activo: lo crea `RootView`, se conecta a PipeWire, publica snapshots de nodos y sigue metadata de endpoints predeterminados;
- `AppState` mantiene fuentes, buses y rutas por posiciones de `Vec`, con datos iniciales de demostracion y sin conexion al audio;
- `src/engine`, `src/model` y `src/platform` no estan declarados desde `main.rs`; por tanto son scaffolding muerto, no una base ejecutada;
- ese scaffolding ya tiene contratos incompatibles con los tipos activos y contiene stubs de mezcla, rings de muestras sin semantica de frame y comentarios de fases anteriores;
- el manifiesto ya incluye PipeWire, `crossbeam-channel`, `rtrb`, `arc-swap`, `thiserror`, `log` y `env_logger`, pero varias dependencias todavia no sostienen una ruta de audio activa.

Antes del primer cambio de implementacion se registrara como evidencia el resultado de `cargo check --workspace`, `cargo test --workspace` y una ejecucion manual de descubrimiento. Los fallos preexistentes se separaran de las regresiones de cada hito.

## 3. Alcance y limites

### Incluido

- Arquitectura de capas y threads descrita en este plan.
- Modelo normalizado, IDs tipados, identidad persistente y resolucion con confidence.
- Streams de captura y playback, attachments y links PipeWire.
- DSP F32 planar, rings conscientes de frames y cambio seguro de plan.
- Cuatro dispositivos virtuales administrables como minimo.
- Persistencia v1, reconexion, telemetria, errores y logging.
- Migracion de la UI y del monitor activo; sustitucion del scaffolding muerto.

### Diferido explicitamente

- EQ y cualquier interfaz de ecualizacion.
- Filtros biquad.
- Resampling con `rubato` o con otra biblioteca.
- FFT, analizadores de espectro y visualizaciones espectrales.
- Grafo de efectos basado en `fundsp`.
- Plugins, hosting de plugins y aislamiento de plugins.
- Escenas, recall de escenas y automatizacion de escenas.

No se añadiran dependencias preparatorias para esos elementos. En particular, una incompatibilidad de sample rate se comunicara como error negociable de endpoint; no se ocultara introduciendo resampling en esta fase.

## 4. Decisiones de estructura

### 4.1 Conservar el paquete raiz

El paquete raiz seguira siendo `orion` y sera a la vez miembro raiz del workspace. Se agregara `[workspace]` con `resolver = "2"` y un unico miembro nuevo: `crates/orion-dsp`.

Esta decision se justifica porque:

- el binario GPUI, sus rutas `include_bytes!`, assets, identidad de aplicacion y configuracion de plataforma ya viven correctamente en el paquete raiz;
- mover la aplicacion a otro crate produciria una migracion amplia sin mejorar el aislamiento del DSP ni la seguridad de tiempo real;
- el dominio puede permanecer independiente mediante limites de modulo y direccion de dependencias, sin crear crates adicionales que el alcance no solicita;
- se conserva la historia de archivos y se reduce el riesgo de romper empaquetado, launcher, Wayland o X11;
- `orion-dsp` si necesita una frontera de crate: debe compilar y probarse sin GPUI, PipeWire, filesystem ni runtime asincrono.

No se crearan crates separados para dominio, backend o persistencia durante esta fase.

### 4.2 Distribucion prevista

```text
Cargo.toml                         paquete raiz + workspace
src/
  app/                            coordinator, comandos, eventos y read model
  backend/
    mod.rs                        trait y DTOs neutrales
    pipewire/                     thread, registry, streams, links y virtuales
  domain/                         AudioGraph, IDs, entidades, validacion e identidad
  persistence/                    schema v1 y worker
  realtime/                       rings por bloques, plan exchange y runner
  ui/                             presentacion GPUI
crates/
  orion-dsp/                      procesamiento F32 planar independiente
```

La distribucion es una meta, no una orden de hacer un traslado masivo. Cada modulo se introducira cuando tenga un consumidor compilable.

### 4.3 Direccion de dependencias

| Capa | Puede depender de | No puede depender de |
|---|---|---|
| `ui` | `app` y read model | PipeWire, persistence internals, DSP |
| `app` | `domain`, trait de backend, persistence, tipos de plan | GPUI, implementacion PipeWire concreta dentro del coordinator |
| `domain` | `serde`, IDs y biblioteca estandar | GPUI, PipeWire, threads, canales, filesystem |
| `backend` | DTOs de `app`/`domain`, PipeWire en Linux | GPUI, estado visual, formato JSON persistido |
| `persistence` | schema y representaciones persistibles de dominio | GPUI, PipeWire, DSP |
| `realtime` | `orion-dsp`, DTOs de plan, `rtrb` | GPUI, filesystem, serializacion, logging en callback |
| `orion-dsp` | `dasp` y biblioteca estandar necesaria | crate raiz, PipeWire, GPUI, canales, logging, serde |

La independencia del dominio se revisara en cada PR comprobando imports y APIs publicas. Al no crear un crate de dominio adicional, esta revision de dependencias es un control obligatorio de auditoria.

## 5. Arquitectura de ejecucion

### 5.1 Threads y propiedad

| Contexto | Propiedad exclusiva | Comunicacion |
|---|---|---|
| Thread GPUI | widgets, navegacion, estado efimero de puntero | envia `AppCommand`; consume `AppEvent` |
| Engine coordinator | `AudioGraph` autoritativo, revision, resoluciones y compilacion de planes | canales acotados con UI, backend y persistence |
| Thread PipeWire | main loop, core, registry, proxies, listeners, streams y objetos link | `BackendCommand`/`BackendEvent` acotados |
| Runner de audio RT | plan activo, smoothers, bloques y estado de medidores | rings SPSC preasignados y plan exchange |
| Persistence worker | lectura, validacion, serializacion, fsync, backup y rename | `PersistenceCommand`/`PersistenceEvent` acotados |

El coordinator es el unico escritor del dominio. GPUI nunca muta directamente rutas autoritativas y el thread PipeWire nunca persiste ni decide politica de usuario.

Los callbacks de proceso PipeWire solo intercambian bloques completos con rings preasignados y actualizan contadores atomicos. El runner RT ejecuta el plan DSP. Si una integracion concreta exige ejecutar el runner desde el callback PipeWire para mantener sincronizacion, se conserva exactamente el mismo contrato RT y la propiedad permanece dentro del backend; el coordinator nunca entra en el camino de audio.

### 5.2 Flujo de control

1. GPUI convierte una accion en un comando con `CommandId` y, cuando corresponda, la revision esperada.
2. El coordinator valida el comando contra `AudioGraph`.
3. Una mutacion valida incrementa `GraphRevision`, compila fuera de RT el siguiente `RenderPlan` y solicita persistencia.
4. El backend reconcilia streams, attachments y virtual devices con el estado deseado.
5. El coordinator publica un read model inmutable para GPUI y eventos con el resultado del comando.
6. El runner publica lotes de medidores desacoplados y descartables; estos nunca bloquean audio.

Los cambios continuos de fader se coalescen por ID de control conservando el ultimo valor. Los comandos estructurales, como borrar una ruta o cambiar un attachment, no se descartan silenciosamente: reciben aceptacion, rechazo o error de cola visible.

## 6. Dominio y `AudioGraph`

### 6.1 IDs tipados

Se definiran newtypes serializables y no intercambiables para:

- `SourceId`;
- `BusId`;
- `RouteId`;
- `AttachmentId`;
- `EndpointId`;
- `VirtualDeviceId`.

Su valor persistente sera UUID generado una sola vez. Los IDs numericos de PipeWire se almacenaran solo como `PwObjectId` de sesion dentro del backend y nunca se serializaran como identidad estable.

### 6.2 Modelo normalizado

`AudioGraph` contendra colecciones por ID de `Source`, `Bus`, `Route`, `Attachment` y `VirtualDevice`. Las relaciones guardaran IDs, no indices ni copias de nombres.

| Entidad | Datos de fase 1 |
|---|---|
| `Source` | ID, nombre, layout mono/stereo, gain dB, mute, pan/balance |
| `Bus` | ID, nombre, layout mono/stereo, gain dB, mute, balance |
| `Route` | ID, `SourceId`, `BusId`, enabled y gain de ruta |
| `Attachment` | ID, extremo del grafo, direccion, selector de endpoint o `VirtualDeviceId`, estado deseado |
| `VirtualDevice` | ID, nombre visible, direccion semantica, canales, enabled |

El inventario descubierto de endpoints no forma parte de `AudioGraph`: es estado runtime asociado mediante `EndpointId` y selectors. Esto evita que el hot-plug reescriba el proyecto del usuario.

Las invariantes validadas antes de publicar una revision seran:

- toda referencia apunta a una entidad existente y del tipo correcto;
- existe como maximo una ruta por par source/bus;
- los valores de gain y pan/balance son finitos y estan dentro del rango documentado;
- el layout tiene uno o dos canales en fase 1;
- una attachment de source consume un endpoint de salida de audio y una attachment de bus alimenta un endpoint de entrada;
- el grafo source-a-bus es aciclico por construccion;
- nunca quedan menos de dos virtual inputs y dos virtual outputs configurados y habilitados.

El orden visual se guardara por separado como listas de IDs. Reordenar la UI no cambiara la identidad ni las rutas.

## 7. Identidad persistente y confidence

Cada endpoint descubierto tendra un `EndpointIdentity` local y un conjunto de evidencias normalizadas: direccion, `media.class`, `node.name`, `device.name`, serial de dispositivo cuando exista, vendor/product, perfil, bus path, channel count/channel map y propiedades Orion para virtuales administrados.

Una seleccion persistida guardara:

- `EndpointId` local;
- direccion esperada;
- fingerprints confirmados por el usuario;
- ultimo nombre visible, solo como ayuda de UI;
- ultima resolucion y confidence observada;
- si se permite auto-attach.

La confidence se recalcula en cada snapshot; el valor persistido no se acepta como autoridad. El matcher sera determinista, devolvera score, nivel y razones auditables.

| Nivel | Regla minima | Accion |
|---|---|---|
| `Exact` | `orion.virtual-device-id` o serial/perfil/direccion unicos | auto-attach |
| `High` | score `>= 0.85`, candidato unico y evidencia estable | auto-attach si estaba permitido |
| `Medium` | score `0.65..0.85` o evidencia estable incompleta | mostrar propuesta; pedir confirmacion |
| `Low` | score `< 0.65` | permanecer offline |
| `Ambiguous` | dos candidatos superiores separados por menos de `0.05` | nunca auto-attach |

Los pesos exactos se fijaran con fixtures de hardware y quedaran versionados en tests. Un nombre visible por si solo nunca alcanza `High`. La confirmacion del usuario actualiza fingerprints, pero no convierte una coincidencia futura ambigua en automatica.

## 8. Contratos de comandos y eventos

### 8.1 UI hacia coordinator

- `SetSourceGain`, `SetSourceMute`, `SetSourcePan`.
- `SetBusGain`, `SetBusMute`, `SetBusBalance`.
- `SetRouteEnabled`, `SetRouteGain`.
- `AttachEndpoint`, `ConfirmEndpointMatch`, `Detach`.
- `CreateVirtualDevice`, `RenameVirtualDevice`, `SetVirtualDeviceEnabled`, `DeleteVirtualDevice`.
- `SetEngineSettings` para sample rate solicitado y quantum preferido.
- `RetryBackend`, `PersistNow` y `Shutdown`.

Cada comando lleva `CommandId`; los comandos sobre entidades llevan su ID tipado. Las mutaciones estructurales pueden llevar `expected_revision` para rechazar ediciones sobre un read model obsoleto.

### 8.2 Coordinator hacia UI

- `StateSnapshot { revision, read_model }`.
- `CommandApplied` o `CommandRejected` con `CommandId`.
- `EndpointInventoryChanged` y `EndpointResolutionChanged` con confidence y razones.
- `MeterBatch` con revision de plan y secuencia monotona.
- `EngineStatusChanged`, `BackendStatusChanged`, `VirtualDeviceStatusChanged`.
- `PersistenceStatusChanged`.
- `OperationError` estructurado y recuperable/no recuperable.

### 8.3 Coordinator hacia backend

- aplicar settings compatibles;
- reconciliar virtual devices;
- crear, actualizar o retirar attachments y links;
- instalar un plan preparado;
- solicitar snapshot, reconexion o shutdown.

### 8.4 Backend hacia coordinator

- conexion/desconexion y numero de generacion del core;
- snapshot inicial y deltas de registry;
- estado de stream, formato negociado y latencia;
- link creado, retirado, fallido o reemplazado;
- XRun, overflow, underflow y bloques descartados;
- error PipeWire con objeto, operacion y codigo nativo.

Los eventos de una generacion antigua se ignoran despues de reconectar. Los snapshots de UI y persistence incluyen `GraphRevision` para correlacionar evidencia.

## 9. DSP y camino de tiempo real

### 9.1 Frontera de `orion-dsp`

`crates/orion-dsp` expondra procesamiento sobre vistas de canales F32 planarias y buffers suministrados por el llamador. No creara threads ni conocera endpoints. Sus metodos de proceso no asignaran, bloquearan, serializaran, haran syscalls ni emitiran logs.

El crate usara `dasp` para tipos/utilidades de sample y operaciones DSP basicas donde reduzca errores, manteniendo bucles explicitos cuando sea necesario demostrar que no hay asignaciones ni adaptadores ocultos. Benchmarks y pruebas verificaran la eleccion; `dasp` no se usara como excusa para introducir un grafo dinamico en RT.

### 9.2 Orden de procesamiento

Por bloque:

1. leer canales de entrada F32 planar;
2. sanear no finitos a silencio y contabilizarlos;
3. aplicar gain de source con smoothing;
4. aplicar mute mediante el mismo smoother hacia cero, sin salto;
5. aplicar pan mono con ley constant-power o balance stereo por atenuacion del lado opuesto, sin boost oculto;
6. acumular rutas habilitadas en buses preinicializados a cero;
7. aplicar gain, mute y balance del bus con smoothing;
8. medir y escribir cada salida F32 planar.

Los targets de gain se convierten de dB a lineal fuera del bucle por muestra. El smoothing sera una rampa basada en frames, con duracion documentada inicialmente entre 5 y 20 ms y estable frente a cambios de quantum. Los tests fijaran el comportamiento exacto y demostraran continuidad para gain y mute.

### 9.3 Medidores

Cada canal publicara:

- peak absoluto del intervalo;
- RMS calculado como raiz de la media de cuadrados;
- peak hold expresado con contador de frames, sin consultar reloj en RT;
- clipping cuando `abs(sample) >= 1.0`, con contador y latch hasta acknowledgement;
- contadores de muestras no finitas, underflow y overflow asociados.

El hold inicial sera 1.5 s y su caida se definira en dB/s usando sample rate. Los datos se agruparan y limitaran a una frecuencia de UI razonable; si la cola esta llena se reemplaza telemetria antigua, nunca audio ni comandos.

### 9.4 Rings conscientes de frames

No se conservara el wrapper actual de `rtrb<f32>` que admite fragmentar un frame multicanal. Se introduciran bloques planarios preasignados con `frame_count`, `channel_count`, capacidad fija y secuencia. Rings SPSC transportaran handles de bloques entre pools libres, captura, runner y playback.

Las operaciones aceptaran o rechazaran bloques/frames completos. En overflow se descarta una unidad completa y se incrementa un contador. En underflow playback escribe silencio para el bloque completo. Nunca se desplaza un canal respecto de otro y el wrap-around tendra pruebas para mono y stereo con quantums variables.

### 9.5 Intercambio de planes sin deallocation RT

El coordinator compila un `RenderPlan` denso fuera de RT: indices de buffers, rutas activas, parametros iniciales y storage dimensionado. El intercambio usa dos rings SPSC preasignados:

- cola de planes preparados hacia RT;
- cola de planes retirados de vuelta al coordinator.

RT mueve el handle del plan nuevo y devuelve el anterior; nunca ejecuta `drop` del storage dinamico. Solo se permite un numero acotado de planes en vuelo y el coordinator coalesce revisiones hasta recuperar un slot retirado. La capacidad del canal de retorno se prueba como invariante, por lo que RT nunca debe liberar un plan como fallback. En shutdown, los handles vuelven al lado no RT antes de destruirse; una salida catastrofica puede filtrar memoria antes que violar la regla RT.

Se retirara `arc-swap` si deja de tener consumidores. No se usara un `Arc` temporal cuyo ultimo `drop` pueda ocurrir en el callback.

## 10. Backend PipeWire

### 10.1 Trait

El modulo neutral definira un trait `AudioBackend` capaz de iniciar un backend con canales de comandos/eventos y devolver un handle de ciclo de vida. El coordinator dependera del trait; Linux inyectara `PipeWireBackend` y los tests inyectaran `FakeBackend` determinista.

El trait representa capacidades y resultados, no tipos de GPUI ni proxies PipeWire. No promete compatibilidad multiplataforma en esta fase; su objetivo es aislar politica de dominio, permitir pruebas y evitar que el coordinator posea objetos con afinidad de thread.

### 10.2 Thread y descubrimiento dinamico

Un unico thread nombrado poseera `MainLoop`, `Context`, `Core`, `Registry`, proxies y listeners PipeWire. El registry mantendra tablas runtime para:

- `Device`: hardware, perfiles y propiedades estables disponibles;
- `Node`: clase, nombre, estado, formato y pertenencia a device;
- `Port`: direccion, node, canal, formato y disponibilidad;
- `Link`: extremos, estado y propiedad Orion/no Orion;
- `Metadata`: defaults y cambios relevantes;
- `Factory`: capacidades necesarias para crear objetos virtuales o links.

Se publicara un snapshot consistente despues del sync inicial y deltas con generacion despues. `global_remove` retirara dependencias runtime y marcara attachments offline sin borrar dominio. Metadata default sera un atributo del inventario, no una sustitucion silenciosa de la seleccion del usuario.

### 10.3 Streams, attachments y links

Se crearan streams de captura para fuentes y streams de playback para buses. Se solicitara `SPA_AUDIO_FORMAT_F32P`; channel count, positions, sample rate y quantum negociados se validaran antes de activar la attachment.

Una `SourceAttachment` enlaza ports de salida del endpoint con ports de entrada del stream de captura Orion. Una `BusAttachment` enlaza ports de salida del stream playback Orion con ports de entrada del endpoint. Los links se crean explicitamente con autoconnect deshabilitado para evitar rutas sorprendentes.

El backend conservara una tabla `AttachmentId -> stream(s)/port(s)/link(s)` y reconciliara estado deseado de forma idempotente. Solo eliminara links que Orion haya creado y marcado con sus propiedades. Un cambio de ports retira links obsoletos y crea reemplazos sin cambiar `AttachmentId`.

Los callbacks:

- obtienen/dequeue el buffer PipeWire;
- verifican stride, planes y numero de frames;
- copian o entregan frames completos a bloques preasignados;
- ponen silencio ante falta de salida;
- reencolan el buffer;
- actualizan solo atomics/rings preexistentes.

No construyen strings, no emiten logs y no llaman al coordinator directamente.

### 10.4 Virtual devices

Semantica visible para el usuario:

- una entrada virtual de Orion se anuncia como sink para que aplicaciones reproduzcan hacia ella; ese audio entra como source del mixer;
- una salida virtual de Orion se anuncia como source para que aplicaciones la elijan como microfono; recibe la mezcla de un bus.

El seed v1 crea dos de cada direccion, con UUID persistente y nombres unicos. La pagina `Virtual Devices` permite ver estado, crear, renombrar, habilitar/deshabilitar, recrear y borrar. Borrar o deshabilitar se rechazara si dejaria menos de dos dispositivos habilitados de esa direccion.

Cada nodo lleva propiedades estables, como application ID y `VirtualDeviceId`, para obtener match `Exact`. Los nodos existen mientras Orion esta conectado; su configuracion persiste y se recrean al iniciar o reconectar. No se invocaran `pw-cli`, scripts de shell ni configuracion global externa.

Antes de cerrar el hito se verificara en la version objetivo de PipeWire si la creacion debe hacerse directamente con stream/node properties o mediante una factory anunciada. La implementacion elegida debe conservar la semantica anterior y quedar cubierta por una prueba de integracion; no se codificara contra un nombre de factory supuesto.

## 11. Persistencia v1

### 11.1 Ubicacion y contenido

Se usara el directorio de estado/configuracion XDG resuelto para `io.github.gardun0.orion`, sin depender del working directory. El archivo principal se llamara de forma estable, por ejemplo `state-v1.json`, y tendra `schema_version: 1`.

| Persistido | No persistido |
|---|---|
| IDs, sources, buses, routes y orden visual | IDs numericos PipeWire |
| attachments deseadas y endpoint selectors | proxies, ports y links runtime |
| virtual devices y sus nombres/enabled | meters y buffers |
| gain, mute, pan/balance y settings solicitados | confidence tratada como autoridad |
| fingerprints confirmados y ultima confidence informativa | errores transitorios y backoff |

Las escenas no forman parte del schema v1.

### 11.2 Worker y protocolo

El worker recibe snapshots inmutables con `GraphRevision`, aplica debounce a cambios frecuentes y serializa fuera de GPUI/coordinator. Devuelve `Saved(revision)`, `Loaded`, `RecoveredFromBackup` o un error estructurado. Un fallo no borra el estado en memoria ni marca una revision como guardada.

Secuencia de escritura:

1. serializar y validar en memoria no RT;
2. crear un temporal unico en el mismo filesystem y con permisos restrictivos;
3. escribir, `flush` y `sync_all` del temporal;
4. si el principal actual es valido, copiarlo a un temporal de backup, sincronizarlo y renombrarlo atomicamente a `.bak`;
5. renombrar el temporal nuevo sobre el principal;
6. sincronizar el directorio padre;
7. publicar acknowledgement de la revision.

En arranque se intenta principal, luego `.bak`. Un archivo invalido se conserva o se pone en cuarentena para diagnostico y nunca se sobreescribe antes de ofrecer recovery. Se validan version, referencias, valores finitos, conteos minimos virtuales y limites de tamano. Una version futura desconocida produce error compatible y no un reset silencioso.

No existe migracion de formato anterior porque actualmente no hay estado persistido. El loader se estructura por `schema_version` para que v2 pueda agregar una migracion explicita y probada.

## 12. Reconexion y reconciliacion

Ante error fatal de core o desconexion:

1. el backend incrementa su generacion, publica `Disconnected` y pone playback en silencio;
2. destruye proxies, streams y links en su thread, sin tocar `AudioGraph` ni selectors;
3. limpia IDs PipeWire de sesion y marca endpoints/attachments offline;
4. reintenta con backoff acotado desde 250 ms hasta 10 s, sin busy loop;
5. tras conectar, realiza sync completo del registry;
6. vuelve a resolver identidades con confidence;
7. recrea virtual devices y despues attachments/links de forma idempotente;
8. publica estado operativo solo cuando streams y formatos requeridos estan listos.

El backoff se reinicia despues de un periodo estable. Hot-plug usa la misma reconciliacion sin reiniciar el core. Un endpoint ambiguo no se conecta solo para recuperar audio rapidamente.

## 13. Errores y logging estructurado

Se definiran errores por frontera: `DomainError`, `CoordinatorError`, `BackendError`, `StreamError`, `PersistenceError` y `DspConfigError`, derivados con `thiserror` y conservando source chains.

Los errores que llegan a UI incluiran:

- codigo estable;
- operacion;
- IDs tipados relacionados;
- generacion/revision cuando aplique;
- mensaje apto para usuario;
- causa tecnica para logs;
- clasificacion retryable, user-action-required o fatal.

Se migrara de `log`/`env_logger` a `tracing`/`tracing-subscriber` si la evaluacion confirma que no queda un consumidor legado. Los spans incluiran `command_id`, `graph_revision`, `attachment_id`, `virtual_device_id`, `pw_object_id` y `backend_generation` segun corresponda. Reintentos repetitivos se limitaran para no inundar logs.

El camino RT no registra. Publica contadores acotados que un thread no RT convierte en eventos y logs. Se documentara el nivel por defecto y una forma reproducible de activar diagnostico sin recompilar.

## 14. Estrategia de migracion

### 14.1 Monitor activo

El monitor actual no se elimina al principio. La migracion sera:

1. extraer fixtures y tests de clasificacion/default metadata actuales;
2. introducir trait, fake backend y coordinator sin cambiar aun la vista Devices;
3. ampliar el registry model PipeWire con los seis tipos de objeto requeridos;
4. adaptar temporalmente sus snapshots al `DeviceEvent` actual y comparar inventarios;
5. cambiar `RootView` para consumir el read model del coordinator;
6. retirar `DeviceMonitor` y su polling de 200 ms solo despues de paridad manual y automatizada.

Asi se conserva una ruta de descubrimiento funcional durante la implementacion y se evita una reescritura sin referencia observable.

### 14.2 Scaffolding muerto

`src/engine`, `src/model` y `src/platform` no se conectaran en bloque al binario. Se reemplazaran por las capas nuevas cuando cada consumidor este listo:

- los IDs por nombre y `MatrixSnapshot = Vec<Route>` se sustituyen por IDs tipados y `AudioGraph` normalizado;
- el mixer stub interleaved se sustituye por `orion-dsp` planar probado;
- el ring de muestras parciales se sustituye por bloques frame-aware;
- el thread idle y sus mensajes se sustituyen por coordinator, backend y plan exchange;
- el TODO de virtuales se sustituye por el backend PipeWire real.

Un archivo viejo se borra solo cuando no tiene consumidores y existe reemplazo cubierto. No se añadira compatibilidad con APIs internas que nunca fueron compiladas ni publicadas.

### 14.3 Estado y presentacion GPUI

`AppState` se separara en estado visual efimero y read model de aplicacion. Navegacion, hover y drag pueden seguir en GPUI; sources, buses, rutas, dispositivos, online status y meters proceden del coordinator.

Los indices de widgets capturaran IDs tipados. Al renderizar un snapshot obsoleto, un comando puede ser rechazado y GPUI pedira/recibira el snapshot actual. Las paginas existentes que simulan escenas no adquiriran persistencia en esta fase y deben indicar claramente que estan diferidas.

## 15. Dependencias previstas

Toda adicion o retirada se revisara con `cargo tree -d`, licencias y advisories. No se agregaran dependencias sin consumidor en el mismo hito.

| Dependencia | Ubicacion | Justificacion |
|---|---|---|
| `orion-dsp` path | raiz | frontera compilable para DSP independiente |
| `dasp` | `orion-dsp` | primitives de sample/DSP probadas sin acoplar plataforma |
| `pipewire` | raiz, Linux | registry, streams, main loop, metadata y links nativos |
| `crossbeam-channel` | raiz | comandos/eventos no RT acotados y seleccion en workers |
| `rtrb` | raiz | rings SPSC preasignados para bloques y planes |
| `serde` + `serde_json` | raiz | schema v1 explicito y legible; `serde` con derive |
| `uuid` | raiz | valores persistentes para newtypes, con features `serde` y generacion elegida |
| `directories` | raiz | ruta XDG estable y testeable |
| `thiserror` | raiz | errores tipados con source chain |
| `tracing` + `tracing-subscriber` | raiz | campos, spans y correlacion estructurada |

`gpui`, `gpui_platform` e `image` permanecen por ser dependencias activas de presentacion. `arc-swap` se retira al adoptar el intercambio con retorno de planes. `log` y `env_logger` se retiran al completar la migracion a tracing. No se propone `tokio`: los main loops y workers dedicados ya resuelven el modelo de ejecucion.

La version y features exactas de `dasp`, `pipewire` y `uuid` se fijaran en el hito que las use y quedaran bloqueadas en `Cargo.lock`; el plan no inventa features antes de comprobar las APIs contra la toolchain actual.

## 16. Hitos pequenos y compilables

Cada hito debe terminar con `cargo fmt --all --check`, `cargo check --workspace --all-targets`, `cargo clippy --workspace --all-targets -- -D warnings` y `cargo test --workspace`. Si el entorno no tiene servicios PipeWire, las pruebas puras/fake siguen siendo obligatorias y la omision de integracion se registra.

| Hito | Entrega compilable | Evidencia minima |
|---|---|---|
| M0 | linea base y fixtures del monitor actual | comandos de baseline y captura manual |
| M1 | workspace raiz y crate `orion-dsp` minimo | raiz y DSP compilan por separado |
| M2 | IDs, `AudioGraph`, invariantes e identidad sin conectar UI | unit tests de normalizacion y matcher |
| M3 | comandos/eventos, coordinator y `FakeBackend` | round trips, revisiones y colas llenas |
| M4 | gain/mute/pan/balance/mix y meters en DSP | golden vectors y tests de continuidad |
| M5 | bloques frame-aware, runner y plan exchange | wrap-around, stress de planes y auditoria de allocations |
| M6 | persistence worker y schema v1 | fault injection de escritura/recovery |
| M7 | registry PipeWire completo y migracion temporal del monitor | paridad de Node/defaults y deltas de seis objetos |
| M8 | streams F32P, una captura, un playback, attachment y links | loopback manual y fake integration tests |
| M9 | dos virtual inputs y dos virtual outputs administrables | inventario `pw-dump`, audio en ambos sentidos, recreate |
| M10 | GPUI consume coordinator; faders, rutas, Devices y Virtual Devices reales | pruebas de comandos y recorrido manual UI |
| M11 | reconexion, errores, tracing, retirada de scaffolding y dependencias muertas | restart PipeWire, hot-plug y checklist final |

No se acumularan M7-M10 en una sola rama no compilable. Los adapters temporales deben llevar condicion clara de retirada y no formar parte de la arquitectura final.

## 17. Plan de pruebas

### 17.1 Unitarias

- IDs no se intercambian y sobreviven round-trip serde.
- `AudioGraph` rechaza referencias huerfanas, rutas duplicadas, no finitos y conteos virtuales bajo minimo.
- Matcher devuelve score, nivel y razones deterministas; cubre exact, high, medium, low y ambiguous.
- Gain dB/lineal, smoothing, mute sin discontinuidad, pan mono, balance stereo y suma de multiples rutas.
- Peak, RMS, hold, clipping, no finitos y ventanas que cruzan bloques.
- Rings nunca entregan medio frame y mantienen canales alineados tras wrap, overflow y underflow.
- Plan exchange soporta una tormenta de revisiones sin drop/deallocation en RT.
- Schema v1 rechaza version futura y recupera backup.
- Backoff y reconciliacion son idempotentes con reloj fake.

### 17.2 Integracion automatizada

- `FakeBackend` reordena, duplica y retrasa eventos para verificar revision/generacion.
- Registry fixtures añaden y retiran Device/Node/Port/Link/Metadata/Factory.
- Un PipeWire de prueba, cuando CI lo permita, crea endpoints, negocia F32P, mueve una senal conocida y valida links.
- Persistence usa directorio temporal y fault injection antes/despues de cada fsync/rename.
- Un allocator instrumentado o contador por thread verifica cero allocations/deallocations despues de warm-up durante process, cambios de parametros y swaps de plan.
- Stress prolongado cambia faders/rutas, provoca cola de meters llena y confirma que audio continua.

### 17.3 Validacion manual Linux

1. Arrancar Orion sin PipeWire, observar error recuperable, iniciar PipeWire y confirmar reconexion automatica.
2. Inspeccionar con `pw-dump` que aparecen dos sinks de entrada virtual y dos sources de salida virtual con IDs Orion estables.
3. Reproducir tonos distintos hacia ambas entradas virtuales y rutarlos de forma independiente a buses fisicos/virtuales.
4. Grabar desde ambas salidas virtuales y confirmar que reciben solo las rutas seleccionadas.
5. Verificar gain smoothing y mute con auriculares/analisis de waveform, sin clicks en cambios normales.
6. Confirmar peak, RMS, hold y clipping con silencio, seno conocido y senal por encima de 0 dBFS.
7. Desconectar/reconectar un dispositivo fisico; comprobar confidence, auto-attach solo high/exact y no auto-attach ambiguous.
8. Reiniciar PipeWire durante audio; comprobar silencio seguro, backoff, recreacion de virtuales y recuperacion de links.
9. Reiniciar Orion; comprobar IDs, nombres, rutas, attachments y controles restaurados desde v1.
10. Corromper una copia de prueba del principal; comprobar recovery desde `.bak` y mensaje visible sin perdida silenciosa.
11. Renombrar, habilitar y recrear virtuales; comprobar que no se puede reducir ninguna direccion por debajo de dos habilitados.
12. Revisar logs correlacionando un `CommandId`, `GraphRevision`, `AttachmentId` y generacion backend sin mensajes desde RT.

## 18. Criterios de aceptacion

La fase 1 se acepta solo si se cumplen todos los puntos:

- El paquete raiz sigue construyendo el binario GPUI y el workspace solo agrega `crates/orion-dsp` como nuevo crate.
- `orion-dsp` compila y prueba sin depender del crate raiz, GPUI o PipeWire.
- La UI y el motor usan un unico `AudioGraph` normalizado con IDs tipados; no quedan rutas autoritativas por indices.
- El coordinator es el unico escritor de dominio y funciona con `FakeBackend`.
- El backend descubre y actualiza los seis tipos PipeWire requeridos.
- Al menos una ruta fisica y las cuatro rutas virtuales minimas transportan audio F32 planar correctamente.
- Existen dos entradas y dos salidas virtuales administrables, persistentes como configuracion y recreadas al reconectar.
- Attachments crean links explicitos, no borran links ajenos y sobreviven a cambios de IDs PipeWire mediante matching con confidence.
- Gain, mute, pan/balance, mix y meters cumplen sus golden tests.
- Rings no fragmentan frames y underflow produce silencio alineado.
- El camino RT demuestra cero allocations, deallocations, locks, logs y syscalls despues del warm-up.
- Los planes cambian sin deallocation RT bajo stress.
- El schema v1 se guarda atomicamente, mantiene backup y recupera un principal corrupto.
- Reiniciar PipeWire no exige reiniciar Orion y no causa auto-attach ambiguo.
- Errores y logs contienen contexto estructurado y la UI distingue offline, retrying, degraded y ready.
- El monitor activo fue migrado sin perder defaults/hot-plug y el scaffolding muerto fue reemplazado, no reanimado con stubs.
- EQ, biquad, `rubato`, FFT, `fundsp`, plugins y escenas no forman parte de la entrega ni de sus dependencias.

## 19. Riesgos y mitigaciones

| Riesgo | Impacto | Mitigacion/criterio de salida |
|---|---|---|
| Afinidad de thread de proxies PipeWire | crashes o UB | todos los objetos/listeners nacen, se usan y destruyen en el thread PipeWire |
| Negociacion F32P o layout incompatible | attachment sin audio | validar formato/positions, fallar con error estructurado; sin resampling oculto |
| Callbacks de streams no sincronizados | drift, underflow | bloques con secuencia, capacidad medida, contadores y politica de silencio; stress con quantums variables |
| Deallocation indirecta al cambiar plan | glitch RT | protocolo de retorno de handles, capacidad probada y allocator instrumentado |
| Rings por muestras desalinean stereo | audio corrupto | bloques/frame commits atomicos y rechazo de unidades incompletas |
| IDs PipeWire cambian tras reinicio | rutas equivocadas | selectors persistentes, confidence, generation y bloqueo de ambiguous |
| Propiedades de hardware pobres | falso match | pedir confirmacion para medium/low y mostrar razones/candidatos |
| Creacion virtual varia por version PipeWire | nodos ausentes | spike contra version objetivo, Factory discovery y prueba real sin shell externo |
| Escritura interrumpida | estado corrupto | temporal mismo filesystem, fsync, rename, backup y fault injection |
| Frecuencia de fader satura canales | UI o coordinator bloqueado | coalescing por control y canales separados de telemetria/estructura |
| Reconexiones en bucle | CPU/log flood | backoff acotado, rate limit y reconciliacion por generacion |
| Migracion amplia rompe UI funcional | regresion dificil de aislar | adapters temporales, hitos pequenos y paridad antes de retirar monitor |
| Dominio vuelve a acoplarse a GPUI/PipeWire | pruebas fragiles | tabla de dependencias obligatoria en review y fake backend |
| Alcance crece hacia efectos/escenas | fase no terminable | lista de diferidos y rechazo de dependencias sin consumidor de fase 1 |

## 20. Trazabilidad para auditoria

| ID | Requisito | Diseno | Implementacion | Verificacion |
|---|---|---|---|---|
| R-01 | paquete raiz + solo `orion-dsp` | 4.1-4.2 | M1 | aceptacion 1-2 |
| R-02 | GPUI presentacion y dominio independiente | 4.3, 5 | M2-M3, M10 | fake backend, revision de imports |
| R-03 | coordinator, backend trait y workers | 5, 8, 10.1, 11 | M3, M6-M8 | integracion fake y lifecycle |
| R-04 | IDs y grafo normalizado | 6 | M2 | unitarias de invariantes |
| R-05 | identidad persistente con confidence | 7 | M2, M7 | fixtures y hot-plug manual |
| R-06 | comandos/eventos | 8 | M3 | ack/reject, revision y cola llena |
| R-07 | DSP y meters | 9.1-9.3 | M4 | golden vectors |
| R-08 | rings y planes RT-safe | 9.4-9.5 | M5 | allocator y stress |
| R-09 | streams, attachments y links | 10.3 | M8 | loopback y `pw-dump` |
| R-10 | descubrimiento de seis objetos | 10.2 | M7 | registry fixtures/deltas |
| R-11 | 2+2 virtuales administrables | 10.4 | M9-M10 | audio bidireccional y recreate |
| R-12 | persistence v1 atomica/backup | 11 | M6 | fault injection y recovery |
| R-13 | reconnect | 12 | M11 | restart/hot-plug |
| R-14 | errores y logs estructurados | 13 | M11 | correlacion manual/automatizada |
| R-15 | migrar monitor y reemplazar scaffolding | 14 | M7, M10-M11 | paridad y ausencia de stubs |
| R-16 | dependencias justificadas | 15 | todos | `cargo tree`, licencias/advisories |
| R-17 | diferir efectos/plugins/escenas | 3 | todos | revision final de manifests/UI |

Para cerrar cada hito se adjuntaran al cambio: IDs de requisitos afectados, decisiones que difieran de este plan, comandos de verificacion, resultados, riesgos abiertos y evidencia manual cuando corresponda. Una desviacion arquitectonica requiere actualizar primero este documento o registrar una decision equivalente revisable; no basta con que el codigo compile.

## 21. Registro de desviaciones

Desviaciones aprobadas respecto a §9 durante la construccion del motor de bloques (2026-08):

1. **Intercambio de planes mediante `ArcSwap` + reclamacion por generaciones** (§9.5). El plan preveia dos rings SPSC de planes preparados/retirados. La arquitectura real tiene un unico publicador (el hilo del backend), por lo que `src/realtime::PlanSlot` usa `ArcSwap` y `PlanReclaimer` solo descarta planes retirados cuando el callback reporta (`completed_generation`) haber terminado con generaciones iguales o posteriores: la liberacion nunca ocurre en el hilo RT. La entrega de recursos mutables (mitades de ring, correctores, scratch) no viaja en el plan sino por rings SPSC de inbox/outbox etiquetados con la generacion del plan que los lista.
2. **Bloques interleaved F32 en lugar de pools de bloques planos** (§9.4). El transporte entre captura y bus es `f32` interleaved sobre `rtrb`, ya alineado por frame (el productor verifica `slots()` antes de empujar un frame completo). La negociacion F32P y las vistas planas de copia cero quedan como optimizacion posterior; los adaptadores de plataforma convierten desde/hacia el formato nativo.
3. **Topologia trigger de PipeWire retirada** (§10.3). Los streams por ruta (capture ASYNC + playback TRIGGER) se sustituyen por streams normales por endpoint: el patron trigger no escala a buses N:1 con relojes independientes y no existe en WASAPI/CoreAudio. El drift entre relojes lo absorbe `orion_dsp::DriftCorrector` por ruta (objetivo: un quantum de buffering, validado hasta dos).
4. **Los procesadores planares del crate (`GainProcessor`, `StereoBalanceProcessor`, `Mixer`) permanecen mono/estereo y sin consumidores directos** hasta la migracion al `RenderPlan` plano; el motor usa las primitivas compartidas del crate (`ParameterSmoother`, `linear_balance_gains`, `ChannelEq`, `DriftCorrector`). Envolverlos exigiria colecciones de vistas por bloque con asignacion en RT.
5. **Los dispositivos virtuales se reportan como capacidad de backend en runtime** (`BackendCapabilities.virtual_devices`) en lugar de asumirse siempre disponibles, preparando los ports a macOS/Windows; la UI oculta las affordances de creacion cuando el backend no la reporta.

## 22. Definicion de terminado

Ademas de los criterios funcionales, el cierre requiere:

- todos los tests y checks del workspace verdes en la toolchain soportada;
- ninguna dependencia o feature sin uso;
- ningun `unwrap`/`expect` nuevo en caminos recuperables de backend/persistence;
- shutdown ordenado con join de threads y retorno de planes/bloques;
- documentacion de schema v1, codigos de error y propiedades de nodos virtuales junto a su implementacion;
- registro de prueba manual con version de kernel, PipeWire, hardware/endpoints y comandos usados;
- issues explicitos para limites conocidos, sin esconderlos como estado `online` falso;
- confirmacion de que la interfaz no anuncia escenas o procesamiento diferido como funcional.
