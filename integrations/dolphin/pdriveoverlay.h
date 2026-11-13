// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Bertrand Presles
//
// Emblems for the sync state of the files of the PDrive mount, in Dolphin.
#pragma once

#include <KOverlayIconPlugin>

#include <QHash>
#include <QQueue>
#include <QSet>
#include <QStringList>

#include "pdriveclient.h"

class PDriveOverlayPlugin : public KOverlayIconPlugin
{
    Q_OBJECT
public:
    explicit PDriveOverlayPlugin(QObject *parent = nullptr);

    /// Never blocks: answers from the cache, or with no emblem while the daemon is asked
    /// (`overlaysChanged` follows with the answer).
    QStringList getOverlays(const QUrl &url) override;

private:
    void request(const QString &path);
    void pump();
    void forgetEverything();
    static QStringList overlaysForStatus(const QString &status);

    PDriveClient *m_client;
    QHash<QString, QStringList> m_cache;
    QQueue<QString> m_queue;
    QSet<QString> m_pending;
    int m_inflight = 0;
    /// Incremented when the cache is dropped: answers of an earlier generation are ignored.
    int m_generation = 0;
};
