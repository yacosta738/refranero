#!/usr/bin/env bash
# Configura los secretos de la GitHub App para el workflow de Release Please.
#
# Uso:
#   scripts/setup-release-app-secrets.sh <ruta-a-clave-privada.pem> <client-id>
#
# Requisitos:
#   - gh CLI autenticado con acceso de administracion al repositorio.
#   - La App instalada en el repositorio destino con permisos de Contents,
#     Issues y Pull requests (read/write).
#   - El Client ID de la App (Iv1...) pasado como segundo argumento
#     o en la variable APP_CLIENT_ID.
#
# La clave privada se lee del archivo local y viaja por stdin hacia `gh secret set`.
# Nunca se imprime en pantalla ni se guarda en el historial del shell.

set -euo pipefail

REPO="${REPO:-yacosta738/refranero}"
PRIVATE_KEY_FILE="${1:-}"
CLIENT_ID="${2:-${APP_CLIENT_ID:-}}"

if [[ -z "$PRIVATE_KEY_FILE" ]]; then
  echo "Uso: $0 <ruta-a-clave-privada.pem> [client-id]" >&2
  exit 1
fi

if [[ ! -f "$PRIVATE_KEY_FILE" ]]; then
  echo "No existe el archivo: $PRIVATE_KEY_FILE" >&2
  exit 1
fi

if [[ -z "$CLIENT_ID" ]]; then
  echo "Falta el Client ID de la GitHub App (Iv1...)." >&2
  echo "Genera uno en la configuración de la GitHub App." >&2
  exit 1
fi

echo "Configurando secretos de Release Please en $REPO"

printf '%s' "$CLIENT_ID" | gh secret set APP_CLIENT_ID --repo "$REPO"
gh secret set APP_PRIVATE_KEY --repo "$REPO" < "$PRIVATE_KEY_FILE"

echo "Listo. Secretos configurados: APP_CLIENT_ID, APP_PRIVATE_KEY"
