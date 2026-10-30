# RPI: Release Please y acciones fijadas

## Ruta
Delegated direct. Añadir gestión de versiones/releases con Release Please y fijar todas las GitHub Actions a SHA completos. No automatizar publicación npm.

## Criterios de aceptación
- Cada `uses:` de los workflows de Refranero apunta a un SHA completo e incluye comentario de versión.
- Release Please abre/actualiza una release PR y crea GitHub Release/tag al fusionarla, según convenciones Conventional Commits.
- La versión raíz Rust y las versiones del wrapper/dependencias opcionales npm quedan sincronizadas en la configuración de Release Please.
- No se publican paquetes npm ni binarios automáticamente.
- Conservar y revisar cambios locales previos; validar workflows y configuración.

## Tareas
- [x] Comparar CI/Release Please de AgentSync con Refranero.
- [x] Fijar SHA de acciones y añadir workflow/configuración Release Please mínima.
- [x] Ejecutar validaciones disponibles y revisar diff.

## Evidencia
- AgentSync usa `release-please-action@45996ed1f6d02564a971a2fa1b5860e934307cf7` y GitHub App token para que eventos de release puedan iniciar workflows posteriores.
- Refranero usará Release Please para changelog, versión, tag y GitHub Release; no publica paquetes npm ni binarios.
- Release Please requiere `APP_ID` y `APP_PRIVATE_KEY` configurados como secrets de Refranero, siguiendo AgentSync. El usuario confirmó que está configurando NPM_TOKEN; ese token no sustituye credenciales de GitHub App y no lo usa este flujo.
- `actionlint .github/workflows/ci.yml .github/workflows/release.yml`: PASS.
- GitHub PR run expuso que `dtolnay/rust-toolchain` pinned requiere el input `toolchain`; añadí `stable` en ambos jobs CI. Pendiente nuevo run.
- `python3 -m json.tool` para ambos archivos de Release Please: PASS.
- `git diff --check`: PASS; búsqueda de `uses:` sin SHA completo: cero resultados.
- `gh auth status` y `gh repo view yacosta738/refranero`: acceso confirmado.
- `gh secret list --repo yacosta738/refranero`: en esa comprobación no había secrets. Release Please queda a la espera de configurar `APP_ID` y `APP_PRIVATE_KEY`.
- Revisión independiente: no se detectaron errores bloqueantes. Se eliminó el extra-file del template npm porque usa marcadores y no debe editarse como un JSON de versión normal.
- `cargo fmt --all -- --check`, `cargo clippy --all-targets -- -D warnings`, `cargo test --all-targets` (15 PASS), `node --check` x2, `node --test` (3 PASS), `npm pack --dry-run`: PASS.
- CI local de matriz cross-platform no se ejecutó; requiere runners GitHub.
- GitHub Actions encontró dos problemas y ambos quedaron corregidos en commits posteriores de la misma PR: faltaba `toolchain: stable` para `dtolnay/rust-toolchain`; el test npm generaba un paquete de plataforma distinto al runner Linux.
- La suite Node pasa local tras la corrección. Runs remotos actualizados siguen pendientes al cierre de esta revisión.
- PR #2: https://github.com/yacosta738/refranero/pull/2; último commit `183c60ac8866efb6081f9bed11ff0ad1a15c28e6`.

## Estado
Checking.
