// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Bertrand Presles
//
// Client of the pdrived daemon (D-Bus io.github.bpresles.PDrive1) for the Dolphin plugins.
// Nothing here blocks: the plugins run inside Dolphin. Without the daemon, every question
// is answered "not in the drive".
#pragma once

#include <QObject>
#include <QString>
#include <QStringList>
#include <QUrl>

#include <functional>

class QDBusServiceWatcher;

class PDriveClient : public QObject
{
    Q_OBJECT
public:
    explicit PDriveClient(QObject *parent = nullptr);

    /// Whether the daemon runs and its mount point is known.
    bool isReady() const;
    /// The mount point (the folder shown to the user), empty when unknown.
    QString mountPoint() const;
    /// Whether the URL is a local file strictly inside the mount point or the real mount folder.
    bool isInMount(const QUrl &url) const;
    bool isInMount(const QString &path) const;

    /// Asks the daemon for the state of a file ("online", "available", "pinned", "uploading", ...).
    /// `done` is called later, with an empty string when the daemon cannot answer.
    void fileStatus(const QString &path, std::function<void(const QString &)> done);
    void keepOffline(const QStringList &paths);
    void freeUpSpace(const QStringList &paths);

Q_SIGNALS:
    /// The daemon appeared or the drive was mounted, and the mount point is known.
    void mounted();
    /// The daemon vanished or the drive was unmounted.
    void unmounted();
    void fileStatusChanged(const QString &path);

private Q_SLOTS:
    void onFileStatusChanged(const QString &path);
    void onMountStateChanged(bool mounted);

private:
    void onOwnerChanged(const QString &newOwner);
    void loadMountPoints();
    void callWithPaths(const QString &method, const QStringList &paths);
    void clear();

    QDBusServiceWatcher *m_watcher = nullptr;
    bool m_connected = false;
    /// "/path/to/folder/" with the trailing slash, so that the folder itself is not inside.
    QString m_mountPrefix;
    QString m_targetPrefix;
    /// Incremented when the daemon changes: answers of an earlier one are ignored.
    int m_generation = 0;
};
