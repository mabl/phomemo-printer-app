//
// Phomemo Printer Application — entry point.
//
// Runs PAPPL's main loop with the drivers from the Rust model table and a
// system callback that applies this application's settings, and adds the
// register-cups and unregister-cups sub-commands, which manage a queue for
// the server in the local CUPS scheduler.
//

#include <errno.h>
#include <fcntl.h>
#include <spawn.h>
#include <stddef.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <strings.h>
#include <sys/file.h>
#include <sys/socket.h>
#include <sys/stat.h>
#include <sys/un.h>
#include <sys/wait.h>
#include <unistd.h>
#include "phomemo.h"

#ifndef PHOMEMO_VERSION
#  error "PHOMEMO_VERSION must be defined by the build: see the Makefile."
#endif
_Static_assert(sizeof(PHOMEMO_VERSION) > 1, "PHOMEMO_VERSION is empty");

// The environment for posix_spawnp, which POSIX leaves to the application
// to declare.
extern char **environ;

// Native systemd service socket, accessible to clients of every account.
// Only systemd's RUNTIME_DIRECTORY authorizes a server to use this location.
// Packaged launchers instead supply PHOMEMO_RUNTIME_DIRECTORY to both server
// and clients, independently of PAPPL's configuration/state directories.
#ifndef SERVICE_DIRECTORY
#  define SERVICE_DIRECTORY "/run/phomemo-printer-app"
#endif

// What main hands papplMainloop's callbacks as their data.
typedef struct {
    const char        *name;         // the program's name, for messages
    pappl_pr_driver_t *drivers;      // one driver per model
    int                num_drivers;
    bool               server;       // whether the sub-command is "server"
    int                runtime_fd;   // private directory, locked while serving
} app_t;

static char runtime_socket[sizeof(((struct sockaddr_un *)NULL)->sun_path)];
static bool service_server;
static bool server_command;

// PAPPL 1.4's mainloop-support.c has no public socket-path callback. Its
// exported path helper is shared by the server and *all* CLI sub-commands.
// Interpose just that helper, retaining its Linux native path conventions.
// This requires shared PAPPL 1.x with interposable symbols; run the real-server
// runtime test against each packaged build to verify this integration.
char *_papplMainloopGetServerPath(const char *base_name, uid_t uid,
                                char *buffer, size_t bufsize);

char *_papplMainloopGetServerPath(const char *base_name, uid_t uid,
                                char *buffer, size_t bufsize) {
    const char *snap_common = getenv("SNAP_COMMON");

    if (runtime_socket[0])
        snprintf(buffer, bufsize, "%s", runtime_socket);
    else if (service_server || (!server_command && !uid && !snap_common &&
                               !access(SERVICE_DIRECTORY, X_OK)))
        snprintf(buffer, bufsize, SERVICE_DIRECTORY "/%s.sock", base_name);
    else if (uid)
        snprintf(buffer, bufsize, "%s/%s%lu.sock", papplGetTempDir(), base_name,
                 (unsigned long)uid);
    else
        snprintf(buffer, bufsize, "%s/%s.sock", snap_common ? snap_common : "/run",
                 base_name);

    return buffer;
}

// The launcher creates the directory. Fail closed on a bad setting rather
// than quietly connecting to a different server or starting a private one.
static bool runtime_init(app_t *app) {
    const char *directory = getenv("PHOMEMO_RUNTIME_DIRECTORY");
    if (!directory)
        return true;

    size_t length = strlen(directory);
    while (length > 1 && directory[length - 1] == '/')
        length--;
    int count = snprintf(runtime_socket, sizeof(runtime_socket), "%.*s/%s.sock",
                         (int)(length < sizeof(runtime_socket) ? length : sizeof(runtime_socket)),
                         directory, app->name);
    if (directory[0] != '/' || count < 0 || (size_t)count >= sizeof(runtime_socket)) {
        fprintf(stderr, "%s: PHOMEMO_RUNTIME_DIRECTORY must be a non-empty absolute "
                "path short enough for a UNIX socket.\n", app->name);
        return false;
    }

    char path[sizeof(runtime_socket)];
    snprintf(path, sizeof(path), "%.*s", (int)length, directory);
    app->runtime_fd = open(path, O_RDONLY | O_DIRECTORY | O_NOFOLLOW | O_CLOEXEC);
    struct stat st;
    if (app->runtime_fd < 0 || fstat(app->runtime_fd, &st) ||
        !S_ISDIR(st.st_mode) || st.st_uid != geteuid() || (st.st_mode & 0777) != 0700 ||
        access(path, W_OK | X_OK)) {
        fprintf(stderr, "%s: PHOMEMO_RUNTIME_DIRECTORY must be an existing writable "
                "private directory owned by the current user (mode 0700), not a symlink.\n",
                app->name);
        return false;
    }
    return true;
}

// PAPPL/libcups can unlink an existing domain socket before binding it.
// Serialize servers before any listener or state work, and reject a live
// socket belonging to an older/uncooperative server as well as non-sockets.
static bool runtime_claim(const app_t *app) {
    if (flock(app->runtime_fd, LOCK_EX | LOCK_NB)) {
        fprintf(stderr, "%s: Runtime directory already has a server, or cannot be locked: %s.\n",
                app->name, strerror(errno));
        return false;
    }

    struct stat st;
    if (lstat(runtime_socket, &st)) {
        if (errno == ENOENT)
            return true;
    } else if (S_ISSOCK(st.st_mode) && st.st_uid == geteuid()) {
        int fd = socket(AF_UNIX, SOCK_STREAM | SOCK_CLOEXEC | SOCK_NONBLOCK, 0);
        if (fd >= 0) {
            struct sockaddr_un address = { .sun_family = AF_UNIX };
            memcpy(address.sun_path, runtime_socket, strlen(runtime_socket) + 1);
            int result = connect(fd, (struct sockaddr *)&address, sizeof(address));
            int error = errno;
            close(fd);
            if (result < 0 && (error == ECONNREFUSED || error == ENOENT))
                return true;  // stale socket; PAPPL replaces it
        }
    }

    fprintf(stderr, "%s: Refusing to replace active or unsafe runtime socket %s.\n",
            app->name, runtime_socket);
    return false;
}

// Recognize aliases too: adding the CLI listener twice can unlink the first
// binding. The containing directory's inode identifies equivalent paths.
static bool same_socket_path(const char *a, const char *b) {
    const char *aslash = strrchr(a, '/');
    const char *bslash = strrchr(b, '/');
    if (!aslash || !bslash || strcmp(aslash, bslash))
        return false;
    char adir[1024], bdir[1024];
    if ((size_t)(aslash - a) >= sizeof(adir) || (size_t)(bslash - b) >= sizeof(bdir))
        return false;
    snprintf(adir, sizeof(adir), "%.*s/", (int)(aslash - a), a);
    snprintf(bdir, sizeof(bdir), "%.*s/", (int)(bslash - b), b);
    struct stat ast, bst;
    return !stat(adir, &ast) && !stat(bdir, &bst) &&
           ast.st_dev == bst.st_dev && ast.st_ino == bst.st_ino;
}

// ---------------------------------------------------------------------------
// Settings
// ---------------------------------------------------------------------------

// The server's settings: these defaults, overridden by the environment,
// overridden in turn by papplMainloop's options (-o NAME=VALUE and its
// configuration files). An empty value restores a setting's default, so an
// empty option undoes the environment.
typedef struct {
    int              server_port;      // 0 lets PAPPL choose
    const char      *listen_hostname;  // as papplSystemAddListeners takes it
    const char      *auth_service;     // PAM service, NULL for none
    const char      *admin_group;      // NULL for PAPPL's default
    const char      *log_file;         // "-" is stderr
    pappl_loglevel_t log_level;
    const char      *spool_directory;  // NULL for a temporary directory
    const char      *state_file;       // NULL for papplMainloop's choice
    bool             tls_only;
} settings_t;

static const settings_t settings_defaults = {
    .server_port     = 0,
    .listen_hostname = "localhost",
    .log_file        = "-",
    .log_level       = PAPPL_LOGLEVEL_INFO,
};

// Parse a port number from `min` to 65535.
static bool parse_port(const char *value, int min, int *port) {
    char *end;
    errno = 0;
    long parsed = strtol(value, &end, 10);
    if (errno || end == value || *end || parsed < min || parsed > 65535)
        return false;

    *port = (int)parsed;
    return true;
}

static bool set_server_port(settings_t *settings, const char *value) {
    return parse_port(value, 0, &settings->server_port);
}

static bool set_listen_hostname(settings_t *settings, const char *value) {
    settings->listen_hostname = value;
    return true;
}

static bool set_auth_service(settings_t *settings, const char *value) {
    settings->auth_service = value;
    return true;
}

static bool set_admin_group(settings_t *settings, const char *value) {
    settings->admin_group = value;
    return true;
}

static bool set_log_file(settings_t *settings, const char *value) {
    settings->log_file = value;
    return true;
}

static bool set_log_level(settings_t *settings, const char *value) {
    static const struct {
        const char      *name;
        pappl_loglevel_t level;
    } levels[] = {
        { "debug",   PAPPL_LOGLEVEL_DEBUG },
        { "info",    PAPPL_LOGLEVEL_INFO },
        { "warn",    PAPPL_LOGLEVEL_WARN },
        { "warning", PAPPL_LOGLEVEL_WARN },
        { "error",   PAPPL_LOGLEVEL_ERROR },
        { "fatal",   PAPPL_LOGLEVEL_FATAL },
    };

    for (size_t i = 0; i < sizeof(levels) / sizeof(levels[0]); i++) {
        if (!strcasecmp(value, levels[i].name)) {
            settings->log_level = levels[i].level;
            return true;
        }
    }
    return false;
}

static bool set_spool_directory(settings_t *settings, const char *value) {
    settings->spool_directory = value;
    return true;
}

static bool set_state_file(settings_t *settings, const char *value) {
    settings->state_file = value;
    return true;
}

static bool set_tls_only(settings_t *settings, const char *value) {
    static const struct {
        const char *name;
        bool        value;
    } booleans[] = {
        { "1", true },  { "true", true },   { "yes", true }, { "on", true },
        { "0", false }, { "false", false }, { "no", false }, { "off", false },
    };

    for (size_t i = 0; i < sizeof(booleans) / sizeof(booleans[0]); i++) {
        if (!strcasecmp(value, booleans[i].name)) {
            settings->tls_only = booleans[i].value;
            return true;
        }
    }
    return false;
}

// A setting: its option name and environment variable, what its values
// are, the setter that parses one, and the settings_t field it sets, which
// an empty value restores to its default. A setter returns false for an
// invalid value, which leaves the setting as it was.
typedef struct {
    const char *option;
    const char *env;
    const char *values;
    bool      (*set)(settings_t *settings, const char *value);
    size_t      offset;
    size_t      size;
} setting_t;

#define SETTING(option, env, values, set, field) \
    { option, env, values, set, offsetof(settings_t, field), \
      sizeof(((settings_t *)NULL)->field) }

static const setting_t settings_table[] = {
    SETTING("server-port", "PHOMEMO_SERVER_PORT",
            "a port number from 0 to 65535, 0 to let PAPPL choose",
            set_server_port, server_port),
    SETTING("listen-hostname", "PHOMEMO_LISTEN_HOSTNAME",
            "a host name, an IP address ([...] for IPv6), * for all, or a socket path",
            set_listen_hostname, listen_hostname),
    SETTING("auth-service", "PHOMEMO_AUTH_SERVICE", "a PAM service name",
            set_auth_service, auth_service),
    SETTING("admin-group", "PHOMEMO_ADMIN_GROUP", "a group name",
            set_admin_group, admin_group),
    SETTING("log-file", "PHOMEMO_LOG_FILE", "a file name, - for stderr, or syslog",
            set_log_file, log_file),
    SETTING("log-level", "PHOMEMO_LOG_LEVEL", "debug, info, warn, error or fatal",
            set_log_level, log_level),
    SETTING("spool-directory", "PHOMEMO_SPOOL_DIRECTORY", "a directory",
            set_spool_directory, spool_directory),
    SETTING("state-file", "PHOMEMO_STATE_FILE", "a file name",
            set_state_file, state_file),
    SETTING("tls-only", "PHOMEMO_TLS_ONLY", "1, true, yes, on, 0, false, no or off",
            set_tls_only, tls_only),
};

#define NUM_SETTINGS (sizeof(settings_table) / sizeof(settings_table[0]))

// Apply `value` of `setting`, named `source` where it came from: none
// leaves the setting as it is, an empty one restores its default. Nothing
// logs yet, so an invalid value is reported on stderr.
static void settings_apply(const app_t *app, settings_t *settings, const setting_t *setting,
                           const char *source, const char *value) {
    if (!value)
        return;

    if (!*value)
        memcpy((char *)settings + setting->offset,
               (const char *)&settings_defaults + setting->offset, setting->size);
    else if (!setting->set(settings, value))
        fprintf(stderr, "%s: Ignoring %s=\"%s\": expected %s.\n",
                app->name, source, value, setting->values);
}

// The settings in effect for the server: the defaults, the environment, and
// papplMainloop's options.
static settings_t settings_load(const app_t *app, int num_options, cups_option_t *options) {
    settings_t settings = settings_defaults;

    for (size_t i = 0; i < NUM_SETTINGS; i++) {
        const setting_t *setting = &settings_table[i];
        settings_apply(app, &settings, setting, setting->env, getenv(setting->env));
    }

    for (size_t i = 0; i < NUM_SETTINGS; i++) {
        const setting_t *setting = &settings_table[i];
        settings_apply(app, &settings, setting, setting->option,
                       cupsGetOption(setting->option, num_options, options));
    }

    return settings;
}

// Whether a listener on `hostname` serves this host only: the loopback
// interface or a domain socket.
static bool is_local_listener(const char *hostname) {
    return hostname[0] == '/' ||
           !strcasecmp(hostname, "localhost") ||
           !strcmp(hostname, "127.0.0.1") ||
           !strcmp(hostname, "[::1]") ||
           !strcmp(hostname, "::1");
}

// ---------------------------------------------------------------------------
// PAPPL callbacks
// ---------------------------------------------------------------------------

// Whether this process is the service: whether systemd made SERVICE_DIRECTORY
// one of its runtime directories.
static bool is_service(void) {
    const char *list = getenv("RUNTIME_DIRECTORY");
    const size_t length = strlen(SERVICE_DIRECTORY);

    for (const char *dir = list; dir; dir = strchr(dir, ':')) {
        if (*dir == ':')
            dir++;
        if (!strncmp(dir, SERVICE_DIRECTORY, length) && (!dir[length] || dir[length] == ':'))
            return true;
    }
    return false;
}

// The system's save callback: save its state to the file `data` names.
static bool save_state_cb(pappl_system_t *system, void *data) {
    return papplSystemSaveState(system, data);
}

// papplMainloop's system callback: create the system, local-first.
static pappl_system_t *system_cb(int num_options, cups_option_t *options, void *data) {
    const app_t *app = data;
    settings_t settings = settings_load(app, num_options, options);

    if (app->server && runtime_socket[0]) {
        if (cupsGetOption("private-server", num_options, options)) {
            fprintf(stderr, "%s: Private-server fallback is disabled with "
                    "PHOMEMO_RUNTIME_DIRECTORY; start the packaged server explicitly.\n",
                    app->name);
            return NULL;
        }
        if (!runtime_claim(app))
            return NULL;
    }

    // Multi-queue: the Bluetooth connection manager keeps one link per
    // printer address, so several printers can coexist.
    pappl_soptions_t soptions = PAPPL_SOPTIONS_WEB_INTERFACE | PAPPL_SOPTIONS_MULTI_QUEUE;

    // A local web interface needs no login; a remote one does, through PAM's
    // "cups" service unless another one is set.
    const char *auth_service = settings.auth_service;
    if (!is_local_listener(settings.listen_hostname)) {
        soptions |= PAPPL_SOPTIONS_WEB_REMOTE;
        if (!auth_service)
            auth_service = "cups";
    }
    if (auth_service)
        soptions |= PAPPL_SOPTIONS_WEB_SECURITY;

    pappl_system_t *system = papplSystemCreate(
        soptions,
        "Phomemo Printer App",
        settings.server_port,
        NULL,                     // no DNS-SD subtypes
        settings.spool_directory,
        settings.log_file,
        settings.log_level,
        auth_service,
        settings.tls_only);
    if (!system)
        return NULL;

    // The names of the overprint canvases and vendor options, merged into
    // PAPPL's own "en" strings, which papplSystemCreate has loaded and which
    // win for any key both have (loc.c), and served as printer-strings-uri.
    // PAPPL keeps the pointer: the catalog is static Rust data.
    papplSystemAddStringsData(system, "/en.strings", "en", pm_strings_en());

    if (app->server && auth_service && geteuid())
        papplLog(system, PAPPL_LOGLEVEL_WARN,
                 "Not running as root, so logins through PAM service \"%s\" fail for "
                 "accounts other than this one when pam_unix checks them: run the "
                 "service as root with its drop-in root.conf for remote logins.",
                 auth_service);

    // papplMainloop also creates a system to list its drivers, which needs
    // neither listeners nor state.
    if (app->server) {
        char socket_path[1024];
        _papplMainloopGetServerPath(app->name, getuid(), socket_path, sizeof(socket_path));
        // papplMainloop adds the CLI socket after this callback.
        if (!same_socket_path(settings.listen_hostname, socket_path) &&
            !papplSystemAddListeners(system, settings.listen_hostname)) {
            papplSystemDelete(system);
            return NULL;
        }
    }

    if (settings.admin_group)
        papplSystemSetAdminGroup(system, settings.admin_group);

    phomemo_bt_add_scheme();

    // Set here rather than left to papplMainloop, which sets no printer
    // creation callback.
    papplSystemSetPrinterDrivers(system, app->num_drivers, app->drivers,
                                 phomemo_autoadd_cb, phomemo_media_create_cb,
                                 phomemo_driver_cb, NULL);

    // papplMainloop picks a state file only for a system without a save
    // callback, so a configured one replaces it, and is loaded the way
    // papplMainloop loads its own: without saved state, the printers found
    // locally are added. The file name outlives the system: it is the
    // environment's or papplMainloop's options', and is only ever read.
    if (app->server && settings.state_file) {
        papplSystemSetSaveCallback(system, save_state_cb, (void *)settings.state_file);
        if (!papplSystemLoadState(system, settings.state_file))
            papplSystemCreatePrinters(system, PAPPL_DEVTYPE_LOCAL, NULL, NULL);
    }

    return system;
}

// Print the register-cups and unregister-cups usage.
static void cups_usage(FILE *fp, const char *name) {
    fprintf(fp,
            "Usage: %s register-cups [--queue NAME] [--port PORT] [--replace]\n"
            "       %s unregister-cups [--queue NAME]\n",
            name, name);
}

// papplMainloop's usage callback. It replaces PAPPL's own summary (usage()
// in PAPPL's mainloop.c), so this repeats it before this application's
// additions.
static void usage_cb(void *data) {
    const app_t *app = data;
    const char *name = app->name;

    printf("Usage: %s SUB-COMMAND [OPTIONS] [FILENAME]\n"
           "       %s [OPTIONS] [FILENAME]\n"
           "       %s [OPTIONS] -\n"
           "\n"
           "Sub-commands:\n"
           "  add PRINTER      Add a printer.\n"
           "  autoadd          Automatically add supported printers.\n"
           "  cancel           Cancel one or more jobs.\n"
           "  default          Set the default printer.\n"
           "  delete           Delete a printer.\n"
           "  devices          List devices.\n"
           "  drivers          List drivers.\n"
           "  jobs             List jobs.\n"
           "  modify           Modify a printer.\n"
           "  options          List printer options.\n"
           "  pause            Pause printing for a printer.\n"
           "  printers         List printers.\n"
           "  register-cups    Add a CUPS queue for the server.\n"
           "  resume           Resume printing for a printer.\n"
           "  server           Run a server.\n"
           "  shutdown         Shutdown a running server.\n"
           "  status           Show server/printer/job status.\n"
           "  submit           Submit a file for printing.\n"
           "  unregister-cups  Remove the CUPS queue.\n"
           "\n"
           "Options:\n"
           "  -a               Cancel all jobs (cancel).\n"
           "  -d PRINTER       Specify printer.\n"
           "  -j JOB-ID        Specify job ID (cancel).\n"
           "  -m DRIVER-NAME   Specify driver (add/modify).\n"
           "  -n COPIES        Specify number of copies (submit).\n"
           "  -o NAME=VALUE    Specify option (add,modify,server,submit).\n"
           "  -u URI           Specify ipp: or ipps: printer/server.\n"
           "  -v DEVICE-URI    Specify btspp:, socket: or usb: device (add/modify).\n"
           "\n",
           name, name, name);

    cups_usage(stdout, name);

    puts("\n"
         "Server settings, from the environment or as server options, which take\n"
         "precedence; an empty value restores the default:");
    for (size_t i = 0; i < NUM_SETTINGS; i++)
        printf("  %s, -o %s=VALUE\n      %s\n", settings_table[i].env,
               settings_table[i].option, settings_table[i].values);

    puts("\n"
         "Runtime socket (server and CLI):\n"
         "  PHOMEMO_RUNTIME_DIRECTORY  Existing writable private directory (mode 0700),\n"
         "      owned by the current user; non-empty absolute path, no final symlink.\n"
         "      Both server and CLI must use the same directory and executable name.\n"
         "      The full DIRECTORY/NAME.sock path must fit a UNIX socket (107 bytes on Linux).\n"
         "      Private-server auto-start is disabled; start 'server' explicitly.\n"
         "      Unset to use native systemd/PAPPL socket discovery.\n"
         "\n"
         "Bluetooth:\n"
         "  PHOMEMO_BT_CHANNELS      RFCOMM channels to try, comma-separated");
}

// ---------------------------------------------------------------------------
// CUPS queue sub-commands
// ---------------------------------------------------------------------------

// Run a CUPS client tool, with its output discarded if `quiet`; its exit
// status, or -1 if it could not be run or was killed (both reported).
static int cups_run(const char *name, char *const argv[], bool quiet) {
    posix_spawn_file_actions_t actions;
    int error = posix_spawn_file_actions_init(&actions);
    if (error) {
        fprintf(stderr, "%s: Unable to run %s: %s\n", name, argv[0], strerror(error));
        return -1;
    }

    if (quiet) {
        error = posix_spawn_file_actions_addopen(&actions, STDOUT_FILENO, "/dev/null",
                                                 O_WRONLY, 0);
        if (!error)
            error = posix_spawn_file_actions_adddup2(&actions, STDOUT_FILENO, STDERR_FILENO);
    }

    pid_t pid;
    if (!error)
        error = posix_spawnp(&pid, argv[0], &actions, NULL, argv, environ);
    posix_spawn_file_actions_destroy(&actions);

    if (error) {
        fprintf(stderr, "%s: Unable to run %s: %s%s\n", name, argv[0], strerror(error),
                error == ENOENT ? ". Install the CUPS client tools first." : "");
        return -1;
    }

    int status;
    while (waitpid(pid, &status, 0) < 0) {
        if (errno != EINTR) {
            fprintf(stderr, "%s: Unable to wait for %s: %s\n", name, argv[0], strerror(errno));
            return -1;
        }
    }

    if (WIFSIGNALED(status)) {
        fprintf(stderr, "%s: %s was killed by signal %d (%s).\n", name, argv[0],
                WTERMSIG(status), strsignal(WTERMSIG(status)));
        return -1;
    }

    return WIFEXITED(status) ? WEXITSTATUS(status) : -1;
}

// Whether the CUPS scheduler answers; says why not.
static bool cups_scheduler_running(const char *name) {
    char *const argv[] = { "lpstat", "-r", NULL };
    int status = cups_run(name, argv, true);

    if (status > 0)
        fprintf(stderr, "%s: The CUPS scheduler is not running or not reachable.\n", name);
    return status == 0;
}

static bool cups_queue_exists(const char *name, const char *queue) {
    char *const argv[] = { "lpstat", "-p", (char *)queue, NULL };
    return cups_run(name, argv, true) == 0;
}

static bool cups_delete_queue(const char *name, const char *queue) {
    char *const argv[] = { "lpadmin", "-x", (char *)queue, NULL };
    return cups_run(name, argv, false) == 0;
}

// Write the URI a local CUPS queue reaches the server at: the address it
// listens on, or localhost when it listens on every address. False, having
// said why, when there is none.
static bool cups_server_uri(const char *name, const settings_t *settings, int port,
                            char *uri, size_t urisize) {
    const char *listen = settings->listen_hostname;
    char host[256];

    if (listen[0] == '/') {
        fprintf(stderr, "%s: The server listens on domain socket %s, which an IPP "
                "Everywhere queue cannot use.\n", name, listen);
        return false;
    }

    if (!strcmp(listen, "*") || !strcmp(listen, "0.0.0.0") || !strcmp(listen, "[::]"))
        listen = "localhost";

    // PAPPL takes an IPv6 address in brackets; httpAssembleURI adds them.
    size_t length = strlen(listen);
    bool bracketed = listen[0] == '[' && length > 2 && listen[length - 1] == ']';
    int host_length = bracketed
        ? snprintf(host, sizeof(host), "%.*s", (int)(length - 2), listen + 1)
        : snprintf(host, sizeof(host), "%s", listen);

    if (host_length < 0 || (size_t)host_length >= sizeof(host) ||
        httpAssembleURI(HTTP_URI_CODING_ALL, uri, (int)urisize,
                        settings->tls_only ? "ipps" : "ipp", NULL, host, port,
                        "/ipp/print") < HTTP_URI_STATUS_OK) {
        fprintf(stderr, "%s: Unable to make a URI for listen hostname %s.\n", name, listen);
        return false;
    }

    return true;
}

static int cups_register(const char *name, const settings_t *settings,
                         const char *queue, int port, bool replace) {
    char uri[1024];
    if (!cups_server_uri(name, settings, port, uri, sizeof(uri)) ||
        !cups_scheduler_running(name))
        return 1;

    if (cups_queue_exists(name, queue)) {
        if (!replace) {
            fprintf(stderr, "%s: Queue '%s' already exists. Use --replace to recreate it.\n",
                    name, queue);
            return 0;
        }

        if (!cups_delete_queue(name, queue)) {
            fprintf(stderr, "%s: Unable to remove existing queue '%s'.\n", name, queue);
            return 1;
        }
    }

    char *const argv[] = {
        "lpadmin", "-p", (char *)queue, "-E", "-v", uri, "-m", "everywhere", NULL
    };
    return cups_run(name, argv, false) == 0 ? 0 : 1;
}

static int cups_unregister(const char *name, const char *queue) {
    if (!cups_scheduler_running(name))
        return 1;

    if (!cups_queue_exists(name, queue)) {
        fprintf(stderr, "%s: Queue '%s' does not exist.\n", name, queue);
        return 0;
    }

    return cups_delete_queue(name, queue) ? 0 : 1;
}

// Run register-cups or unregister-cups, argv[1]: 0 on success, 1 on
// failure, 2 for invalid arguments.
static int cups_subcommand(const app_t *app, int argc, char *argv[]) {
    const char *name = app->name;
    bool registering = !strcmp(argv[1], "register-cups");
    settings_t settings = settings_load(app, 0, NULL);
    const char *queue = "phomemo";
    int port = settings.server_port;
    bool replace = false;

    for (int i = 2; i < argc; i++) {
        const char *option = argv[i];

        if (!strcmp(option, "--help")) {
            cups_usage(stdout, name);
            return 0;
        } else if (registering && !strcmp(option, "--replace")) {
            replace = true;
        } else if (!strcmp(option, "--queue") || (registering && !strcmp(option, "--port"))) {
            const char *value = argv[++i];  // argv[argc] is NULL
            if (!value) {
                fprintf(stderr, "%s: Missing value for %s.\n", name, option);
                return 2;
            }

            if (!strcmp(option, "--queue")) {
                queue = value;
            } else if (!parse_port(value, 1, &port)) {
                fprintf(stderr, "%s: Invalid --port value \"%s\": expected a port number "
                        "from 1 to 65535.\n", name, value);
                return 2;
            }
        } else {
            fprintf(stderr, "%s: Unknown option '%s'.\n", name, option);
            cups_usage(stderr, name);
            return 2;
        }
    }

    if (!registering)
        return cups_unregister(name, queue);

    if (port == 0) {
        fprintf(stderr, "%s: No fixed server port is configured. Pass --port, or set "
                "PHOMEMO_SERVER_PORT to a port number from 1 to 65535.\n", name);
        return 2;
    }

    return cups_register(name, &settings, queue, port, replace);
}

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

// Fill in PAPPL's driver table from the Rust model table.
static bool build_driver_table(app_t *app) {
    unsigned count = pm_model_count();
    pappl_pr_driver_t *drivers = calloc(count, sizeof(*drivers));
    if (!drivers)
        return false;

    for (unsigned i = 0; i < count; i++) {
        const PmModel *model = pm_model_get(i);
        drivers[i] = (pappl_pr_driver_t) {
            .name        = model->driver_name,
            .description = model->name,
            .device_id   = model->device_id,
        };
    }

    app->drivers = drivers;
    app->num_drivers = (int)count;
    return true;
}

// Whether papplMainloop will run the server: whether "server" is among its
// arguments, other than as an option's value or after "--", which precedes
// a file name. papplMainloop accepts one sub-command anywhere.
static bool runs_server(int argc, char *argv[]) {
    for (int i = 1; i < argc; i++) {
        const char *arg = argv[i];

        if (!strcmp(arg, "--")) {
            i++;
        } else if (arg[0] == '-' && arg[1]) {
            // PAPPL 1.4 mainloop.c switches on argv[i][1], not *opt.
            // Consuming a value changes i *inside* this loop, so later
            // letters inspect that value: "-dd -a server" runs a server.
            // Match that quirk exactly; classification gates socket safety.
            for (const char *opt = arg + 1; *opt; opt++) {
                if (!argv[i][0] || !argv[i][1])
                    return false;
                if (strchr("dhjmnotuv", argv[i][1])) {
                    if (++i >= argc)
                        return false;
                } else if (argv[i][1] != 'a') {
                    return false;  // PAPPL will report an unknown option
                }
            }
        } else if (!strcmp(arg, "server")) {
            return true;
        }
    }
    return false;
}

int main(int argc, char *argv[]) {
    const char *path = argc > 0 ? argv[0] : "phomemo-printer-app";
    const char *slash = strrchr(path, '/');
    app_t app = {
        .name   = slash ? slash + 1 : path,
        .server = runs_server(argc, argv),
        .runtime_fd = -1,
    };

    if (argc > 1 &&
        (!strcmp(argv[1], "register-cups") || !strcmp(argv[1], "unregister-cups")))
        return cups_subcommand(&app, argc, argv);

    // Standalone help/version need no runtime directory, even with a broken
    // launcher. Leave compound command parsing and errors to papplMainloop.
    if (argc == 2 && (!strcmp(argv[1], "--help") || !strcmp(argv[1], "--version"))) {
        if (!strcmp(argv[1], "--help"))
            usage_cb(&app);
        else
            puts(PHOMEMO_VERSION);
        return 0;
    }

    if (!runtime_init(&app)) {
        if (app.runtime_fd >= 0)
            close(app.runtime_fd);
        return 1;
    }
    service_server = app.server && !getenv("SNAP_COMMON") && is_service();
    server_command = app.server;

    if (!build_driver_table(&app)) {
        fprintf(stderr, "%s: Unable to allocate the driver table.\n", app.name);
        if (app.runtime_fd >= 0)
            close(app.runtime_fd);
        return 1;
    }

    // The web interface needs a footer: PAPPL 1.4 looks a NULL one up as a
    // localized string (papplClientHTMLFooter), which crashes.
    int status = papplMainloop(argc, argv, PHOMEMO_VERSION,
                               "Phomemo Printer Application",
                               app.num_drivers, app.drivers,
                               phomemo_autoadd_cb, phomemo_driver_cb,
                               NULL, NULL,  // no custom sub-command
                               system_cb, usage_cb, &app);

    free(app.drivers);
    if (app.runtime_fd >= 0)
        close(app.runtime_fd);
    return status;
}
