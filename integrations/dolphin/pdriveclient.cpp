// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Bertrand Presles

#include "pdriveclient.h"

#include <QDBusConnection>
#include <QDBusMessage>
#include <QDBusPendingCallWatcher>
#include <QDBusPendingReply>
#include <QDBusServiceWatcher>
#include <QDBusVariant>
#include <QDir>

namespace
{
const auto BusName = QStringLiteral("io.github.bpresles.PDrive");
const auto ObjectPath = QStringLiteral("/io/github/bpresles/PDrive");
const auto Interface = QStringLiteral("io.github.bpresles.PDrive1");
constexpr int CallTimeoutMs = 2000;

QString folderPrefix(const QString &path)
{
    if (path.isEmpty()) {
        return QString();
    }
    return QDir::cleanPath(path) + QLatin1Char('/');
}
}

PDriveClient::PDriveClient(QObject *parent)
    : QObject(parent)
{
    QDBusConnection bus = QDBusConnection::sessionBus();
    if (!bus.isConnected()) {
        return;
    }
    m_watcher = new QDBusServiceWatcher(BusName, bus, QDBusServiceWatcher::WatchForOwnerChange, this);
    connect(m_watcher, &QDBusServiceWatcher::serviceOwnerChanged, this, [this](const QString &, const QString &, const QString &newOwner) {
        onOwnerChanged(newOwner);
    });
    bus.connect(BusName, ObjectPath, Interface, QStringLiteral("FileStatusChanged"), this, SLOT(onFileStatusChanged(QString)));
    bus.connect(BusName, ObjectPath, Interface, QStringLiteral("MountStateChanged"), this, SLOT(onMountStateChanged(bool)));

    // The daemon may already run: NameHasOwner, asynchronously.
    auto call = QDBusMessage::createMethodCall(QStringLiteral("org.freedesktop.DBus"),
                                               QStringLiteral("/org/freedesktop/DBus"),
                                               QStringLiteral("org.freedesktop.DBus"),
                                               QStringLiteral("NameHasOwner"));
    call << BusName;
    auto *watcher = new QDBusPendingCallWatcher(bus.asyncCall(call, CallTimeoutMs), this);
    connect(watcher, &QDBusPendingCallWatcher::finished, this, [this](QDBusPendingCallWatcher *w) {
        w->deleteLater();
        const QDBusPendingReply<bool> reply = *w;
        if (reply.isValid() && reply.value() && !m_connected) {
            onOwnerChanged(QStringLiteral("running"));
        }
    });
}

bool PDriveClient::isReady() const
{
    return m_connected && !m_mountPrefix.isEmpty();
}

QString PDriveClient::mountPoint() const
{
    return m_mountPrefix.isEmpty() ? QString() : m_mountPrefix.chopped(1);
}

bool PDriveClient::isInMount(const QUrl &url) const
{
    return url.isLocalFile() && isInMount(url.toLocalFile());
}

bool PDriveClient::isInMount(const QString &path) const
{
    if (!m_connected) {
        return false;
    }
    return (!m_mountPrefix.isEmpty() && path.startsWith(m_mountPrefix)) || (!m_targetPrefix.isEmpty() && path.startsWith(m_targetPrefix));
}

void PDriveClient::clear()
{
    ++m_generation;
    m_connected = false;
    m_mountPrefix.clear();
    m_targetPrefix.clear();
}

void PDriveClient::onOwnerChanged(const QString &newOwner)
{
    clear();
    if (newOwner.isEmpty()) {
        Q_EMIT unmounted();
        return;
    }
    m_connected = true;
    loadMountPoints();
}

void PDriveClient::loadMountPoints()
{
    const int generation = m_generation;
    const struct {
        const char *property;
        bool isPoint;
    } properties[] = {{"MountPoint", true}, {"MountTarget", false}};
    for (const auto &entry : properties) {
        auto call = QDBusMessage::createMethodCall(BusName, ObjectPath, QStringLiteral("org.freedesktop.DBus.Properties"), QStringLiteral("Get"));
        call << Interface << QString::fromLatin1(entry.property);
        const bool isPoint = entry.isPoint;
        auto *watcher = new QDBusPendingCallWatcher(QDBusConnection::sessionBus().asyncCall(call, CallTimeoutMs), this);
        connect(watcher, &QDBusPendingCallWatcher::finished, this, [this, generation, isPoint](QDBusPendingCallWatcher *w) {
            w->deleteLater();
            const QDBusPendingReply<QDBusVariant> reply = *w;
            // The daemon changed (or vanished) meanwhile: this answer is stale.
            if (generation != m_generation || !reply.isValid()) {
                return;
            }
            const QString prefix = folderPrefix(reply.value().variant().toString());
            (isPoint ? m_mountPrefix : m_targetPrefix) = prefix;
            if (isPoint && !prefix.isEmpty()) {
                Q_EMIT mounted();
            }
        });
    }
}

void PDriveClient::fileStatus(const QString &path, std::function<void(const QString &)> done)
{
    auto call = QDBusMessage::createMethodCall(BusName, ObjectPath, Interface, QStringLiteral("GetFileStatus"));
    call << path;
    auto *watcher = new QDBusPendingCallWatcher(QDBusConnection::sessionBus().asyncCall(call, CallTimeoutMs), this);
    connect(watcher, &QDBusPendingCallWatcher::finished, this, [done = std::move(done)](QDBusPendingCallWatcher *w) {
        w->deleteLater();
        const QDBusPendingReply<QString> reply = *w;
        done(reply.isValid() ? reply.value() : QString());
    });
}

void PDriveClient::keepOffline(const QStringList &paths)
{
    callWithPaths(QStringLiteral("KeepOffline"), paths);
}

void PDriveClient::freeUpSpace(const QStringList &paths)
{
    callWithPaths(QStringLiteral("FreeUpSpace"), paths);
}

void PDriveClient::callWithPaths(const QString &method, const QStringList &paths)
{
    auto call = QDBusMessage::createMethodCall(BusName, ObjectPath, Interface, method);
    call << paths;
    // The daemon does the work in the background: nothing to wait for.
    QDBusConnection::sessionBus().asyncCall(call, CallTimeoutMs);
}

void PDriveClient::onFileStatusChanged(const QString &path)
{
    Q_EMIT fileStatusChanged(path);
}

void PDriveClient::onMountStateChanged(bool mounted)
{
    if (mounted) {
        // The mount points may differ in a new run of the daemon.
        loadMountPoints();
    } else {
        Q_EMIT unmounted();
    }
}
