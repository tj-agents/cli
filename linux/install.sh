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
target="$bin_dir/tj"
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

install_binary() {
    if [ "$mode" = "uninstall" ]; then
        if [ -e "$target" ]; then rm -f "$target"; say removed "$target"; else say ok "$target (not installed)"; fi
        return
    fi
    if [ "$mode" = "check" ]; then
        if [ -x "$target" ]; then say ok "$target ($("$target" --version))"; else say missing "$target"; status=1; fi
        return
    fi

    local built
    if command -v cargo >/dev/null 2>&1; then
        cargo build --release --locked --quiet --manifest-path "$repo/Cargo.toml"
        built="$repo/target/release/tj"
    elif command -v gh >/dev/null 2>&1; then
        local tmp
        tmp="$(mktemp -d)"
        trap 'rm -rf "$tmp"' EXIT
        gh release download --repo tj-agents/cli --pattern 'tj-x86_64-unknown-linux-gnu.tar.gz' --dir "$tmp" --clobber
        tar -xzf "$tmp/tj-x86_64-unknown-linux-gnu.tar.gz" -C "$tmp"
        built="$tmp/tj"
    else
        echo "install needs either cargo (to build) or gh (to download a release)" >&2
        exit 1
    fi

    if [ -x "$target" ] && cmp -s "$built" "$target"; then
        say ok "$target"
    else
        mkdir -p "$bin_dir"
        # Copy then rename, so a running `tj` is never left with a half-written binary.
        cp "$built" "$target.new" && chmod 755 "$target.new" && mv -f "$target.new" "$target"
        say installed "$target ($("$target" --version))"
    fi
}

check_deps() {
    [ "$mode" = "uninstall" ] && return
    if command -v fzf >/dev/null 2>&1; then
        say ok "fzf $(fzf --version | cut -d' ' -f1)"
    else
        say missing "fzf - install it with your package manager (Arch: pacman -S fzf, Debian/Ubuntu: apt install fzf)"
        status=1
    fi
    case ":$PATH:" in
        *":$bin_dir:"*) ;;
        *) say warning "$bin_dir is not on PATH - add it in your shell's startup file" ;;
    esac
}

install_binary
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
