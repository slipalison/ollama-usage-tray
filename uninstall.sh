#!/usr/bin/env bash
# uninstall.sh — remove ollama-usage-tray (user-level install only).
set -euo pipefail

say() { printf '\033[1;32m==>\033[0m %s\n' "$*"; }

BIN="$HOME/.local/bin/ollama-usage-tray"
SHARE="$HOME/.local/share/ollama-usage-tray"
AUTOSTART="$HOME/.config/autostart/ollama-usage-tray.desktop"

say "Encerrando instância…"
pkill -f "^.*/ollama-usage-tray$" 2>/dev/null || true
pkill -x ollama-usage 2>/dev/null || true
sleep 1

say "Removendo binário"
rm -f "$BIN"

say "Removendo assets"
rm -rf "$SHARE"

say "Removendo autostart"
rm -f "$AUTOSTART"

say "Removendo ícones do tema"
find "$HOME/.local/share/icons" -name 'ollama-usage-tray*' -delete 2>/dev/null || true
update-icon-caches "$HOME/.local/share/icons/hicolor" >/dev/null 2>&1 || true

# keep user key file (~/.config/ollama-usage-tray.key) unless asked
if [ "${1:-}" = "--purge" ]; then
  rm -f "$HOME/.config/ollama-usage-tray.key"
  rm -f /tmp/ollama-usage-tray.lock
  say "Key local e lock removidos (--purge)"
fi

say "Pronto. (o repositório clonado e a key do opencode não são tocados)"