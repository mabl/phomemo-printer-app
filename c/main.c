//
// Phomemo Printer Application — PAPPL entry point.
//
// Thin C shell: system_cb with local-first defaults, device scheme
// registration, driver table (built dynamically from Rust model
// database), and papplMainloop.
//

#include <fcntl.h>
#include <errno.h>
#include <stdbool.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <strings.h>
#include <sys/wait.h>
#include <time.h>
#include <unistd.h>
#include <pappl/pappl.h>
#include "phomemo_pappl.h"

// Forward declarations from driver.c
extern bool tp_driver_cb(pappl_system_t *system, const char *driver_name,
    const char *device_uri, const char *device_id,
    pappl_pr_driver_data_t *dd, ipp_t **driver_attrs, void *data);
extern const char *tp_autoadd_cb(const char *device_info,
    const char *device_uri, const char *device_id, void *data);

// Forward declarations from media.c
extern void media_printer_created(pappl_printer_t *printer, void *data);

// Forward declarations from device_bt.c
extern bool bt_list_cb(pappl_device_cb_t cb, void *data,
    pappl_deverror_cb_t err_cb, void *err_data);
extern bool bt_open_cb(pappl_device_t *device, const char *device_uri,
    const char *name);
extern void bt_close_cb(pappl_device_t *device);
extern ssize_t bt_read_cb(pappl_device_t *device, void *buffer, size_t bytes);
extern ssize_t bt_write_cb(pappl_device_t *device, const void *buffer,
    size_t bytes);
extern pappl_preason_t bt_status_cb(pappl_device_t *device);
extern int bt_supplies_cb(pappl_device_t *device, int max_supplies,
    pappl_supply_t *supplies);
extern char *bt_id_cb(pappl_device_t *device, char *buffer, size_t bufsize);

// ---------------------------------------------------------------------------
// Driver table — built dynamically from the Rust model database
// ---------------------------------------------------------------------------

static pappl_pr_driver_t *drivers = NULL;
static int                num_drivers = 0;

static bool media_refresh_timer_cb(pappl_system_t *system, void *data) {
    (void)data;
    papplSystemIteratePrinters(system, media_printer_created, NULL);
    return false;
}

enum {
    DEFAULT_SERVER_PORT = 0
};

static int g_server_port = DEFAULT_SERVER_PORT;
static const char *g_listen_hostname = "localhost";
static const char *g_auth_service = NULL;
static const char *g_admin_group = NULL;
static const char *g_log_file = "-";
static pappl_loglevel_t g_log_level = PAPPL_LOGLEVEL_INFO;
static const char *g_spool_directory = NULL;
static bool g_tls_only = false;

static void build_driver_table(void) {
    unsigned n = pm_model_count();
    drivers = calloc(n, sizeof(*drivers));
    if (!drivers)
        return;
    num_drivers = (int)n;
    for (unsigned i = 0; i < n; i++) {
        const struct ModelInfoC *m = pm_model_get(i);
        drivers[i].name      = m->driver_name;
        drivers[i].description = m->name;
        drivers[i].device_id = m->device_id;
        drivers[i].extension = NULL;
    }
}

static bool parse_port(const char *value, int *port, bool allow_auto) {
    char *end = NULL;
    long parsed;

    if (!value || !*value || !port)
        return false;

    errno = 0;
    parsed = strtol(value, &end, 10);
    if (errno || !end || *end || parsed > 65535)
        return false;

    if (allow_auto) {
        if (parsed < 0)
            return false;
    } else if (parsed < 1) {
        return false;
    }

    *port = (int)parsed;
    return true;
}

static bool parse_port_strict(const char *value, int *port) {
    return parse_port(value, port, false);
}

static bool parse_port_or_auto(const char *value, int *port) {
    return parse_port(value, port, true);
}

static bool parse_bool(const char *value, bool *result) {
    if (!value || !*value || !result)
        return false;

    if (!strcasecmp(value, "1") || !strcasecmp(value, "true") ||
        !strcasecmp(value, "yes") || !strcasecmp(value, "on")) {
        *result = true;
        return true;
    }

    if (!strcasecmp(value, "0") || !strcasecmp(value, "false") ||
        !strcasecmp(value, "no") || !strcasecmp(value, "off")) {
        *result = false;
        return true;
    }

    return false;
}

static bool parse_log_level(const char *value, pappl_loglevel_t *level) {
    if (!value || !*value || !level)
        return false;

    if (!strcasecmp(value, "debug")) {
        *level = PAPPL_LOGLEVEL_DEBUG;
        return true;
    }
    if (!strcasecmp(value, "info")) {
        *level = PAPPL_LOGLEVEL_INFO;
        return true;
    }
    if (!strcasecmp(value, "warn") || !strcasecmp(value, "warning")) {
        *level = PAPPL_LOGLEVEL_WARN;
        return true;
    }
    if (!strcasecmp(value, "error")) {
        *level = PAPPL_LOGLEVEL_ERROR;
        return true;
    }
    if (!strcasecmp(value, "fatal")) {
        *level = PAPPL_LOGLEVEL_FATAL;
        return true;
    }

    return false;
}

static bool is_local_listener(const char *hostname) {
    return !hostname ||
           !strcmp(hostname, "localhost") ||
           !strcmp(hostname, "127.0.0.1") ||
           !strcmp(hostname, "::1");
}

static int unregister_local_cups_queue(const char *queue_name);

static int run_process_internal(char *const argv[], bool quiet) {
    pid_t pid = fork();
    int status = 0;

    if (pid < 0) {
        perror("fork");
        return 1;
    }

    if (pid == 0) {
        if (quiet) {
            int devnull = open("/dev/null", O_RDWR);
            if (devnull >= 0) {
                dup2(devnull, STDOUT_FILENO);
                dup2(devnull, STDERR_FILENO);
                close(devnull);
            }
        }

        execvp(argv[0], argv);
        perror(argv[0]);
        _exit(127);
    }

    if (waitpid(pid, &status, 0) < 0) {
        perror("waitpid");
        return 1;
    }

    if (WIFEXITED(status))
        return WEXITSTATUS(status);

    return 1;
}

static int run_process(char *const argv[]) {
    return run_process_internal(argv, false);
}

static int run_process_quiet(char *const argv[]) {
    return run_process_internal(argv, true);
}

static bool command_exists(const char *name) {
    const char *path = getenv("PATH");
    char *path_copy;
    char *saveptr = NULL;
    char *entry;

    if (!name || !*name)
        return false;

    if (strchr(name, '/'))
        return access(name, X_OK) == 0;

    if (!path || !*path)
        return false;

    path_copy = strdup(path);
    if (!path_copy)
        return false;

    entry = strtok_r(path_copy, ":", &saveptr);
    while (entry) {
        char candidate[1024];
        if (snprintf(candidate, sizeof(candidate), "%s/%s", entry, name) < (int)sizeof(candidate) &&
            access(candidate, X_OK) == 0) {
            free(path_copy);
            return true;
        }
        entry = strtok_r(NULL, ":", &saveptr);
    }

    free(path_copy);
    return false;
}

static bool cups_scheduler_running(void) {
    char *const argv[] = { "lpstat", "-r", NULL };
    return run_process_quiet(argv) == 0;
}

static bool cups_queue_exists(const char *queue_name) {
    char *const argv[] = { "lpstat", "-p", (char *)queue_name, NULL };
    return run_process_quiet(argv) == 0;
}

static int register_local_cups_queue(const char *queue_name, int port, bool replace) {
    char uri[128];

    if (!command_exists("lpadmin")) {
        fprintf(stderr, "lpadmin not found. Install CUPS client tools first.\n");
        return 127;
    }

    if (!command_exists("lpstat")) {
        fprintf(stderr, "lpstat not found. Install CUPS client tools first.\n");
        return 127;
    }

    if (!cups_scheduler_running()) {
        fprintf(stderr, "CUPS scheduler is not running or not reachable.\n");
        return 1;
    }

    if (cups_queue_exists(queue_name)) {
        if (!replace) {
            fprintf(stderr,
                    "Queue '%s' already exists. Use --replace to recreate it.\n",
                    queue_name);
            return 0;
        }

        if (unregister_local_cups_queue(queue_name) != 0) {
            fprintf(stderr, "Failed to remove existing queue '%s'.\n", queue_name);
            return 1;
        }
    }

    snprintf(uri, sizeof(uri), "ipp://localhost:%d/ipp/print", port);

    char *const argv[] = {
        "lpadmin",
        "-p", (char *)queue_name,
        "-E",
        "-v", uri,
        "-m", "everywhere",
        NULL
    };

    return run_process(argv);
}

static int unregister_local_cups_queue(const char *queue_name) {
    if (!command_exists("lpadmin")) {
        fprintf(stderr, "lpadmin not found. Install CUPS client tools first.\n");
        return 127;
    }

    if (!command_exists("lpstat")) {
        fprintf(stderr, "lpstat not found. Install CUPS client tools first.\n");
        return 127;
    }

    if (!cups_scheduler_running()) {
        fprintf(stderr, "CUPS scheduler is not running or not reachable.\n");
        return 1;
    }

    if (!cups_queue_exists(queue_name)) {
        fprintf(stderr, "Queue '%s' does not exist.\n", queue_name);
        return 0;
    }

    char *const argv[] = {
        "lpadmin",
        "-x", (char *)queue_name,
        NULL
    };

    return run_process(argv);
}

static void print_cups_subcommand_help(const char *progname) {
    fprintf(stderr,
            "Usage:\n"
            "  %s register-cups [--queue NAME] [--port PORT] [--replace]\n"
            "  %s unregister-cups [--queue NAME]\n",
            progname, progname);
}

static int run_cups_subcommand(int argc, char **argv) {
    bool is_register = !strcmp(argv[1], "register-cups");
    const char *queue_name = "phomemo";
    int port = g_server_port;
    bool replace = false;
    bool port_specified = false;

    for (int i = 2; i < argc; i ++) {
        if (!strcmp(argv[i], "--queue")) {
            if (i + 1 >= argc) {
                fprintf(stderr, "Missing value for --queue\n");
                return 2;
            }
            queue_name = argv[++i];
        } else if (!strcmp(argv[i], "--port")) {
            if (i + 1 >= argc) {
                fprintf(stderr, "Missing value for --port\n");
                return 2;
            }
            if (!parse_port_strict(argv[++i], &port)) {
                fprintf(stderr, "Invalid --port value\n");
                return 2;
            }
            port_specified = true;
        } else if (!strcmp(argv[i], "--help")) {
            print_cups_subcommand_help(argv[0]);
            return 0;
        } else if (!strcmp(argv[i], "--replace")) {
            if (!is_register) {
                fprintf(stderr, "--replace is only valid with register-cups\n");
                return 2;
            }
            replace = true;
        } else {
            fprintf(stderr, "Unknown option: %s\n", argv[i]);
            print_cups_subcommand_help(argv[0]);
            return 2;
        }
    }

    if (is_register && port < 1) {
        if (!port_specified) {
            fprintf(stderr,
                    "No fixed server port configured. Pass --port or set PHOMEMO_SERVER_PORT to a value from 1 to 65535.\n");
        } else {
            fprintf(stderr, "Invalid --port value\n");
        }
        return 2;
    }

    if (is_register)
        return register_local_cups_queue(queue_name, port, replace);

    if (!strcmp(argv[1], "unregister-cups"))
        return unregister_local_cups_queue(queue_name);

    return 2;
}

static void print_extra_help(void) {
    fputs("\nPhomemo-specific commands:\n"
          "  phomemo-printer-app register-cups [--queue NAME] [--port PORT] [--replace]\n"
          "  phomemo-printer-app unregister-cups [--queue NAME]\n"
          "\nEnvironment overrides:\n"
          "  PHOMEMO_SERVER_PORT, PHOMEMO_LISTEN_HOSTNAME, PHOMEMO_AUTH_SERVICE,\n"
          "  PHOMEMO_ADMIN_GROUP, PHOMEMO_LOG_FILE, PHOMEMO_LOG_LEVEL,\n"
          "  PHOMEMO_SPOOL_DIRECTORY, PHOMEMO_TLS_ONLY\n",
          stdout);
}

// ---------------------------------------------------------------------------
// system_cb — create a custom system (local-first, remote optional)
// ---------------------------------------------------------------------------

static pappl_system_t *
system_cb(int num_options, cups_option_t *options, void *data) {
    (void)data;

    int server_port = g_server_port;
    const char *listen_hostname = g_listen_hostname;
    const char *auth_service = g_auth_service;
    const char *admin_group = g_admin_group;
    const char *log_file = g_log_file;
    pappl_loglevel_t log_level = g_log_level;
    const char *spool_directory = g_spool_directory;
    bool tls_only = g_tls_only;
    bool remote_mode;

    const char *server_port_opt = cupsGetOption("server-port", num_options, options);
    const char *listen_opt = cupsGetOption("listen-hostname", num_options, options);
    const char *auth_opt = cupsGetOption("auth-service", num_options, options);
    const char *admin_group_opt = cupsGetOption("admin-group", num_options, options);
    const char *log_file_opt = cupsGetOption("log-file", num_options, options);
    const char *log_level_opt = cupsGetOption("log-level", num_options, options);
    const char *spool_opt = cupsGetOption("spool-directory", num_options, options);
    const char *tls_only_opt = cupsGetOption("tls-only", num_options, options);

    if (server_port_opt)
        parse_port_or_auto(server_port_opt, &server_port);

    if (listen_opt && *listen_opt)
        listen_hostname = listen_opt;

    if (auth_opt)
        auth_service = (*auth_opt) ? auth_opt : NULL;

    if (admin_group_opt)
        admin_group = (*admin_group_opt) ? admin_group_opt : NULL;

    if (log_file_opt && *log_file_opt)
        log_file = log_file_opt;

    if (log_level_opt)
        parse_log_level(log_level_opt, &log_level);

    if (spool_opt)
        spool_directory = (*spool_opt) ? spool_opt : NULL;

    if (tls_only_opt)
        parse_bool(tls_only_opt, &tls_only);

    remote_mode = !is_local_listener(listen_hostname);

    pappl_soptions_t soptions = PAPPL_SOPTIONS_WEB_INTERFACE;

    if (remote_mode)
        soptions |= PAPPL_SOPTIONS_WEB_REMOTE;

    if (!auth_service && remote_mode)
        auth_service = "cups";

    if (auth_service && *auth_service)
        soptions |= PAPPL_SOPTIONS_WEB_SECURITY;

    // Multi-queue — the BT connection manager maintains a per-MAC
    // connection map so multiple printers can coexist.
    soptions |= PAPPL_SOPTIONS_MULTI_QUEUE;
    pappl_system_t *system = papplSystemCreate(
        soptions,
        "Phomemo Printer App",
        server_port,
        NULL,       // no DNS-SD subtypes
        spool_directory,
        log_file,
        log_level,
        auth_service,
        tls_only
    );

    if (!system)
        return NULL;

    papplSystemAddListeners(system, listen_hostname);

    if (admin_group && *admin_group)
        papplSystemSetAdminGroup(system, admin_group);

    // Register Bluetooth SPP device scheme
    papplDeviceAddScheme2(
        "btspp",
        PAPPL_DEVTYPE_CUSTOM_LOCAL,
        bt_list_cb,
        bt_open_cb,
        bt_close_cb,
        bt_read_cb,
        bt_write_cb,
        bt_status_cb,
        bt_supplies_cb,
        bt_id_cb
    );

    // Register printer drivers
    papplSystemSetPrinterDrivers(system,
        num_drivers, drivers,
        tp_autoadd_cb,
        (pappl_pr_create_cb_t)media_printer_created,
        tp_driver_cb,
        data);

    // Ensure custom media resources/normalization are applied to printers
    // loaded from persisted state after startup.
    papplSystemIteratePrinters(system, media_printer_created, NULL);
    papplSystemAddTimerCallback(
        system,
        time(NULL) + 1,
        1,
        media_refresh_timer_cb,
        NULL);

    // Set version info for web UI
    pappl_version_t versions[1] = {
        { .name = "phomemo-printer-app", .sversion = "0.1.0",
          .version = { 0, 1, 0, 0 } }
    };
    papplSystemSetVersions(system, 1, versions);

    return system;
}

int main(int argc, char **argv) {
    bool show_extra_help = false;
    const char *env_port = getenv("PHOMEMO_SERVER_PORT");
    const char *env_listen = getenv("PHOMEMO_LISTEN_HOSTNAME");
    const char *env_auth_service = getenv("PHOMEMO_AUTH_SERVICE");
    const char *env_admin_group = getenv("PHOMEMO_ADMIN_GROUP");
    const char *env_log_file = getenv("PHOMEMO_LOG_FILE");
    const char *env_log_level = getenv("PHOMEMO_LOG_LEVEL");
    const char *env_spool_directory = getenv("PHOMEMO_SPOOL_DIRECTORY");
    const char *env_tls_only = getenv("PHOMEMO_TLS_ONLY");

    if (env_port)
        parse_port_or_auto(env_port, &g_server_port);

    if (env_listen && *env_listen)
        g_listen_hostname = env_listen;

    if (env_auth_service)
        g_auth_service = (*env_auth_service) ? env_auth_service : NULL;

    if (env_admin_group)
        g_admin_group = (*env_admin_group) ? env_admin_group : NULL;

    if (env_log_file && *env_log_file)
        g_log_file = env_log_file;

    if (env_log_level)
        parse_log_level(env_log_level, &g_log_level);

    if (env_spool_directory)
        g_spool_directory = (*env_spool_directory) ? env_spool_directory : NULL;

    if (env_tls_only)
        parse_bool(env_tls_only, &g_tls_only);

    if (argc > 1 &&
        (!strcmp(argv[1], "register-cups") || !strcmp(argv[1], "unregister-cups")))
        return run_cups_subcommand(argc, argv);

    if (argc > 1 &&
        (!strcmp(argv[1], "--help") || !strcmp(argv[1], "-h") || !strcmp(argv[1], "help")))
        show_extra_help = true;

    build_driver_table();

    int ret = papplMainloop(
        argc, argv,
        "0.1.0",
        "Phomemo Printer Application",
        num_drivers,
        drivers,
        tp_autoadd_cb,
        tp_driver_cb,
        NULL,               // subcmd_name
        NULL,               // subcmd_cb
        system_cb,
        NULL,               // usage_cb
        NULL                // data
    );

    if (show_extra_help)
        print_extra_help();

    return ret;
}
