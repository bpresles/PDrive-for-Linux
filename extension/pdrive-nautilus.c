/*
 * PDrive for Linux - Nautilus extension.
 *
 * Shows the sync state of the files of the PDrive mount as emblems, adds "Keep Available
 * Offline" and "Free Up Space" to their context menu, and reloads the windows when the drive is
 * mounted. All the work is done by the pdrived
 * daemon, queried over D-Bus (io.github.pdrive.PDrive1): this extension runs inside Nautilus
 * and is kept minimal, asynchronous and passive when the daemon is not running.
 *
 * SPDX-License-Identifier: GPL-3.0-or-later
 */

/* Translations: domain "pdrive", catalogues in LOCALEDIR (set by the Makefile). _() is
 * dgettext (GETTEXT_PACKAGE, ...), which keeps the domain of Nautilus untouched. */
#define GETTEXT_PACKAGE "pdrive"
#ifndef LOCALEDIR
#define LOCALEDIR "/usr/share/locale"
#endif

#include <glib/gi18n-lib.h>
#include <gio/gio.h>
#include <gmodule.h>
#include <gtk/gtk.h>
#include <nautilus-extension.h>

#define BUS_NAME "io.github.pdrive.PDrive"
#define OBJECT_PATH "/io/github/pdrive/PDrive"
#define INTERFACE "io.github.pdrive.PDrive1"
#define CALL_TIMEOUT_MS 2000

typedef struct
{
    GObject parent_instance;
} PDriveProvider;

typedef struct
{
    GObjectClass parent_class;
} PDriveProviderClass;

static GType provider_types[1];
static GDBusConnection *bus;
static guint watch_id;
static guint signal_id;
static guint mount_signal_id;
/* "file:///…/ProtonDrive/" while the daemon runs, NULL otherwise. */
static char *mount_uri_prefix;
/* Same for the real mount folder, the mount point being a link to it. */
static char *target_uri_prefix;

static gboolean
is_in_mount (const char *uri)
{
    return (mount_uri_prefix != NULL && g_str_has_prefix (uri, mount_uri_prefix)) ||
           (target_uri_prefix != NULL && g_str_has_prefix (uri, target_uri_prefix));
}

/* "file:///path/" for a folder path, NULL if it cannot be converted. */
static char *
folder_uri_prefix (const char *path)
{
    g_autofree char *uri = g_filename_to_uri (path, NULL, NULL);

    return uri != NULL ? g_strconcat (uri, "/", NULL) : NULL;
}

static char *
get_path_property (GDBusConnection *connection,
                   const char      *name)
{
    g_autoptr (GVariant) reply = NULL;
    g_autoptr (GVariant) value = NULL;

    reply = g_dbus_connection_call_sync (connection, BUS_NAME, OBJECT_PATH,
                                         "org.freedesktop.DBus.Properties", "Get",
                                         g_variant_new ("(ss)", INTERFACE, name),
                                         G_VARIANT_TYPE ("(v)"), G_DBUS_CALL_FLAGS_NONE,
                                         CALL_TIMEOUT_MS, NULL, NULL);
    if (reply == NULL)
        return NULL;

    g_variant_get (reply, "(v)", &value);
    return folder_uri_prefix (g_variant_get_string (value, NULL));
}

/* Emblem names; Nautilus looks up the "emblem-<name>" icon. */
static const char *
emblem_for_status (const char *status)
{
    if (g_str_equal (status, "uploading"))
        return "synchronizing";
    if (g_str_equal (status, "pinned"))
        return "pdrive-pinned";
    if (g_str_equal (status, "available"))
        return "pdrive-available";
    if (g_str_equal (status, "online"))
        return "pdrive-online";
    return NULL;
}

/* Daemon and mount point tracking */

static void
on_name_appeared (GDBusConnection *connection,
                  const char      *name,
                  const char      *owner,
                  gpointer         user_data)
{
    g_free (mount_uri_prefix);
    g_free (target_uri_prefix);
    mount_uri_prefix = get_path_property (connection, "MountPoint");
    target_uri_prefix = get_path_property (connection, "MountTarget");
}

static void
on_name_vanished (GDBusConnection *connection,
                  const char      *name,
                  gpointer         user_data)
{
    g_clear_pointer (&mount_uri_prefix, g_free);
    g_clear_pointer (&target_uri_prefix, g_free);
}

/* The state of a file changed: make Nautilus query it again if it shows it. */
static void
on_file_status_changed (GDBusConnection *connection,
                        const char      *sender,
                        const char      *object_path,
                        const char      *interface,
                        const char      *signal,
                        GVariant        *parameters,
                        gpointer         user_data)
{
    const char *path;
    g_autofree char *uri = NULL;
    g_autoptr (NautilusFileInfo) file = NULL;

    g_variant_get (parameters, "(&s)", &path);
    uri = g_filename_to_uri (path, NULL, NULL);
    if (uri == NULL)
        return;

    file = nautilus_file_info_lookup_for_uri (uri);
    if (file != NULL)
        nautilus_file_info_invalidate_extension_info (file);
}

/* Delay before reloading after a mount: Nautilus first processes the reappearance of the mount
 * point folder (a "created" event for the folder itself). */
#define RELOAD_DELAY_MS 600

static gboolean
reload_windows (gpointer user_data)
{
    GApplication *application = g_application_get_default ();

    if (GTK_IS_APPLICATION (application))
    {
        for (GList *l = gtk_application_get_windows (GTK_APPLICATION (application)); l != NULL; l = l->next)
            gtk_widget_activate_action (GTK_WIDGET (l->data), "slot.reload", NULL);
    }
    return G_SOURCE_REMOVE;
}

/* A mount over the mount point produces no file monitor event for its content, so the windows
 * showing it are reloaded. An unmount needs nothing: Nautilus gets an "unmounted" event for the
 * folder and leaves it by itself, which a reload would interfere with. */
static void
on_mount_state_changed (GDBusConnection *connection,
                        const char      *sender,
                        const char      *object_path,
                        const char      *interface,
                        const char      *signal,
                        GVariant        *parameters,
                        gpointer         user_data)
{
    gboolean mounted;

    g_variant_get (parameters, "(b)", &mounted);
    if (mounted)
        g_timeout_add (RELOAD_DELAY_MS, reload_windows, NULL);
}

/* Info provider: emblems */

typedef struct
{
    NautilusInfoProvider *provider;
    NautilusFileInfo *file;
    GClosure *update_complete;
    GCancellable *cancellable;
} StatusRequest;

static void
status_request_free (StatusRequest *request)
{
    g_object_unref (request->file);
    g_closure_unref (request->update_complete);
    g_object_unref (request->cancellable);
    g_free (request);
}

static void
on_status_reply (GObject      *source,
                 GAsyncResult *result,
                 gpointer      user_data)
{
    StatusRequest *request = user_data;
    g_autoptr (GVariant) reply = NULL;

    reply = g_dbus_connection_call_finish (G_DBUS_CONNECTION (source), result, NULL);

    /* Cancelled: Nautilus no longer waits for this update. */
    if (g_cancellable_is_cancelled (request->cancellable))
    {
        status_request_free (request);
        return;
    }

    if (reply != NULL)
    {
        const char *status;
        const char *emblem;

        g_variant_get (reply, "(&s)", &status);
        emblem = emblem_for_status (status);
        if (emblem != NULL)
            nautilus_file_info_add_emblem (request->file, emblem);
    }

    nautilus_info_provider_update_complete_invoke (request->update_complete, request->provider,
                                                   (NautilusOperationHandle *) request,
                                                   NAUTILUS_OPERATION_COMPLETE);
    status_request_free (request);
}

static NautilusOperationResult
update_file_info (NautilusInfoProvider     *provider,
                  NautilusFileInfo         *file,
                  GClosure                 *update_complete,
                  NautilusOperationHandle **handle)
{
    g_autofree char *uri = nautilus_file_info_get_uri (file);
    g_autofree char *path = NULL;
    StatusRequest *request;

    if (bus == NULL || !is_in_mount (uri))
        return NAUTILUS_OPERATION_COMPLETE;

    path = g_filename_from_uri (uri, NULL, NULL);
    if (path == NULL)
        return NAUTILUS_OPERATION_COMPLETE;

    request = g_new0 (StatusRequest, 1);
    request->provider = provider;
    request->file = g_object_ref (file);
    request->update_complete = g_closure_ref (update_complete);
    request->cancellable = g_cancellable_new ();

    g_dbus_connection_call (bus, BUS_NAME, OBJECT_PATH, INTERFACE, "GetFileStatus",
                            g_variant_new ("(s)", path), G_VARIANT_TYPE ("(s)"),
                            G_DBUS_CALL_FLAGS_NONE, CALL_TIMEOUT_MS, request->cancellable,
                            on_status_reply, request);

    *handle = (NautilusOperationHandle *) request;
    return NAUTILUS_OPERATION_IN_PROGRESS;
}

static void
cancel_update (NautilusInfoProvider    *provider,
               NautilusOperationHandle *handle)
{
    g_cancellable_cancel (((StatusRequest *) handle)->cancellable);
}

/* Menu provider: offline actions */

static void
on_menu_item_activate (NautilusMenuItem *item,
                       gpointer          method)
{
    GList *files = g_object_get_data (G_OBJECT (item), "pdrive-files");
    GVariantBuilder paths;

    g_variant_builder_init (&paths, G_VARIANT_TYPE ("as"));
    for (GList *l = files; l != NULL; l = l->next)
    {
        g_autofree char *uri = nautilus_file_info_get_uri (l->data);
        g_autofree char *path = g_filename_from_uri (uri, NULL, NULL);

        if (path != NULL)
            g_variant_builder_add (&paths, "s", path);
    }

    g_dbus_connection_call (bus, BUS_NAME, OBJECT_PATH, INTERFACE, method,
                            g_variant_new ("(as)", &paths), NULL, G_DBUS_CALL_FLAGS_NONE,
                            CALL_TIMEOUT_MS, NULL, NULL, NULL);
}

static NautilusMenuItem *
new_menu_item (const char *name,
               const char *label,
               const char *tip,
               const char *method,
               GList      *files)
{
    NautilusMenuItem *item = nautilus_menu_item_new (name, label, tip, NULL);

    g_object_set_data_full (G_OBJECT (item), "pdrive-files", nautilus_file_info_list_copy (files),
                            (GDestroyNotify) nautilus_file_info_list_free);
    g_signal_connect (item, "activate", G_CALLBACK (on_menu_item_activate), (gpointer) method);
    return item;
}

static GList *
get_file_items (NautilusMenuProvider *provider,
                GList                *files)
{
    GList *items = NULL;

    if (bus == NULL || files == NULL)
        return NULL;

    for (GList *l = files; l != NULL; l = l->next)
    {
        g_autofree char *uri = nautilus_file_info_get_uri (l->data);

        if (!is_in_mount (uri))
            return NULL;
    }

    items = g_list_append (items, new_menu_item ("PDrive::keep_offline", _("Keep Available Offline"),
                                                 _("Download and keep available without network"),
                                                 "KeepOffline", files));
    items = g_list_append (items, new_menu_item ("PDrive::free_up_space", _("Free Up Space"),
                                                 _("Remove the downloaded copy, keeping it in the cloud"),
                                                 "FreeUpSpace", files));
    return items;
}

/* Type and module */

static void
pdrive_provider_info_iface_init (NautilusInfoProviderInterface *iface)
{
    iface->update_file_info = update_file_info;
    iface->cancel_update = cancel_update;
}

static void
pdrive_provider_menu_iface_init (NautilusMenuProviderInterface *iface)
{
    iface->get_file_items = get_file_items;
}

G_DEFINE_DYNAMIC_TYPE_EXTENDED (PDriveProvider, pdrive_provider, G_TYPE_OBJECT, 0,
                                G_IMPLEMENT_INTERFACE_DYNAMIC (NAUTILUS_TYPE_INFO_PROVIDER,
                                                               pdrive_provider_info_iface_init)
                                G_IMPLEMENT_INTERFACE_DYNAMIC (NAUTILUS_TYPE_MENU_PROVIDER,
                                                               pdrive_provider_menu_iface_init))

static void
pdrive_provider_init (PDriveProvider *self)
{
}

static void
pdrive_provider_class_init (PDriveProviderClass *klass)
{
}

static void
pdrive_provider_class_finalize (PDriveProviderClass *klass)
{
}

G_MODULE_EXPORT void
nautilus_module_initialize (GTypeModule *module)
{
    bindtextdomain (GETTEXT_PACKAGE, LOCALEDIR);
    bind_textdomain_codeset (GETTEXT_PACKAGE, "UTF-8");

    pdrive_provider_register_type (module);
    provider_types[0] = pdrive_provider_get_type ();

    bus = g_bus_get_sync (G_BUS_TYPE_SESSION, NULL, NULL);
    if (bus == NULL)
        return;

    watch_id = g_bus_watch_name_on_connection (bus, BUS_NAME, G_BUS_NAME_WATCHER_FLAGS_NONE,
                                               on_name_appeared, on_name_vanished, NULL, NULL);
    signal_id = g_dbus_connection_signal_subscribe (bus, BUS_NAME, INTERFACE, "FileStatusChanged",
                                                    OBJECT_PATH, NULL, G_DBUS_SIGNAL_FLAGS_NONE,
                                                    on_file_status_changed, NULL, NULL);
    mount_signal_id = g_dbus_connection_signal_subscribe (bus, BUS_NAME, INTERFACE, "MountStateChanged",
                                                          OBJECT_PATH, NULL, G_DBUS_SIGNAL_FLAGS_NONE,
                                                          on_mount_state_changed, NULL, NULL);
}

G_MODULE_EXPORT void
nautilus_module_shutdown (void)
{
    if (bus != NULL)
    {
        g_dbus_connection_signal_unsubscribe (bus, signal_id);
        g_dbus_connection_signal_unsubscribe (bus, mount_signal_id);
        g_bus_unwatch_name (watch_id);
        g_clear_object (&bus);
    }
    g_clear_pointer (&mount_uri_prefix, g_free);
    g_clear_pointer (&target_uri_prefix, g_free);
}

G_MODULE_EXPORT void
nautilus_module_list_types (const GType **types,
                            int          *num_types)
{
    *types = provider_types;
    *num_types = G_N_ELEMENTS (provider_types);
}
