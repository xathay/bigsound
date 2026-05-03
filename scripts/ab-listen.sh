#!/usr/bin/env bash
# A/B listening helper — plays each WAV with a clear banner so the listener
# always knows which preset is sounding. Used during BigSound DSP tuning.
#
# Usage:
#   ./ab-listen.sh                       # plays /tmp/big-bass-ab/*.wav
#   ./ab-listen.sh /path/to/dir          # custom dir
#   ./ab-listen.sh file1.wav file2.wav   # explicit list

set -euo pipefail

if [[ $# -eq 0 ]]; then
    DIR="/tmp/big-bass-ab"
    FILES=(
        "$DIR/bypass.wav:1. BYPASS — CLI sem DSP, controle (deve = dry)"
        "$DIR/dry.wav:2. DRY — original"
        "$DIR/wet-sutil-v2.wav:3. SUTIL — target 90 / drive 50 / mix 35"
        "$DIR/wet-laptop-v2.wav:4. LAPTOP — target 150 / drive 70 / mix 55 + cut_dry"
    )
elif [[ -d "$1" && $# -eq 1 ]]; then
    FILES=()
    for f in "$1"/*.wav; do
        FILES+=("$f:$(basename "$f" .wav)")
    done
else
    FILES=()
    for f in "$@"; do
        FILES+=("$f:$(basename "$f" .wav)")
    done
fi

C_BANNER='\033[1;36m'
C_RESET='\033[0m'
C_DIM='\033[2m'

for entry in "${FILES[@]}"; do
    file="${entry%%:*}"
    label="${entry#*:}"

    if [[ ! -f "$file" ]]; then
        echo "▸ pulando (não existe): $file"
        continue
    fi

    echo
    echo -e "${C_BANNER}════════════════════════════════════════════════════════════${C_RESET}"
    echo -e "${C_BANNER}  $label${C_RESET}"
    echo -e "${C_BANNER}════════════════════════════════════════════════════════════${C_RESET}"
    echo -e "${C_DIM}  $file${C_RESET}"
    echo

    mpv --no-video \
        --term-status-msg='  ▸ ${time-pos} / ${duration} (vol ${volume}%)' \
        "$file" || { echo "(interrompido)"; exit 0; }

    echo
    echo -en "${C_DIM}  Enter pro próximo · Ctrl+C pra parar tudo... ${C_RESET}"
    read -r _
done

echo
echo "── fim do A/B ──"
