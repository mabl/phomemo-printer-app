# Shared by the command chain and configure hook. snapctl failures are fatal:
# silently substituting defaults could make the CLI register the wrong port.
phomemo_read_config() {
    PHOMEMO_SERVER_PORT=$(snapctl get port) || return
    PHOMEMO_LISTEN_HOSTNAME=$(snapctl get listen-hostname) || return
    PHOMEMO_SERVER_PORT=${PHOMEMO_SERVER_PORT:-8000}
    PHOMEMO_LISTEN_HOSTNAME=${PHOMEMO_LISTEN_HOSTNAME:-localhost}

    # Canonical decimal only: reject signs, whitespace, octal and port 0.
    case "$PHOMEMO_SERVER_PORT" in
        ''|0*|*[!0-9]*)
            printf '%s\n' 'port must be a decimal integer from 1 to 65535.' >&2
            return 1 ;;
    esac
    if [ "${#PHOMEMO_SERVER_PORT}" -gt 5 ] || [ "$PHOMEMO_SERVER_PORT" -gt 65535 ]; then
        printf '%s\n' 'port must be a decimal integer from 1 to 65535.' >&2
        return 1
    fi

    # Remote PAM authentication is not supported by this strictly confined snap.
    case "$PHOMEMO_LISTEN_HOSTNAME" in
        localhost|127.0.0.1|'[::1]'|'::1') ;;
        *)
            printf '%s\n' 'listen-hostname must be localhost, 127.0.0.1, [::1] or ::1.' >&2
            return 1 ;;
    esac
    export PHOMEMO_SERVER_PORT PHOMEMO_LISTEN_HOSTNAME
}
