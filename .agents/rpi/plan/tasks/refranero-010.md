# Refranero CLI 0.1.0 — tareas RPI

## Ruta
Delegated direct. Preparar la versión 0.1.0 con verificación reproducible de Rust, builds multiplataforma y empaquetado npm. No publicar ni hacer push sin autorización expresa.

## Tareas

### RPI-001 — Confirmar estado y brechas
- **Criterios de aceptación:** identificar pruebas y CI existentes; revisar qué impide validar localmente o empaquetar/publicar de forma repetible.
- **Evidencia:** Rust 1.99.1/Cargo 1.99.1; solo target local `aarch64-apple-darwin`; cinco tests Rust pasan; Clippy pasa. `cargo fmt --check` fallaba inicialmente por formato, luego se corrigió con `cargo fmt --all`. El wrapper se empaquetaba, pero no había prueba nativa end-to-end.
- **Hallazgo:** No había pruebas Node del empaquetador/launcher ni builds CI para los seis paquetes npm.
- **Referencia:** AgentSync organiza binarios en paquetes opcionales por plataforma, genera paquetes desde una matriz, verifica los tarballs y publica paquetes nativos antes del wrapper.
- **Estado:** Completado.

### RPI-002 — Cerrar validación Rust y multiplataforma
- **Criterios de aceptación:** workflow ejecuta formato, Clippy y tests; compila binarios para los seis targets npm previstos o deja explícito el bloqueo verificable.
- **Evidencia:** CI ampliado con matriz de seis targets y generación + `npm pack --dry-run` para cada paquete. `actionlint .github/workflows/ci.yml` pasa. Build local Rust release para `aarch64-apple-darwin` pasa y generó un tarball nativo de 7.9 MB unpacked. Los cinco builds remotos restantes y runners ARM no se han ejecutado en esta sesión.
- **Estado:** Parcial: pendiente ejecución del workflow en GitHub.

### RPI-003 — Preparar empaquetado npm reproducible
- **Criterios de aceptación:** launcher, paquetes por plataforma y metadatos se validan; versión y artefactos concuerdan; los paquetes pueden generarse sin publicar.
- **Evidencia:** tres pruebas Node pasan: empaquetado con carpeta de salida, rechazo de binario faltante/vacío, y forwarding de argumentos/código de salida del launcher. Wrapper y paquete `darwin-arm64` pasan `npm pack --dry-run`.
- **Estado:** Completado localmente; faltan los demás targets en CI.

### RPI-004 — Documentar release y límites
- **Criterios de aceptación:** README explica builds, empaquetado y publicación, junto con cualquier prerequisito manual; no se exponen secretos ni se publica desde esta tarea.
- **Evidencia:** README ahora enumera seis targets y el orden de publicación de paquetes nativos antes del wrapper. CI no publica. No se configuró workflow de publicación porque no se verificaron acceso al namespace npm ni credenciales.
- **Estado:** Completado con bloqueo explícito para configurar publicación.
### RPI-002 — Cerrar validación Rust y multiplataforma
- **Criterios de aceptación:** workflow ejecuta formato, Clippy y tests; compila binarios para los seis targets npm previstos o deja explícito el bloqueo verificable.
- **Evidencia:** resultados de comandos locales y/o workflow.
- **Estado:** Pendiente.

### RPI-003 — Preparar empaquetado npm reproducible
- **Criterios de aceptación:** launcher, paquetes por plataforma y metadatos se validan; versión y artefactos concuerdan; los paquetes pueden generarse sin publicar.
- **Evidencia:** checks Node/npm y `npm pack --dry-run`.
- **Estado:** Pendiente.

### RPI-004 — Documentar release y límites
- **Criterios de aceptación:** README explica builds, empaquetado y publicación, junto con cualquier prerequisito manual; no se exponen secretos ni se publica desde esta tarea.
- **Evidencia:** revisión de docs y workflow final.
- **Estado:** Pendiente.

## Actualización de namespace npm
- Scope cambiado de `@dallay` a `@yacosta738` en wrapper, dependencias opcionales, plantillas de paquetes nativos, launcher, pruebas y README, según decisión del usuario de publicar en su namespace personal.
- Verificación: `@yacosta738/refranero` responde 404 en el registry público; wrapper `npm pack --dry-run` lo identifica con ese nombre y las tres pruebas Node pasan.
- No se publicó nada. El 404 no demuestra permisos ni reserva del nombre; la publicación requiere autenticación de `yacosta738`.

## Prueba manual del CLI (2026-10-30)
- Ejecuté `cargo run --quiet -- --db <SQLite temporal>` sin alterar el catálogo del usuario.
- PASS: ejecución aleatoria por defecto, `--count 5`, filtro `proverbio`, filtro `--lang en`, `stats`, salida `--json --count 2` e importación de `data/import-example.json` (1 registro insertado; total 17).
- PASS: `--count 0` termina con error explícito `--count must be between 1 and 20`.
- FALLA/limitación de catálogo: `--category adivinanza` devuelve `no local entries for lang=es category=adivinanza`; el seed no contiene adivinanzas, aunque la categoría CLI existe.
- `--count 21` no quedó validado en esta sesión porque la salida se truncó durante la secuencia; el caso `--count 0` sí se observó con código de salida 1.
- No abrí navegador ni probé instalación npm real. La base SQLite temporal fue eliminada.

## RPI-005 — Actualización automática y manual de caché
- **Criterios de aceptación:** preservar `sync` manual; en ejecución aleatoria normal, si el catálogo local tiene más de 24 horas desde la última actualización exitosa, intentar refrescar desde la fuente actual; `--offline` desactiva la red; ante error se conserva/usa la caché existente y se informa; persistir la hora de éxito y no marcar una actualización fallida.
- **TDD:** pruebas de expiración/no expiración, actualización exitosa, fallo con fallback y modo offline antes de implementar.
- **Estado:** Completado.

## RPI-005 — Actualización automática y manual de caché
- **Implementación:** ejecución aleatoria automática cuando no hay timestamp exitoso o tiene más de 24 h; `--offline` evita red; sincronización manual existente preservada. Full sync de `virtual` reemplaza transaccionalmente solo registros `cervantes_virtual`; CVC/Wayback no se tocan. Validación de todos los registros y snapshot no vacío antes de confirmar; errores dejan snapshot y timestamp anterior intactos. Timeout 5 s por request.
- **TDD:** pruebas escritas primero; se observó fallo de compilación por helper ausente. Pruebas cubren fresh/stale/never refreshed/offline/fallo, guardado de timestamp, snapshot vacío/inválido y preservación de otros orígenes.
- **Prueba manual real:** primera consulta mostró que la página refran excede 2 MiB; tras elevar el límite acotado a 8 MiB, `cargo run` recuperó 133 registros, guardó timestamp y dejó 132 entradas `cervantes_virtual` (1 duplicado descartado por unicidad); mantuvo 5 entradas CVC. Segunda ejecución `--offline` usó la base local.
- **Estado:** Completado.

## RPI-006 — Servir caché y refrescar en segundo plano
- **Criterios de aceptación:** el comando de lectura sirve desde SQLite sin esperar HTTP; si los datos están vencidos, lanza un worker separado y retorna; `--offline` no lanza worker; SQLite permite solo un worker activo con lease recuperable; worker hace refresh transaccional y registra éxito; errores no reemplazan snapshot.
- **TDD:** primero tests de secuencia/respuesta inmediata, no-spawn en caché vigente/offline, lease concurrente y recuperación de lease vencido.
- **Implementación:** lectura desde SQLite antes de programar el worker; `--offline` impide spawn. Worker oculto es un proceso separado, escribe log al lado de la DB y procesa categorías secuencialmente con timeout acotado. Lease SQLite de 5 minutos con owner, renovación antes de cada categoría, liberación condicional y recuperación tras expiración.
- **Pruebas:** lease concurrente, reclamación de lease expirado, worker viejo no renueva ni libera lease del nuevo owner.
- **Prueba manual:** primera ejecución leyó y mostró seed, retornó en 2.85 s, arrancó worker; luego el worker descargó y persistió timestamp, catálogo quedó con 133 entradas actuales de Cervantes y 5 CVC; lease liberado.
- **Verificación final:** fmt, Clippy, 15 tests Rust, 3 tests Node, `actionlint`, `git diff --check` pasan.
- **Estado:** Completado.

## Evidencia final
- `cargo fmt --all -- --check`: PASS.
- `cargo clippy --all-targets -- -D warnings`: PASS.
- `cargo test --all-targets`: PASS (15 tests Rust para el estado actual).
- `node --test tests/npm-packaging.test.mjs`: PASS (3 tests Node).
- `node --check` para wrapper y empaquetador: PASS.
- `actionlint .github/workflows/ci.yml`: PASS.
- `git diff --check`: PASS.
- Build release + `npm pack --dry-run` para `darwin-arm64`: PASS.
- `npm pack --dry-run` para wrapper: PASS.

## Próximo paso
Ejecutar el workflow de CI en GitHub para validar los cinco targets no probados localmente. Confirmar acceso/credenciales npm y política de release antes de añadir o ejecutar publicación automatizada.
