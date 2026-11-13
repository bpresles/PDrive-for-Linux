// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Bertrand Presles

#include "pdriveoverlay.h"

#include <KPluginFactory>
#include <kdirnotify.h>

namespace
{
/// Bounds the memory: the cache is simply dropped when it is full, and fills up again with
/// what is shown.
constexpr int MaxCacheEntries = 10000;
/// Questions to the daemon in flight at the same time (a folder can hold thousands of files).
constexpr int MaxInflight = 16;
}

PDriveOverlayPlugin::PDriveOverlayPlugin(QObject *parent)
    : KOverlayIconPlugin(parent)
    , m_client(new PDriveClient(this))
{
    // The drive was just mounted: Dolphin does not see the content appear over the mount
    // point, so it is told to list the folder again.
    connect(m_client, &PDriveClient::mounted, this, [this]() {
        forgetEverything();
        org::kde::KDirNotify::emitFilesAdded(QUrl::fromLocalFile(m_client->mountPoint()));
    });
    connect(m_client, &PDriveClient::unmounted, this, &PDriveOverlayPlugin::forgetEverything);
    connect(m_client, &PDriveClient::fileStatusChanged, this, [this](const QString &path) {
        // Only what was asked for before is shown somewhere.
        if (m_cache.contains(path)) {
            request(path);
        }
    });
}

QStringList PDriveOverlayPlugin::overlaysForStatus(const QString &status)
{
    // The emblems of the Nautilus extension; "emblem-synchronizing" is in every icon theme,
    // the others are installed by the package (hicolor).
    if (status == QLatin1String("uploading")) {
        return {QStringLiteral("emblem-synchronizing")};
    }
    if (status == QLatin1String("pinned")) {
        return {QStringLiteral("emblem-pdrive-pinned")};
    }
    if (status == QLatin1String("available")) {
        return {QStringLiteral("emblem-pdrive-available")};
    }
    if (status == QLatin1String("online")) {
        return {QStringLiteral("emblem-pdrive-online")};
    }
    return {};
}

QStringList PDriveOverlayPlugin::getOverlays(const QUrl &url)
{
    if (!m_client->isInMount(url)) {
        return {};
    }
    const QString path = url.toLocalFile();
    const auto it = m_cache.constFind(path);
    if (it != m_cache.constEnd()) {
        return *it;
    }
    request(path);
    return {};
}

void PDriveOverlayPlugin::request(const QString &path)
{
    if (m_pending.contains(path)) {
        return;
    }
    m_pending.insert(path);
    m_queue.enqueue(path);
    pump();
}

void PDriveOverlayPlugin::pump()
{
    while (m_inflight < MaxInflight && !m_queue.isEmpty()) {
        const QString path = m_queue.dequeue();
        ++m_inflight;
        m_client->fileStatus(path, [this, path, generation = m_generation](const QString &status) {
            --m_inflight;
            if (generation == m_generation) {
                m_pending.remove(path);
                const QStringList overlays = overlaysForStatus(status);
                const QStringList shown = m_cache.value(path);
                if (m_cache.size() >= MaxCacheEntries) {
                    m_cache.clear();
                }
                m_cache.insert(path, overlays);
                if (overlays != shown) {
                    Q_EMIT overlaysChanged(QUrl::fromLocalFile(path), overlays);
                }
            }
            pump();
        });
    }
}

void PDriveOverlayPlugin::forgetEverything()
{
    ++m_generation;
    m_cache.clear();
    m_queue.clear();
    m_pending.clear();
}

K_PLUGIN_CLASS_WITH_JSON(PDriveOverlayPlugin, "pdriveoverlay.json")

#include "pdriveoverlay.moc"
