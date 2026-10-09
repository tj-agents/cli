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
watcher="$bin_dir/tj-watch"
unit_file="$HOME/.config/systemd/user/tj-watch.service"
desktop_file="$HOME/.config/autostart/tj-watch.desktop"
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
        if [ -e "$watcher" ]; then rm -f "$watcher"; say removed "$watcher"; else say ok "$watcher (not installed)"; fi
        return
    fi
    if [ "$mode" = "check" ]; then
        if [ -x "$target" ]; then say ok "$target ($("$target" --version))"; else say missing "$target"; status=1; fi
        if [ -x "$watcher" ]; then say ok "$watcher"; else say missing "$watcher"; status=1; fi
        return
    fi

    local built
    if command -v cargo >/dev/null 2>&1; then
        cargo build --release --locked --quiet --manifest-path "$repo/Cargo.toml" --bin tj --bin tj-watch
        built="$repo/target/release/tj"
        built_watcher="$repo/target/release/tj-watch"
    elif command -v gh >/dev/null 2>&1; then
        local tmp
        tmp="$(mktemp -d)"
        trap 'rm -rf "$tmp"' EXIT
        gh release download --repo tj-agents/cli --pattern 'tj-x86_64-unknown-linux-gnu.tar.gz' --dir "$tmp" --clobber
        tar -xzf "$tmp/tj-x86_64-unknown-linux-gnu.tar.gz" -C "$tmp"
        built="$tmp/tj"
        built_watcher="$tmp/tj-watch"
    else
        echo "install needs either cargo (to build) or gh (to download a release)" >&2
        exit 1
    fi

    mkdir -p "$bin_dir"
    for binary in "$built:$target" "$built_watcher:$watcher"; do
        source="${binary%%:*}"
        destination="${binary#*:}"
        if [ -x "$destination" ] && cmp -s "$source" "$destination"; then
            say ok "$destination"
        else
            cp "$source" "$destination.new" && chmod 755 "$destination.new" && mv -f "$destination.new" "$destination"
            say installed "$destination"
        fi
    done
}

set_watcher_startup() {
    if [ "$mode" = "uninstall" ]; then
        if systemctl --user show-environment >/dev/null 2>&1; then
            systemctl --user disable --now tj-watch.service >/dev/null 2>&1 || true
            systemctl --user daemon-reload
        fi
        if [ -e "$unit_file" ]; then rm -f "$unit_file"; say removed "$unit_file"; else say ok "$unit_file (not installed)"; fi
        if [ -e "$desktop_file" ]; then rm -f "$desktop_file"; say removed "$desktop_file"; else say ok "$desktop_file (not installed)"; fi
        return
    fi

    if systemctl --user show-environment >/dev/null 2>&1; then
        local body
        body="[Unit]
Description=tj session watcher

[Service]
ExecStart=$watcher
Restart=on-failure

[Install]
WantedBy=default.target"
        if [ -f "$unit_file" ] && [ "$(cat "$unit_file")" = "$body" ] && systemctl --user is-enabled tj-watch.service >/dev/null 2>&1; then
            say ok "$unit_file"
        elif [ "$mode" = "check" ]; then
            say missing "$unit_file"; status=1
        else
            mkdir -p "$(dirname "$unit_file")"
            printf '%s\n' "$body" >"$unit_file"
            systemctl --user daemon-reload
            systemctl --user enable --now tj-watch.service
            say installed "$unit_file"
        fi
        return
    fi

    local desktop
    desktop="[Desktop Entry]
Type=Application
Name=tj watch
Exec=$watcher
Terminal=false"
    if [ -f "$desktop_file" ] && [ "$(cat "$desktop_file")" = "$desktop" ]; then
        say ok "$desktop_file"
    elif [ "$mode" = "check" ]; then
        say missing "$desktop_file"; status=1
    else
        mkdir -p "$(dirname "$desktop_file")"
        printf '%s\n' "$desktop" >"$desktop_file"
        say installed "$desktop_file"
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

install_binary
check_deps
set_watcher_startup

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
