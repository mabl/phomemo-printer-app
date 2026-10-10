// Test-only OpenURI service. Never installed in the application.
#include <gio/gio.h>
#include <stdio.h>
#include <string.h>

static const char xml[] =
    "<node><interface name='org.freedesktop.portal.OpenURI'>"
    "<method name='OpenURI'><arg type='s' direction='in'/>"
    "<arg type='s' direction='in'/><arg type='a{sv}' direction='in'/>"
    "<arg type='o' direction='out'/></method></interface></node>";
static const char request_xml[] =
    "<node><interface name='org.freedesktop.portal.Request'><method name='Close'/></interface></node>";
typedef struct { GDBusConnection *bus; char *path; guint code; guint object; guint source; } Reply;

static void log_line(const char *line) {
    const char *path = g_getenv("PHOMEMO_TEST_PORTAL_LOG");
    FILE *log = path ? fopen(path, "a") : NULL;
    if (log) { fprintf(log, "%s\n", line); fclose(log); }
}

static gboolean respond(gpointer data) {
    Reply *reply = data;
    GVariantBuilder results;
    g_variant_builder_init(&results, G_VARIANT_TYPE_VARDICT);
    g_dbus_connection_emit_signal(reply->bus, NULL, reply->path,
        "org.freedesktop.portal.Request", "Response",
        g_variant_new("(u@a{sv})", reply->code, g_variant_builder_end(&results)), NULL);
    g_dbus_connection_unregister_object(reply->bus, reply->object);
    g_object_unref(reply->bus);
    g_free(reply->path);
    g_free(reply);
    return G_SOURCE_REMOVE;
}

static void close_request(GDBusConnection *bus, const gchar *sender, const gchar *path,
                          const gchar *interface, const gchar *name, GVariant *parameters,
                          GDBusMethodInvocation *invocation, gpointer data) {
    (void)bus; (void)sender; (void)path; (void)interface; (void)name; (void)parameters;
    Reply *reply = data;
    log_line("closed");
    g_dbus_method_invocation_return_value(invocation, NULL);
    if (reply->source) g_source_remove(reply->source);
    reply->code = 1;
    respond(reply);
}

static void method(GDBusConnection *bus, const gchar *sender, const gchar *path,
                   const gchar *interface, const gchar *name, GVariant *parameters,
                   GDBusMethodInvocation *invocation, gpointer data) {
    (void)path; (void)interface; (void)name; (void)data;
    const char *parent, *uri, *token = NULL;
    GVariant *options;
    g_variant_get(parameters, "(&s&s@a{sv})", &parent, &uri, &options);
    (void)parent;
    g_variant_lookup(options, "handle_token", "&s", &token);
    if (!token) {
        g_dbus_method_invocation_return_dbus_error(invocation, "org.freedesktop.portal.Error.InvalidArgument", "Missing token");
        g_variant_unref(options);
        return;
    }
    g_autofree char *component = g_strdup(sender + 1);
    for (char *p = component; *p; p++) if (*p == '.') *p = '_';
    Reply *reply = g_new0(Reply, 1);
    reply->bus = g_object_ref(bus);
    reply->path = g_strdup_printf("/org/freedesktop/portal/desktop/request/%s/%s", component, token);
    reply->code = !g_strcmp0(g_getenv("PHOMEMO_TEST_PORTAL_MODE"), "refuse") ? 1 : 0;
    log_line(uri);
    g_autoptr(GDBusNodeInfo) request_info = g_dbus_node_info_new_for_xml(request_xml, NULL);
    static const GDBusInterfaceVTable request_vtable = { .method_call = close_request };
    reply->object = g_dbus_connection_register_object(bus, reply->path,
        request_info->interfaces[0], &request_vtable, reply, NULL, NULL);
    const char *mode = g_getenv("PHOMEMO_TEST_PORTAL_MODE");
    if (g_strcmp0(mode, "pending-method"))
        g_dbus_method_invocation_return_value(invocation, g_variant_new("(o)", reply->path));
    else g_object_ref(invocation);  // Test a method call that has not answered yet.
    g_variant_unref(options);
    if (g_strcmp0(mode, "pending") && g_strcmp0(mode, "pending-method"))
        reply->source = g_timeout_add(20, respond, reply);
}

int main(int argc, char **argv) {
    for (int i = 1; i < argc; i += 2) {
        if (i + 1 >= argc) return 2;
        if (!strcmp(argv[i], "--log")) g_setenv("PHOMEMO_TEST_PORTAL_LOG", argv[i + 1], TRUE);
        else if (!strcmp(argv[i], "--mode")) g_setenv("PHOMEMO_TEST_PORTAL_MODE", argv[i + 1], TRUE);
        else return 2;
    }
    g_autoptr(GError) error = NULL;
    GDBusConnection *bus = g_bus_get_sync(G_BUS_TYPE_SESSION, NULL, &error);
    if (!bus) return 1;
    g_autoptr(GDBusNodeInfo) info = g_dbus_node_info_new_for_xml(xml, NULL);
    static const GDBusInterfaceVTable vtable = { .method_call = method };
    if (!g_dbus_connection_register_object(bus, "/org/freedesktop/portal/desktop",
            info->interfaces[0], &vtable, NULL, NULL, &error)) return 1;
    g_autoptr(GVariant) result = g_dbus_connection_call_sync(bus, "org.freedesktop.DBus",
        "/org/freedesktop/DBus", "org.freedesktop.DBus", "RequestName",
        g_variant_new("(su)", "org.freedesktop.portal.Desktop", 4u),
        G_VARIANT_TYPE("(u)"), G_DBUS_CALL_FLAGS_NONE, 1000, NULL, &error);
    if (!result) return 1;
    guint acquired;
    g_variant_get(result, "(u)", &acquired);
    if (acquired != 1) return 1;  // Never pretend to own an existing host portal.
    puts("ready"); fflush(stdout);
    GMainLoop *loop = g_main_loop_new(NULL, FALSE);
    g_main_loop_run(loop);
    return 0;
}
