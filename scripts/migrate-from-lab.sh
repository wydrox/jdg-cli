#!/usr/bin/env bash
# One-time migration of a LAB installation (lab, LAB_*, ~/.config/lab, lab.sqlite)
# to jdg-cli (jdg-cli, JDG_*, ~/.config/jdg-cli, jdg-cli.sqlite).
#
#   scripts/migrate-from-lab.sh [--dry-run] [ROOT...]
#
# ROOT: extra data directories that may hold lab.sqlite (the checkout and the
# JDG_ROOT from the migrated config are always checked). The script never
# overwrites an existing target, never prints secret values and is safe to rerun.
set -euo pipefail
umask 077

DRY_RUN=0
EXTRA_ROOTS=()
for arg in "$@"; do
  case "$arg" in
    --dry-run) DRY_RUN=1 ;;
    -h|--help) sed -n '2,9p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
    *) EXTRA_ROOTS+=("$arg") ;;
  esac
done

# JDG_MIGRATE_CHECKOUT overrides the checkout directory (used by tests).
CHECKOUT="${JDG_MIGRATE_CHECKOUT:-$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)}"
CONFIG_OLD="$HOME/.config/lab"
CONFIG_NEW="$HOME/.config/jdg-cli"
CHANGES=0
WARNINGS=0

log() { printf '%s\n' "$*"; }
warn() { WARNINGS=$((WARNINGS + 1)); printf 'warning: %s\n' "$*" >&2; }

run() {
  CHANGES=$((CHANGES + 1))
  if [[ "$DRY_RUN" == 1 ]]; then
    log "would: $*"
  else
    log "$*"
    "$@"
  fi
}

# Moves OLD to NEW unless NEW already exists (then both are left untouched).
move_path() {
  local old="$1" new="$2"
  [[ -e "$old" || -L "$old" ]] || return 0
  if [[ -e "$new" || -L "$new" ]]; then
    warn "$new already exists; left $old in place"
    return 0
  fi
  [[ "$DRY_RUN" == 1 ]] || mkdir -p "$(dirname "$new")"
  run mv "$old" "$new"
}

# Renames LAB_* keys to JDG_* and ~/.config/lab, ~/.local/share/lab, ~/.cache/lab
# paths in a KEY=VALUE file. Values pass through sed and are never printed.
rewrite_env_file() {
  local file="$1"
  [[ -f "$file" ]] || return 0
  local tmp
  tmp="$(mktemp "$(dirname "$file")/.migrate.XXXXXX")"
  sed -E \
    -e 's/^([[:space:]]*(export[[:space:]]+)?)LAB_/\1JDG_/' \
    -e 's#/\.config/lab(/|$|["'"'"'[:space:]])#/.config/jdg-cli\1#g' \
    -e 's#/\.local/share/lab(/|$|["'"'"'[:space:]])#/.local/share/jdg-cli\1#g' \
    -e 's#/\.cache/lab(/|$|["'"'"'[:space:]])#/.cache/jdg-cli\1#g' \
    "$file" >"$tmp"
  if cmp -s "$file" "$tmp"; then
    rm -f "$tmp"
    return 0
  fi
  if [[ "$DRY_RUN" == 1 ]]; then
    rm -f "$tmp"
    CHANGES=$((CHANGES + 1))
    log "would: rename LAB_* keys and lab paths in $file"
    return 0
  fi
  chmod 600 "$tmp"
  CHANGES=$((CHANGES + 1))
  log "renamed LAB_* keys and lab paths in $file"
  mv "$tmp" "$file"
}

# --- directories ---------------------------------------------------------------
move_path "$CONFIG_OLD" "$CONFIG_NEW"
move_path "$HOME/.local/share/lab" "$HOME/.local/share/jdg-cli"
move_path "$HOME/.cache/lab" "$HOME/.cache/jdg-cli"
move_path "$HOME/Library/Caches/lab" "$HOME/Library/Caches/jdg-cli"
move_path "$HOME/Library/Logs/lab" "$HOME/Library/Logs/jdg-cli"
move_path "$HOME/Library/Logs/jdg-cli/lab.log" "$HOME/Library/Logs/jdg-cli/jdg-cli.log"

# In a dry run nothing moved yet: read the config from where it is now.
CONFIG_DIR="$CONFIG_NEW"
[[ -d "$CONFIG_DIR" || "$DRY_RUN" == 0 ]] || CONFIG_DIR="$CONFIG_OLD"

# --- config files ---------------------------------------------------------------
rewrite_env_file "$CONFIG_DIR/env"
env_target="$CONFIG_DIR/.env"
if [[ -L "$env_target" ]]; then
  link="$(readlink "$env_target")"
  [[ "$link" == /* ]] || link="$CONFIG_DIR/$link"
  env_target="$link"
fi
rewrite_env_file "$env_target"

# --- databases --------------------------------------------------------------------
config_root=""
if [[ -f "$CONFIG_DIR/env" ]]; then
  config_root="$(sed -nE "s/^[[:space:]]*(export[[:space:]]+)?(JDG|LAB)_ROOT=[\"']?([^\"']*)[\"']?[[:space:]]*$/\3/p" "$CONFIG_DIR/env" | tail -n 1)"
  config_root="${config_root/#\~/$HOME}"
fi
ROOTS=("$CHECKOUT" "$HOME/.local/share/jdg-cli")
[[ -z "$config_root" ]] || ROOTS+=("$config_root")
((${#EXTRA_ROOTS[@]} == 0)) || ROOTS+=("${EXTRA_ROOTS[@]}")

for root in "${ROOTS[@]}"; do
  [[ -d "$root" ]] || continue
  if [[ -e "$root/lab.sqlite" ]]; then
    if [[ -e "$root/jdg-cli.sqlite" ]]; then
      warn "$root/jdg-cli.sqlite already exists; left $root/lab.sqlite in place"
    else
      for suffix in "" -wal -shm -journal; do
        [[ ! -e "$root/lab.sqlite$suffix" ]] || run mv "$root/lab.sqlite$suffix" "$root/jdg-cli.sqlite$suffix"
      done
    fi
  fi
  # Leftovers of interrupted writes under the old temp-file prefix.
  if [[ -d "$root/data" ]]; then
    while IFS= read -r -d '' stale; do
      run rm -f "$stale"
    done < <(find "$root/data" -type f -name '.*.lab-tmp-*' -mmin +60 -print0)
  fi
  # A local .env symlink into the old config directory.
  if [[ -L "$root/.env" ]]; then
    link="$(readlink "$root/.env")"
    case "$link" in
      "$CONFIG_OLD"/*)
        run ln -sfn "$CONFIG_NEW/${link#"$CONFIG_OLD"/}" "$root/.env"
        ;;
    esac
  fi
done

# --- things this script does not change --------------------------------------------
old_plist="$HOME/Library/LaunchAgents/com.rafalw.lab.automation.plist"
if [[ -e "$old_plist" ]]; then
  warn "old launchd job installed: launchctl bootout gui/$(id -u)/com.rafalw.lab.automation && rm '$old_plist', then run scripts/install-launchd.sh"
fi
if command -v security >/dev/null 2>&1 && security find-generic-password -s lab-cli >/dev/null 2>&1; then
  warn "Keychain has items for service lab-cli: save the secrets again with 'jdg-cli onboard' (JDG_USE_KEYCHAIN=1)"
fi
for old_bin in "$HOME/.local/bin/lab" "$HOME/.local/bin/lab-cli"; do
  if [[ -e "$old_bin" || -L "$old_bin" ]]; then
    warn "$old_bin still exists: install target/release/jdg-cli as ~/.local/bin/jdg-cli and remove it"
  fi
done

if [[ "$DRY_RUN" == 1 ]]; then
  log "dry run: $CHANGES change(s), $WARNINGS warning(s)"
else
  log "done: $CHANGES change(s), $WARNINGS warning(s)"
fi
