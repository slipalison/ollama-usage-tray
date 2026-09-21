#!/usr/bin/env bash
# install.sh — build & install ollama-usage-tray for the current user.
# No root required. Idempotent (safe to re-run).
set -euo pipefail

REPO_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
BIN_DST="$HOME/.local/bin/ollama-usage-tray"
SHARE_DIR="$HOME/.local/share/ollama-usage-tray"
ICON_BASE="$HOME/.local/share/icons/hicolor"
AUTOSTART="$HOME/.config/autostart/ollama-usage-tray.desktop"

say() { printf '\033[1;32m==>\033[0m %s\n' "$*"; }
die() { printf '\033[1;31mERRO:\033[0m %s\n' "$*" >&2; exit 1; }

# ---- deps sanity (build) ----
command -v cargo >/dev/null || die "cargo não encontrado. Instale Rust: https://rustup.rs"
command -v pkg-config >/dev/null || die "pkg-config não encontrado (sudo apt-get install pkg-config)"
[ -f /usr/include/openssl/ssl.h ] || [ -f /usr/include/x86_64-linux-gnu/openssl/ssl.h ] \
  || die "libssl-dev não encontrado (sudo apt-get install libssl-dev)"

# ---- deps sanity (runtime, non-fatal) ----
for dep in python3 xclip zenity; do
  command -v "$dep" >/dev/null || say "AVISO: '$dep' não encontrado — alguns recursos podem falhar"
done
python3 -c "import gi; gi.require_version('Rsvg','2.0')" 2>/dev/null \
  || say "AVISO: gir1.2-rsvg ausente (sudo apt-get install gir1.2-rsvg) — ícone pode não instalar"

# ---- build ----
say "Compilando (release)…"
( cd "$REPO_DIR" && cargo build --release )

# ---- install files ----
say "Instalando binário em $BIN_DST"
mkdir -p "$HOME/.local/bin"
cp "$REPO_DIR/target/release/ollama-usage-tray" "$BIN_DST"
chmod +x "$BIN_DST"

say "Instalando assets em $SHARE_DIR"
mkdir -p "$SHARE_DIR"
cp "$REPO_DIR/assets/save_icons.py" "$SHARE_DIR/save_icons.py"
cp "$REPO_DIR/assets/icon.svg" "$SHARE_DIR/icon.svg"

say "Instalando ícone no tema hicolor"
python3 "$SHARE_DIR/save_icons.py" >/dev/null
update-icon-caches "$ICON_BASE" >/dev/null 2>&1 || gtk-update-icon-cache -f -t "$ICON_BASE" >/dev/null 2>&1 || true

say "Criando autostart"
mkdir -p "$(dirname "$AUTOSTART")"
cat > "$AUTOSTART" <<EOF
[Desktop Entry]
Type=Application
Name=Ollama Usage Tray
Exec=$BIN_DST
Icon=ollama-usage-tray
X-GNOME-Autostart-enabled=true
X-GNOME-Autostart-Delay=10
StartupNotify=false
Terminal=false
EOF

case ":$PATH:" in
  *":$HOME/.local/bin:"*) : ;;
  *) say "AVISO: \$HOME/.local/bin não está no PATH — adicione ao seu ~/.bashrc" ;;
esac

# ---- launch ----
say "Iniciando…"
if pgrep -x ollama-usage >/dev/null 2>&1 || pgrep -f "$BIN_DST" >/dev/null 2>&1; then
  say "Já está rodando — nada a fazer (reinicie com: pkill -f ollama-usage-tray)"
else
  if command -v setsid >/dev/null; then
    setsid "$BIN_DST" >/dev/null 2>&1 < /dev/null &
  else
    "$BIN_DST" >/dev/null 2>&1 < /dev/null &
  fi
  sleep 1
  pgrep -f "$BIN_DST" >/dev/null || say "AVISO: o tray não subiu — rode '$BIN_DST' manualmente para ver o erro"
fi

say "Pronto! Ícone na bandeja: \$usado/\$créditos do plano Ollama."