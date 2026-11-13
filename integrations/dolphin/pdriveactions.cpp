// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Bertrand Presles

#include "pdriveactions.h"

#include <KFileItemListProperties>
#include <KLocalizedString>
#include <KPluginFactory>

#include <QAction>
#include <QIcon>
#include <QWidget>

// Texts of the "pdrive" gettext domain, as in the Nautilus extension. i18nd names the domain
// each time: setting the application domain would change the one of Dolphin.

PDriveActionsPlugin::PDriveActionsPlugin(QObject *parent)
    : KAbstractFileItemActionPlugin(parent)
    , m_client(new PDriveClient(this))
{
}

QList<QAction *> PDriveActionsPlugin::actions(const KFileItemListProperties &fileItemInfos, QWidget *parentWidget)
{
    const QList<QUrl> urls = fileItemInfos.urlList();
    if (urls.isEmpty()) {
        return {};
    }
    QStringList paths;
    for (const QUrl &url : urls) {
        // One file outside the drive and there is nothing to offer.
        if (!m_client->isInMount(url)) {
            return {};
        }
        paths.append(url.toLocalFile());
    }

    auto *keep = new QAction(QIcon::fromTheme(QStringLiteral("emblem-pdrive-pinned")), i18nd("pdrive", "Keep Available Offline"), parentWidget);
    keep->setToolTip(i18nd("pdrive", "Download and keep available without network"));
    connect(keep, &QAction::triggered, m_client, [client = m_client, paths]() {
        client->keepOffline(paths);
    });

    auto *release = new QAction(QIcon::fromTheme(QStringLiteral("emblem-pdrive-online")), i18nd("pdrive", "Free Up Space"), parentWidget);
    release->setToolTip(i18nd("pdrive", "Remove the downloaded copy, keeping it in the cloud"));
    connect(release, &QAction::triggered, m_client, [client = m_client, paths]() {
        client->freeUpSpace(paths);
    });

    return {keep, release};
}

K_PLUGIN_CLASS_WITH_JSON(PDriveActionsPlugin, "pdriveactions.json")

#include "pdriveactions.moc"
