# Shared PATH helper for install.sh / install-macos.sh.
# Prints the shell files it updated; never clobbers an existing line.
add_sutura_path() {
    local line='export PATH="$HOME/.local/bin:$PATH"  # added by Sutura installer'
    local shell_name files rc added=""
    shell_name="$(basename "${SHELL:-sh}")"
    case "$shell_name" in
        zsh)
            files="$HOME/.zshrc"
            [ -f "$HOME/.zprofile" ] && files="$files $HOME/.zprofile"
            ;;
        bash)
            files="$HOME/.bashrc $HOME/.bash_profile"
            ;;
        *)
            files="$HOME/.profile"
            ;;
    esac
    for rc in $files; do
        [ -e "$rc" ] || : > "$rc"
        if ! grep -qF "# added by Sutura installer" "$rc" 2>/dev/null; then
            [ -s "$rc" ] && [ -n "$(tail -c1 "$rc")" ] && printf '\n' >> "$rc"
            printf '%s\n' "$line" >> "$rc"
            added="$added $rc"
        fi
    done
    printf '%s\n' "${added# }"
}
