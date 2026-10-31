# Shared install-ownership marker for install.sh / install-macos.sh.
# uninstall.sh uses it to decide whether it may remove sutura-env.
#
# The marker lives under the persistent Sutura data directory so a normal
# (non---purge) uninstall keeps it.  It is plain key=value text, never sourced.

SUTURA_OWNER_REL="data/.sutura-install-owner"

sutura_owner_file() {
    printf '%s/%s\n' "${1%/}" "$SUTURA_OWNER_REL"
}

sutura_owner_get() {
    local file="$1" key="$2"
    [ -f "$file" ] || return 1
    sed -n "s/^${key}=//p" "$file" 2>/dev/null | head -n 1
}

write_sutura_owner() {
    local app_dir="$1" installer="$2" created="$3"
    local env_name="$4" env_path="$5" conda_base="$6"
    local dir file tmp
    dir="${app_dir%/}/data"
    file="$dir/.sutura-install-owner"
    mkdir -p "$dir" || return 1
    tmp="$file.tmp.$$"
    {
        printf 'owner=sutura\n'
        printf 'installer=%s\n' "$installer"
        printf 'created_at=%s\n' "$(date -u +%Y-%m-%dT%H:%M:%SZ)"
        printf 'conda_env_name=%s\n' "$env_name"
        printf 'conda_env_created=%s\n' "$created"
        printf 'conda_env_path=%s\n' "$env_path"
        printf 'conda_base=%s\n' "$conda_base"
    } > "$tmp" || { rm -f "$tmp"; return 1; }
    chmod 600 "$tmp" || { rm -f "$tmp"; return 1; }
    mv -f "$tmp" "$file" || { rm -f "$tmp"; return 1; }
}
