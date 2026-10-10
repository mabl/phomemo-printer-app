// Flatpak-only foreground supervisor and CLI environment, Apache-2.0.
#define _GNU_SOURCE
#include <arpa/inet.h>
#include <cups/cups.h>
#include <gio/gio.h>
#include <glib/gstdio.h>
#include <errno.h>
#include <fcntl.h>
#include <poll.h>
#include <signal.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/file.h>
#include <sys/prctl.h>
#include <sys/socket.h>
#include <sys/stat.h>
#include <sys/un.h>
#include <sys/wait.h>
#include <unistd.h>

#define APP_ID "io.github.mabl.phomemo-printer-app"
#ifndef REAL_BINARY
#define REAL_BINARY "/app/libexec/phomemo-printer-app"
#endif
static volatile sig_atomic_t interrupted;
static gboolean stop_requested;

static gboolean service_control(int control, pid_t *child, int port, const char *socket_path);
static int supervised_port(const char *control_path);

static void on_signal(int signo) { interrupted = signo; }

static void fail(const char *message) {
    fprintf(stderr, "Phomemo Flatpak: %s\n", message);
    exit(EXIT_FAILURE);
}

static int private_directory(const char *path, gboolean create) {
    if (!path || !g_path_is_absolute(path))
        fail("Directories must be absolute paths.");
    // A trailing slash makes O_NOFOLLOW follow a final directory symlink.
    // Strip separators only; realpath would hide the symlink we must reject.
    g_autofree char *normal = g_strdup(path);
    size_t length = strlen(normal);
    while (length > 1 && normal[length - 1] == '/') normal[--length] = '\0';
    if (create && g_mkdir_with_parents(normal, 0700))
        fail("Cannot create private application directory.");
    int fd = open(normal, O_RDONLY | O_DIRECTORY | O_NOFOLLOW | O_CLOEXEC);
    struct stat st;
    if (fd < 0 || fstat(fd, &st) || st.st_uid != geteuid() ||
        (st.st_mode & 0777) != 0700)
        fail("Application directories must be owned by this user, mode 0700, without a final symlink.");
    return fd;
}

static int environment(int *port, char **socket_path) {
    const char *base = g_getenv("XDG_RUNTIME_DIR");
    if (!base || !g_path_is_absolute(base))
        fail("XDG_RUNTIME_DIR must be set to an absolute path.");
    const char *override = g_getenv("PHOMEMO_RUNTIME_DIRECTORY");
    // Flatpak maps this per-app directory across independent invocations.
    // A directory directly below XDG_RUNTIME_DIR would be sandbox-local.
    g_autofree char *runtime = override ? g_strdup(override) :
        g_build_filename(base, "app", APP_ID, "r", NULL);
    *socket_path = g_build_filename(runtime, "phomemo-printer-app.sock", NULL);
    if (strlen(*socket_path) >= sizeof(((struct sockaddr_un *)0)->sun_path))
        fail("Runtime socket path exceeds Linux's 107-byte limit; use a shorter private runtime directory.");
    int runtime_fd = private_directory(runtime, !override);
    g_setenv("PHOMEMO_RUNTIME_DIRECTORY", runtime, TRUE);

    g_autofree char *data = g_build_filename(g_get_user_data_dir(), "phomemo", NULL);
    close(private_directory(data, TRUE));
    if (!g_getenv("PHOMEMO_STATE_FILE")) {
        g_autofree char *state = g_build_filename(data, "printer.state", NULL);
        g_setenv("PHOMEMO_STATE_FILE", state, TRUE);
    }
    if (!g_path_is_absolute(g_getenv("PHOMEMO_STATE_FILE")))
        fail("PHOMEMO_STATE_FILE must be absolute.");
    if (!g_getenv("PHOMEMO_SPOOL_DIRECTORY")) {
        g_autofree char *spool = g_build_filename(data, "spool", NULL);
        g_setenv("PHOMEMO_SPOOL_DIRECTORY", spool, TRUE);
    }
    close(private_directory(g_getenv("PHOMEMO_SPOOL_DIRECTORY"), TRUE));
    if (!g_getenv("PHOMEMO_SERVER_PORT"))
        g_setenv("PHOMEMO_SERVER_PORT", "8631", TRUE);
    const char *value = g_getenv("PHOMEMO_SERVER_PORT");
    char *end;
    errno = 0;
    long parsed = strtol(value, &end, 10);
    if (errno || !*value || *end || parsed < 1 || parsed > 65535)
        fail("PHOMEMO_SERVER_PORT must be between 1 and 65535.");
    *port = (int)parsed;
    // Desktop mode intentionally cannot publish an unauthenticated remote UI.
    g_setenv("PHOMEMO_LISTEN_HOSTNAME", "127.0.0.1", TRUE);
    g_setenv("PHOMEMO_TLS_ONLY", "false", TRUE);
    return runtime_fd;
}

static gboolean ipp_ready(const char *host) {
    // The UNIX endpoint's peer PID was already checked against our child.
    // This IPP round trip checks initialization, not endpoint identity.
    http_t *http = httpConnect2(host, 0, NULL, AF_UNSPEC,
                               HTTP_ENCRYPTION_NEVER, 1, 250, NULL);
    if (!http) return FALSE;
    httpSetTimeout(http, 1.0, NULL, NULL);
    ipp_t *request = ippNewRequest(IPP_OP_GET_SYSTEM_ATTRIBUTES);
    ippAddString(request, IPP_TAG_OPERATION, IPP_TAG_URI, "system-uri", NULL,
                 "ipp://localhost/ipp/system");
    ippAddString(request, IPP_TAG_OPERATION, IPP_TAG_KEYWORD,
                 "requested-attributes", NULL, "system-state");
    ipp_t *response = cupsDoRequest(http, request, "/ipp/system");
    gboolean available = response && ippGetStatusCode(response) <= IPP_STATUS_OK_EVENTS_COMPLETE &&
        ippFindAttribute(response, "system-state", IPP_TAG_ENUM);
    ippDelete(response);
    httpClose(http);
    return available;
}

static gboolean owns_socket(const char *path, pid_t child) {
    int fd = socket(AF_UNIX, SOCK_STREAM | SOCK_CLOEXEC | SOCK_NONBLOCK, 0);
    if (fd < 0) return FALSE;
    struct sockaddr_un address = { .sun_family = AF_UNIX };
    if (strlen(path) >= sizeof(address.sun_path)) { close(fd); return FALSE; }
    strcpy(address.sun_path, path);
    struct ucred peer;
    socklen_t length = sizeof(peer);
    gboolean owned = !connect(fd, (struct sockaddr *)&address, sizeof(address)) &&
        !getsockopt(fd, SOL_SOCKET, SO_PEERCRED, &peer, &length) &&
        peer.uid == geteuid() && peer.pid == child;
    close(fd);
    return owned;
}

static gboolean ready(const char *socket_path, pid_t child) {
    // Only our forked, still-unreaped child can satisfy this check. Both peers
    // live in the supervisor's PID namespace, even across Flatpak invocations.
    if (!owns_socket(socket_path, child)) return FALSE;
    return ipp_ready(socket_path);
}

typedef struct {
    gboolean done;
    guint response;
    const char *path;
    GVariant *reply;
    GError *error;
    gboolean called;
} Portal;

static void portal_called(GObject *object, GAsyncResult *result, gpointer data) {
    Portal *portal = data;
    portal->reply = g_dbus_connection_call_finish(G_DBUS_CONNECTION(object), result, &portal->error);
    if (portal->reply) g_variant_get(portal->reply, "(&o)", &portal->path);
    portal->called = TRUE;
}

static void portal_response(GDBusConnection *connection, const gchar *sender,
                           const gchar *path, const gchar *interface,
                           const gchar *signal, GVariant *parameters, gpointer data) {
    (void)connection; (void)sender; (void)interface; (void)signal;
    Portal *portal = data;
    if (strcmp(path, portal->path)) return;
    GVariant *results;
    g_variant_get(parameters, "(u@a{sv})", &portal->response, &results);
    g_variant_unref(results);
    portal->done = TRUE;
}

static gboolean open_website(int port, int control, pid_t *child, const char *socket_path,
                             const char *control_path) {
    g_autoptr(GError) error = NULL;
    g_autoptr(GDBusConnection) bus = g_bus_get_sync(G_BUS_TYPE_SESSION, NULL, &error);
    if (!bus) {
        fprintf(stderr, "Phomemo Flatpak: OpenURI portal unavailable: %s\n", error->message);
        return FALSE;
    }
    g_autofree char *token = g_strdup_printf("phomemo_%u_%u", (unsigned)getpid(), g_random_int());
    g_autofree char *sender = g_strdup(g_dbus_connection_get_unique_name(bus) + 1);
    for (char *p = sender; *p; p++) if (*p == '.') *p = '_';
    g_autofree char *expected = g_strdup_printf("/org/freedesktop/portal/desktop/request/%s/%s", sender, token);
    Portal portal = { .response = 2, .path = expected };
    guint subscription = g_dbus_connection_signal_subscribe(bus,
        "org.freedesktop.portal.Desktop", "org.freedesktop.portal.Request", "Response",
        NULL, NULL, G_DBUS_SIGNAL_FLAGS_NONE, portal_response, &portal, NULL);
    GVariantBuilder options;
    g_variant_builder_init(&options, G_VARIANT_TYPE_VARDICT);
    g_variant_builder_add(&options, "{sv}", "handle_token", g_variant_new_string(token));
    g_autofree char *uri = g_strdup_printf("http://127.0.0.1:%d/", port);
    g_autoptr(GCancellable) cancellation = g_cancellable_new();
    g_dbus_connection_call(bus,
        "org.freedesktop.portal.Desktop", "/org/freedesktop/portal/desktop",
        "org.freedesktop.portal.OpenURI", "OpenURI",
        g_variant_new("(ss@a{sv})", "", uri, g_variant_builder_end(&options)),
        G_VARIANT_TYPE("(o)"), G_DBUS_CALL_FLAGS_NONE, 10000, cancellation, portal_called, &portal);
    gint64 deadline = g_get_monotonic_time() + 90 * G_TIME_SPAN_SECOND;
    gint64 check_at = 0;
    while (!(portal.called && portal.done) && !interrupted && !stop_requested && g_get_monotonic_time() < deadline) {
        while (g_main_context_iteration(NULL, FALSE)) {}
        if (portal.called && !portal.reply) break;
        if (control >= 0) {
            service_control(control, child, port, socket_path);
            if (*child > 0 && waitpid(*child, NULL, WNOHANG) == *child) *child = -1;
            if (*child < 0) break;
        } else if (g_get_monotonic_time() >= check_at) {
            if (supervised_port(control_path) != port) break;
            check_at = g_get_monotonic_time() + 250000;
        }
        g_usleep(10000);
    }
    if (!portal.called) {
        g_cancellable_cancel(cancellation);
        while (!portal.called) g_main_context_iteration(NULL, TRUE);
    }
    if (!portal.done) {
        g_autoptr(GVariant) closed = g_dbus_connection_call_sync(bus,
            "org.freedesktop.portal.Desktop", portal.path, "org.freedesktop.portal.Request",
            "Close", NULL, NULL, G_DBUS_CALL_FLAGS_NONE, 1000, NULL, NULL);
    }
    g_dbus_connection_signal_unsubscribe(bus, subscription);
    gboolean opened = portal.reply && portal.done && !portal.response &&
        !stop_requested && !interrupted && (control < 0 || *child > 0);
    if (!opened) {
        fprintf(stderr, "Phomemo Flatpak: Website was not opened%s%s.\n",
                portal.error ? ": " : "", portal.error ? portal.error->message : "");
    }
    g_clear_pointer(&portal.reply, g_variant_unref);
    g_clear_error(&portal.error);
    return opened;
}

static void stop_child(pid_t child) {
    kill(child, SIGTERM);
    for (int i = 0; i < 100; i++) {
        pid_t result = waitpid(child, NULL, WNOHANG);
        if (result == child || (result < 0 && errno == ECHILD)) return;
        g_usleep(100000);
    }
    kill(child, SIGKILL);
    while (waitpid(child, NULL, 0) < 0 && errno == EINTR) {}
}

// PAPPL parses each -o argument as a CUPS option string, not an argv value.
// Quote filenames so spaces/quotes/backslashes retain their exact meaning.
static char *file_option(const char *name, const char *value) {
    GString *option = g_string_new(name);
    g_string_append(option, "=\"");
    for (const char *p = value; *p; p++) {
        if (*p == '\\' || *p == '"') g_string_append_c(option, '\\');
        g_string_append_c(option, *p);
    }
    g_string_append_c(option, '"');
    return g_string_free(option, FALSE);
}

static int control_socket(const char *path, gboolean listener) {
    int fd = socket(AF_UNIX, SOCK_STREAM | SOCK_CLOEXEC, 0);
    if (fd < 0) return -1;
    struct sockaddr_un address = { .sun_family = AF_UNIX };
    if (strlen(path) >= sizeof(address.sun_path)) { close(fd); return -1; }
    strcpy(address.sun_path, path);
    if (listener) {
        struct stat st;
        if (!lstat(path, &st)) {
            if (!S_ISSOCK(st.st_mode) || st.st_uid != geteuid()) { close(fd); return -1; }
            int probe = control_socket(path, FALSE);
            if (probe >= 0) { close(probe); close(fd); return -1; }
            if (errno != ECONNREFUSED && errno != ENOENT) { close(fd); return -1; }
            if (unlink(path)) { close(fd); return -1; }
        } else if (errno != ENOENT) { close(fd); return -1; }
        if (bind(fd, (struct sockaddr *)&address, sizeof(address)) || listen(fd, 8)) {
            close(fd); return -1;
        }
    } else if (connect(fd, (struct sockaddr *)&address, sizeof(address))) {
        int saved = errno;
        close(fd); errno = saved; return -1;
    }
    return fd;
}

static gboolean peer_is_user(int fd) {
    struct ucred peer;
    socklen_t length = sizeof(peer);
    return !getsockopt(fd, SOL_SOCKET, SO_PEERCRED, &peer, &length) && peer.uid == geteuid();
}

static gboolean request_stop(const char *path) {
    int fd = control_socket(path, FALSE);
    if (fd < 0) return FALSE;
    char reply;
    struct pollfd pollfd = { .fd = fd, .events = POLLIN };
    gboolean ok = peer_is_user(fd) && send(fd, "S", 1, MSG_NOSIGNAL) == 1 &&
        poll(&pollfd, 1, 15000) > 0 && recv(fd, &reply, 1, 0) == 1 && reply == 'O';
    close(fd);
    return ok;
}

static int supervised_port(const char *control_path) {
    int fd = control_socket(control_path, FALSE);
    if (fd < 0) return -1;
    struct timeval timeout = { .tv_sec = 2 };
    setsockopt(fd, SOL_SOCKET, SO_RCVTIMEO, &timeout, sizeof(timeout));
    uint16_t value;
    gboolean ok = peer_is_user(fd) && send(fd, "P", 1, MSG_NOSIGNAL) == 1 &&
        recv(fd, &value, sizeof(value), MSG_WAITALL) == sizeof(value);
    close(fd);
    return ok ? ntohs(value) : -1;
}

static gboolean service_control(int control, pid_t *child, int port, const char *socket_path) {
    struct pollfd pollfd = { .fd = control, .events = POLLIN };
    if (poll(&pollfd, 1, 0) <= 0) return FALSE;
    int client = accept4(control, NULL, NULL, SOCK_CLOEXEC);
    if (client < 0) return FALSE;
    struct pollfd request = { .fd = client, .events = POLLIN };
    char command;
    gboolean stopped = FALSE;
    if (peer_is_user(client) && poll(&request, 1, 1000) > 0 &&
        recv(client, &command, 1, 0) == 1 && *child > 0) {
        if (command == 'S') {
            stop_child(*child);
            *child = -1;
            stop_requested = TRUE;
            send(client, "O", 1, MSG_NOSIGNAL);
            stopped = TRUE;
        } else if (command == 'P' && owns_socket(socket_path, *child)) {
            // All packaged server starts force this binding before PAPPL opens
            // the UNIX socket. Only this supervisor can attest its child/port.
            uint16_t value = htons((uint16_t)port);
            send(client, &value, sizeof(value), MSG_NOSIGNAL);
        }
    }
    close(client);
    return stopped;
}

static int supervise(pid_t child, int control, const char *path, int port, const char *socket_path) {
    int status = 1;
    for (;;) {
        pid_t result = waitpid(child, &status, WNOHANG);
        if (result == child) break;
        if (interrupted || (result < 0 && errno != EINTR)) {
            stop_child(child); status = 0; break;
        }
        if (service_control(control, &child, port, socket_path)) { status = 0; break; }
        g_usleep(10000);
    }
    if (control >= 0) { close(control); unlink(path); }
    return WIFEXITED(status) ? WEXITSTATUS(status) : 1;
}

static void canonical_cli(int argc, char **argv) {
    static const char *commands[] = {
        "add", "autoadd", "cancel", "default", "delete", "devices", "drivers", "jobs",
        "modify", "options", "pause", "printers", "resume", "server", "shutdown", "status", "submit"
    };
    if (argc == 3 && (!strcmp(argv[2], "--help") || !strcmp(argv[2], "--version"))) return;
    gboolean known = FALSE;
    for (size_t i = 0; i < G_N_ELEMENTS(commands); i++)
        if (argc >= 3 && !strcmp(argv[2], commands[i])) known = TRUE;
    if (!known) fail("Flatpak CLI requires SUB-COMMAND first (or standalone --help/--version).");
    if (!strcmp(argv[2], "shutdown") && argc != 3)
        fail("Flatpak shutdown accepts no options; it stops the selected supervised server.");
    for (int i = 3; i < argc; i++) {
        const char *arg = argv[i];
        if (!strcmp(arg, "--")) {
            if (strcmp(argv[2], "submit") || i != argc - 2)
                fail("Flatpak '--' must precede the final submit filename.");
            return;
        }
        if (!strcmp(arg, "-a")) continue;
        if (strlen(arg) == 2 && arg[0] == '-' && strchr("dhjmnotuv", arg[1])) {
            if (++i >= argc) fail("Missing Flatpak CLI option value.");
            continue;
        }
        if (!strcmp(argv[2], "submit") && (arg[0] != '-' || !strcmp(arg, "-"))) {
            for (size_t j = 0; j < G_N_ELEMENTS(commands); j++)
                if (!strcmp(arg, commands[j])) fail("Use '-- FILENAME' for a reserved sub-command filename.");
            continue;
        }
        fail("Noncanonical Flatpak CLI arguments: use single-letter options after SUB-COMMAND.");
    }
}

int main(int argc, char **argv) {
    umask(0077);
    int port;
    g_autofree char *socket_path = NULL;
    int runtime_fd = environment(&port, &socket_path);
    gboolean cli = argc >= 2 && !strcmp(argv[1], "--cli");
    if (cli) canonical_cli(argc, argv);
    gboolean cli_server = cli && argc >= 3 && !strcmp(argv[2], "server");
    gboolean stop = (argc == 2 && !strcmp(argv[1], "--stop")) ||
        (cli && argc == 3 && !strcmp(argv[2], "shutdown"));
    if (cli && !cli_server && !stop) {
        argv[1] = REAL_BINARY;
        execv(REAL_BINARY, argv + 1);
        fail("Cannot execute packaged printer application.");
    }
    // The supervisor owns a fixed loopback/port/state/spool contract. Passing
    // endpoint options through would invalidate its attestation to a browser.
    for (int i = 3; cli_server && i < argc; i += 2) {
        if (i + 1 >= argc || strcmp(argv[i], "-o") ||
            (!g_str_has_prefix(argv[i + 1], "log-level=") && !g_str_has_prefix(argv[i + 1], "log-file=")))
            fail("Flatpak server accepts only '-o log-level=VALUE' and '-o log-file=VALUE'; use PHOMEMO_* environment settings for its binding/state/spool.");
    }
    if (argc != 1 && !stop && !cli_server) fail("Usage: phomemo-launcher [--stop | --cli SUB-COMMAND ...]");
    struct sigaction action = { .sa_handler = on_signal };
    sigemptyset(&action.sa_mask);
    sigaction(SIGTERM, &action, NULL);
    sigaction(SIGINT, &action, NULL);
    sigaction(SIGHUP, &action, NULL);
    g_autofree char *control_path = g_build_filename(g_getenv("PHOMEMO_RUNTIME_DIRECTORY"), "control.sock", NULL);

    // Stop must not wait behind startup's lock or a pending portal prompt.
    if (stop) {
        if (request_stop(control_path)) return 0;
        fail("No responsive supervised server; refusing an unsafe/raw shutdown fallback.");
    }

    int lock = openat(runtime_fd, "launcher.lock", O_CREAT | O_RDWR | O_NOFOLLOW | O_CLOEXEC, 0600);
    struct stat st;
    if (lock < 0 || fstat(lock, &st) || !S_ISREG(st.st_mode) || st.st_uid != geteuid() ||
        (st.st_mode & 0777) != 0600) fail("Unsafe launcher lock file.");
    // Only startup/portal work is serialized. The real server separately locks
    // the directory inode throughout its life, including direct CLI servers.
    while (flock(lock, LOCK_EX) < 0) {
        if (errno != EINTR || interrupted) fail("Cannot acquire launcher lock.");
    }

    pid_t child = -1;
    int control = -1;
    // Test the server's inode lock rather than trusting a stale PID/socket.
    if (!flock(runtime_fd, LOCK_EX | LOCK_NB)) {
        flock(runtime_fd, LOCK_UN);
        control = control_socket(control_path, TRUE);
        if (control < 0) fail("Cannot create safe supervisor control socket.");
        pid_t parent = getpid();
        child = fork();
        if (child < 0) fail("Cannot start server.");
        if (!child) {
            signal(SIGTERM, SIG_DFL); signal(SIGINT, SIG_DFL); signal(SIGHUP, SIG_DFL);
            // Even SIGKILL of the supervisor cannot orphan its server.
            if (prctl(PR_SET_PDEATHSIG, SIGTERM) || getppid() != parent) _exit(1);
            close(lock); close(runtime_fd);
            g_autofree char *port_option = g_strdup_printf("server-port=%d", port);
            g_autofree char *state_option = file_option("state-file", g_getenv("PHOMEMO_STATE_FILE"));
            g_autofree char *spool_option = file_option("spool-directory", g_getenv("PHOMEMO_SPOOL_DIRECTORY"));
            char **command = g_new0(char *, (size_t)argc + 14);
            char *fixed[] = { REAL_BINARY, "server", "-o", port_option,
                "-o", "listen-hostname=127.0.0.1", "-o", "tls-only=false",
                "-o", state_option, "-o", spool_option };
            size_t count = G_N_ELEMENTS(fixed);
            memcpy(command, fixed, sizeof(fixed));
            for (int i = 3; cli_server && i < argc; i += 2) {
                const char *equals = strchr(argv[i + 1], '=');
                g_autofree char *name = g_strndup(argv[i + 1], (size_t)(equals - argv[i + 1]));
                command[count++] = "-o";
                command[count++] = file_option(name, equals + 1);
            }
            execv(REAL_BINARY, command);
            _exit(127);
        }
    } else if (errno != EWOULDBLOCK && errno != EAGAIN) fail("Cannot inspect server lock.");
    else if (cli_server) fail("A server is already running in this runtime directory.");
    else if (supervised_port(control_path) != port)
        fail("Existing server has no matching supervisor binding contract; refusing to open a website.");

    gboolean available = FALSE;
    gint64 deadline = g_get_monotonic_time() + 15 * G_TIME_SPAN_SECOND;
    while (!interrupted && g_get_monotonic_time() < deadline) {
        if (child > 0 && waitpid(child, NULL, WNOHANG) == child) {
            child = -1;
            break;
        }
        if ((child > 0 && ready(socket_path, child)) ||
            (child < 0 && supervised_port(control_path) == port)) { available = TRUE; break; }
        g_usleep(100000);
    }
    gboolean opened = available && (cli_server || open_website(port, control, &child, socket_path, control_path));
    if (!opened) {
        if (child > 0) stop_child(child);
        if (control >= 0) { close(control); unlink(control_path); }
        if (stop_requested) return 0;
        fail("Launch failed; any server started by this invocation was stopped. Check port, logs and desktop portal.");
    }
    flock(lock, LOCK_UN);
    close(lock); close(runtime_fd);
    if (child < 0) return 0;  // Delegated to an existing server.
    if (cli_server) fprintf(stderr, "Phomemo Flatpak: Foreground CLI server running; use Ctrl-C, desktop Stop or CLI shutdown to stop.\n");
    else fprintf(stderr, "Phomemo Flatpak: Running at http://127.0.0.1:%d/; use the desktop Stop action or CLI shutdown to stop. Closing the browser does not stop printing.\n", port);
    return supervise(child, control, control_path, port, socket_path);
}
