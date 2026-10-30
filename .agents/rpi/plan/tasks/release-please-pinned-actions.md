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
- AgentSync usa GitHub App token, pero Refranero no necesita encadenar workflows de publicación; se usará el `GITHUB_TOKEN` predeterminado con `contents: write` y `pull-requests: write`, sin secrets extra.
- `actionlint .github/workflows/ci.yml .github/workflows/release.yml`: PASS.
- `python3 -m json.tool` para ambos archivos de Release Please: PASS.
- `git diff --check`: PASS; búsqueda de `uses:` sin SHA completo: cero resultados.
- `gh auth status` y `gh repo view yacosta738/refranero`: acceso confirmado.
- `gh secret list --repo yacosta738/refranero`: no hay secrets configurados. Para este alcance no hacen falta; Release Please usa `GITHUB_TOKEN` con permisos mínimos.
- Revisión independiente: no se detectaron errores bloqueantes. Se eliminó el extra-file del template npm porque usa marcadores y no debe editarse como un JSON de versión normal.
- `cargo fmt --all -- --check`, `cargo clippy --all-targets -- -D warnings`, `cargo test --all-targets` (15 PASS), `node --check` x2, `node --test` (3 PASS), `npm pack --dry-run`: PASS.
- CI local de matriz cross-platform no se ejecutó; requiere runners GitHub.
- Rama `ci/release-please-pinned-actions` creada; PR pendiente.

## Estado
Checking.
