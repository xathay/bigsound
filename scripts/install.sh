#!/usr/bin/env bash
# Install BigSound for the current user — builds the LADSPA plugin,
# drops the PipeWire filter-chain config in place, restarts PipeWire,
# and verifies that the "BigSound" sink shows up.
#
# No sudo required: everything goes under $HOME.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
BIGSOUND_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
LADSPA_DIR="$HOME/.ladspa"
PW_CONF_DIR="$HOME/.config/pipewire/filter-chain.conf.d"
BIN_DIR="$HOME/.local/bin"
SYSTEMD_DIR="$HOME/.config/systemd/user"
APPS_DIR="$HOME/.local/share/applications"
PROFILES_DIR="$HOME/.config/bigsound/profiles"
ICON_BASE="$HOME/.local/share/icons/hicolor"
LOCALE_BASE="$HOME/.local/share/locale"

C_OK='\033[1;32m'
C_ERR='\033[1;31m'
C_RST='\033[0m'
C_DIM='\033[2m'
ok()  { echo -e "${C_OK}✓${C_RST} $*"; }
err() { echo -e "${C_ERR}✗${C_RST} $*" >&2; }
step(){ echo -e "${C_DIM}▸${C_RST} $*"; }

# 1. Build ----------------------------------------------------------------
step "compilando workspace em release..."
cd "$BIGSOUND_DIR"
cargo build --release --quiet
BASS_BUILT="$BIGSOUND_DIR/target/release/libbig_bass.so"
CLARITY_BUILT="$BIGSOUND_DIR/target/release/libbig_clarity.so"
SPACE_BUILT="$BIGSOUND_DIR/target/release/libbig_space.so"
CROSS_BUILT="$BIGSOUND_DIR/target/release/libbig_cross.so"
LOUD_BUILT="$BIGSOUND_DIR/target/release/libbig_loud.so"
[[ -f "$BASS_BUILT" ]]    || { err "libbig_bass.so não foi gerada"; exit 1; }
[[ -f "$CLARITY_BUILT" ]] || { err "libbig_clarity.so não foi gerada"; exit 1; }
[[ -f "$SPACE_BUILT" ]]   || { err "libbig_space.so não foi gerada"; exit 1; }
[[ -f "$CROSS_BUILT" ]]   || { err "libbig_cross.so não foi gerada"; exit 1; }
[[ -f "$LOUD_BUILT" ]]    || { err "libbig_loud.so não foi gerada"; exit 1; }
ok "build pronto (BigBass + BigClarity + BigSpace + BigCross + BigLoud)"

# 2. Install plugins ------------------------------------------------------
mkdir -p "$LADSPA_DIR"
BASS_INSTALLED="$LADSPA_DIR/big_bass.so"
CLARITY_INSTALLED="$LADSPA_DIR/big_clarity.so"
SPACE_INSTALLED="$LADSPA_DIR/big_space.so"
CROSS_INSTALLED="$LADSPA_DIR/big_cross.so"
LOUD_INSTALLED="$LADSPA_DIR/big_loud.so"
install -m 755 "$BASS_BUILT"    "$BASS_INSTALLED"
install -m 755 "$CLARITY_BUILT" "$CLARITY_INSTALLED"
install -m 755 "$SPACE_BUILT"   "$SPACE_INSTALLED"
install -m 755 "$CROSS_BUILT"   "$CROSS_INSTALLED"
install -m 755 "$LOUD_BUILT"    "$LOUD_INSTALLED"
ok "plugins instalados em $LADSPA_DIR/"

# 3. Install filter-chain config (substituting plugin paths) --------------
mkdir -p "$PW_CONF_DIR"
TEMPLATE="$BIGSOUND_DIR/configs/pipewire/10-bigsound.conf.template"
TARGET="$PW_CONF_DIR/10-bigsound.conf"
sed -e "s|__BIG_BASS_SO_PATH__|$BASS_INSTALLED|g" \
    -e "s|__BIG_CLARITY_SO_PATH__|$CLARITY_INSTALLED|g" \
    -e "s|__BIG_SPACE_SO_PATH__|$SPACE_INSTALLED|g" \
    -e "s|__BIG_CROSS_SO_PATH__|$CROSS_INSTALLED|g" \
    -e "s|__BIG_LOUD_SO_PATH__|$LOUD_INSTALLED|g" \
    "$TEMPLATE" > "$TARGET"
ok "config instalada: $TARGET"

# 4. Enable filter-chain.service (loads configs from filter-chain.conf.d) -
# This service runs a separate pipewire instance that hosts filter-chains
# and is disabled by default on PipeWire ≥ 1.0. Without enabling it, the
# config we just dropped is never read.
step "habilitando filter-chain.service (carrega o config)..."
systemctl --user enable --now filter-chain.service >/dev/null 2>&1
systemctl --user restart filter-chain.service
sleep 2

# 5. Install binaries (daemon, CLI, GTK app) ------------------------------
# `install` does an atomic unlink+copy so a currently-running daemon
# doesn't trigger ETXTBSY ("Text file busy") on subsequent runs.
mkdir -p "$BIN_DIR"
DAEMON_BUILT="$BIGSOUND_DIR/target/release/bigsound-daemon"
CLI_BUILT="$BIGSOUND_DIR/target/release/bigsound"
APP_BUILT="$BIGSOUND_DIR/target/release/bigsound-app"
[[ -f "$DAEMON_BUILT" ]] || { err "bigsound-daemon não foi gerado"; exit 1; }
[[ -f "$CLI_BUILT" ]]    || { err "bigsound (CLI) não foi gerado";  exit 1; }
[[ -f "$APP_BUILT" ]]    || { err "bigsound-app (GTK) não foi gerado"; exit 1; }
install -m 755 "$DAEMON_BUILT" "$BIN_DIR/bigsound-daemon"
install -m 755 "$CLI_BUILT"    "$BIN_DIR/bigsound"
install -m 755 "$APP_BUILT"    "$BIN_DIR/bigsound-app"
ok "binários instalados em $BIN_DIR/ (bigsound-daemon, bigsound, bigsound-app)"

# 5a. Install .desktop entry so GNOME activities lists "BigSound" --------
mkdir -p "$APPS_DIR"
install -m 644 "$BIGSOUND_DIR/crates/gtk-app/data/com.bigcommunity.BigSound.desktop" "$APPS_DIR/"
update-desktop-database "$APPS_DIR" 2>/dev/null || true
ok "atalho de aplicativo registrado: $APPS_DIR/com.bigcommunity.BigSound.desktop"

# 5a.1. Install application icons (full-colour + symbolic) ---------------
ICON_SCALABLE="$ICON_BASE/scalable/apps"
ICON_SYMBOLIC="$ICON_BASE/symbolic/apps"
mkdir -p "$ICON_SCALABLE" "$ICON_SYMBOLIC"
install -m 644 "$BIGSOUND_DIR/crates/gtk-app/data/icons/com.bigcommunity.BigSound.svg" \
    "$ICON_SCALABLE/com.bigcommunity.BigSound.svg"
install -m 644 "$BIGSOUND_DIR/crates/gtk-app/data/icons/com.bigcommunity.BigSound-symbolic.svg" \
    "$ICON_SYMBOLIC/com.bigcommunity.BigSound-symbolic.svg"
gtk-update-icon-cache -f -t "$ICON_BASE" 2>/dev/null || true
ok "ícone instalado em $ICON_SCALABLE/ (full colour) e $ICON_SYMBOLIC/ (symbolic)"

# 5a.2. Compile + install translation catalogs ---------------------------
# `.po` (source) → `.mo` (binary), one per locale. Strings live in
# crates/gtk-app/po/ and target /usr/share/locale/<LANG>/LC_MESSAGES/
# (system) or ~/.local/share/locale/<LANG>/LC_MESSAGES/ (user). The GTK
# app's init_i18n() picks the user path first, then falls back.
PO_DIR="$BIGSOUND_DIR/crates/gtk-app/po"
if command -v msgfmt >/dev/null; then
    PO_COUNT=0
    for po in "$PO_DIR"/*.po; do
        [[ -f "$po" ]] || continue
        lang="$(basename "$po" .po)"
        target="$LOCALE_BASE/$lang/LC_MESSAGES/bigsound.mo"
        mkdir -p "$(dirname "$target")"
        msgfmt -o "$target" "$po" 2>&1
        PO_COUNT=$((PO_COUNT + 1))
    done
    ok "$PO_COUNT tradução(ões) compiladas em $LOCALE_BASE/<lang>/LC_MESSAGES/bigsound.mo"
else
    err "msgfmt não encontrado — instale 'gettext' para ter traduções (a UI vai ficar em inglês)"
fi

# 5b. Install bundled profiles ------------------------------------------
# Bundled profiles (00-/10-/20-/30-/40-*.json) are always overwritten so
# the user gets pattern fixes from upgrades. User-saved profiles
# (99-user-*.json) are NEVER touched.
mkdir -p "$PROFILES_DIR"
PROFILE_COUNT=0
for src in "$BIGSOUND_DIR/crates/daemon/data/profiles/"*.json; do
    [[ -f "$src" ]] || continue
    base="$(basename "$src")"
    install -m 644 "$src" "$PROFILES_DIR/$base"
    PROFILE_COUNT=$((PROFILE_COUNT + 1))
done
ok "$PROFILE_COUNT profile(s) bundled atualizados em $PROFILES_DIR/ (user 99-user-*.json preservados)"

# 6. Install systemd user unit + (re)start the daemon ---------------------
# The canonical unit file ships with ExecStart=/usr/bin/bigsound-daemon
# (correct for packaged installs). For this user-local install, patch the
# path to point at $HOME/.local/bin/bigsound-daemon where install -m 755
# just placed the binary above.
mkdir -p "$SYSTEMD_DIR"
sed "s|^ExecStart=/usr/bin/bigsound-daemon|ExecStart=$BIN_DIR/bigsound-daemon|" \
    "$BIGSOUND_DIR/crates/daemon/data/bigsound-daemon.service" \
    > "$SYSTEMD_DIR/bigsound-daemon.service"
chmod 644 "$SYSTEMD_DIR/bigsound-daemon.service"
systemctl --user daemon-reload
systemctl --user enable bigsound-daemon.service >/dev/null 2>&1
# Restart so the new binary + new node id are picked up.
systemctl --user restart bigsound-daemon.service
sleep 1
if systemctl --user is-active bigsound-daemon.service >/dev/null; then
    ok "bigsound-daemon.service rodando"
else
    err "bigsound-daemon.service NÃO subiu — veja: journalctl --user -u bigsound-daemon -n 20"
fi

# 5. Verify ---------------------------------------------------------------
if pactl list short sinks 2>/dev/null | grep -qE "^[0-9]+\s+BigSound\b"; then
    ok "sink 'BigSound' está ATIVO"
    echo
    echo -e "Próximo passo:"
    echo -e "  Abra ${C_OK}Configurações → Som → Saída${C_RST}"
    echo -e "  e selecione ${C_OK}'BigSound (DSP)'${C_RST} como dispositivo de saída."
    echo
    echo -e "Toda música/vídeo do sistema (YouTube, Spotify, etc) vai passar por BigSound."
    echo
    echo -e "Frontend GTK: abra no menu de aplicativos como ${C_OK}'BigSound'${C_RST}, ou pelo terminal:"
    echo -e "  ${C_DIM}bigsound-app${C_RST}"
    echo
    echo -e "Tuning ao vivo via terminal:"
    echo -e "  ${C_DIM}bigsound list${C_RST}                          # parâmetros disponíveis"
    echo -e "  ${C_DIM}bigsound show${C_RST}                          # tudo + valores atuais"
    echo -e "  ${C_DIM}bigsound set bigloud:amount 0.5${C_RST}        # exemplo"
    echo -e "  ${C_DIM}bigsound set bigbass:loudness_db 8${C_RST}     # exemplo (sync L+R)"
    echo
    echo -e "Pra desinstalar tudo:"
    echo -e "  ${C_DIM}systemctl --user disable --now bigsound-daemon.service filter-chain.service${C_RST}"
    echo -e "  ${C_DIM}rm \"$TARGET\" \"$BASS_INSTALLED\" \"$CLARITY_INSTALLED\" \"$SPACE_INSTALLED\" \"$CROSS_INSTALLED\" \"$LOUD_INSTALLED\"${C_RST}"
    echo -e "  ${C_DIM}rm \"$BIN_DIR/bigsound\" \"$BIN_DIR/bigsound-daemon\" \"$BIN_DIR/bigsound-app\"${C_RST}"
    echo -e "  ${C_DIM}rm \"$SYSTEMD_DIR/bigsound-daemon.service\" \"$APPS_DIR/com.bigcommunity.BigSound.desktop\"${C_RST}"
else
    err "sink 'BigSound' NÃO apareceu após o restart."
    echo
    echo "Sinks atuais:"
    pactl list short sinks
    echo
    echo "Logs do PipeWire (últimas 30 linhas):"
    journalctl --user -u pipewire -n 30 --no-pager 2>/dev/null || true
    exit 1
fi
