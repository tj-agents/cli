#!/usr/bin/env bash
# Installs `tj` and wires the `f` and `cr` shell functions. Safe to run any number of times: each step
# reports "ok" when there is nothing to change.
#
#   linux/install.sh              install or update
#   linux/install.sh --check      report what is and isn't set up, change nothing
#   linux/install.sh --uninstall  remove the binary and the shell wiring
set -euo pipefail

repo="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
bin_dir="${TJ_BIN_DIR:-$HOME/.local/bin}"
mode="install"
case "${1:-}" in
    --check) mode="check" ;;
    --uninstall) mode="uninstall" ;;
    "") ;;
    *) echo "usage: $0 [--check|--uninstall]" >&2; exit 2 ;;
esac

begin='# >>> tj-agents/cli >>>'
end='# <<< tj-agents/cli <<<'
status=0

say() { printf '%-10s %s\n' "$1" "$2"; }

# Replace (or append, or remove) the marked block in a startup file. Everything outside the markers
# is left exactly as it was.
wire() {
    local file="$1" body="$2" current="" next
    [ -f "$file" ] && current="$(cat "$file")"
    local block
    block="$(printf '%s\n%s\n%s' "$begin" "$body" "$end")"
    local stripped
    stripped="$(printf '%s\n' "$current" | awk -v b="$begin" -v e="$end" '
        $0 == b { skip = 1; next }
        $0 == e { skip = 0; next }
        !skip')"
    if [ "$mode" = "uninstall" ]; then
        next="$stripped"
    elif [ -n "$stripped" ]; then
        next="$(printf '%s\n\n%s' "$stripped" "$block")"
    else
        next="$block"
    fi
    if [ "$current" = "$next" ]; then
        say ok "$file"
        return
    fi
    case "$mode" in
        check) say missing "$file"; status=1 ;;
        *)
            if [ -z "$next" ]; then
                rm -f "$file"
                say removed "$file"
            else
                mkdir -p "$(dirname "$file")"
                printf '%s\n' "$next" >"$file"
                say updated "$file"
            fi
            ;;
    esac
}

install_binaries() {
    if ! command -v python3 >/dev/null 2>&1; then
        echo 'install needs Python 3.12 or newer' >&2
        status=1
        return
    fi
    local arguments=(-m tj_cli.native_install --bin-dir "$bin_dir" --source "$repo")
    case "$mode" in
        check) arguments+=(--check) ;;
        uninstall) arguments+=(--uninstall) ;;
    esac
    if ! PYTHONPATH="$repo/packages/tj-cli/src${PYTHONPATH:+:$PYTHONPATH}" python3 "${arguments[@]}"; then
        status=1
    fi
}

check_deps() {
    [ "$mode" = "uninstall" ] && return
    if command -v fzf >/dev/null 2>&1; then
        local v
        v="$(fzf --version | cut -d' ' -f1)"
        # cr's preview pane needs --preview-wrap-sign, added in fzf 0.54.
        if [ "$(printf '%s\n0.54\n' "$v" | sort -V | head -1)" != "0.54" ]; then
            say warning "fzf $v is older than 0.54 - cr needs a newer one (https://github.com/junegunn/fzf/releases)"
            status=1
        else
            say ok "fzf $v"
        fi
    else
        say missing "fzf - install it with your package manager (Arch: pacman -S fzf, Debian/Ubuntu: apt install fzf)"
        status=1
    fi
    case ":$PATH:" in
        *":$bin_dir:"*) ;;
        *) say warning "$bin_dir is not on PATH - add it in your shell's startup file" ;;
    esac
}

install_binaries
check_deps

posix='command -v tj >/dev/null 2>&1 && eval "$(tj init bash)"'
wire "$HOME/.bashrc" "$posix"
[ -f "$HOME/.zshrc" ] && wire "$HOME/.zshrc" "${posix/init bash/init zsh}"
if command -v fish >/dev/null 2>&1 || [ -d "$HOME/.config/fish" ]; then
    wire "$HOME/.config/fish/conf.d/tj.fish" 'command -q tj; and tj init fish | source'
fi
if command -v pwsh >/dev/null 2>&1; then
    profile="$(pwsh -NoProfile -NonInteractive -Command '$PROFILE.CurrentUserAllHosts')"
    wire "$profile" 'if (Get-Command tj -ErrorAction SilentlyContinue) { Invoke-Expression (& tj init pwsh | Out-String) }'
fi

if [ "$mode" = "install" ]; then
    echo
    echo "Done. Open a new terminal (or run: exec \$SHELL) and try: f, cr"
fi
exit "$status"
