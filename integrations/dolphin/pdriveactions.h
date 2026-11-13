// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Bertrand Presles
//
// "Keep Available Offline" and "Free Up Space" in the context menu of Dolphin, for the files
// of the PDrive mount only (a service menu cannot be limited to a folder, a plugin can).
#pragma once

#include <KAbstractFileItemActionPlugin>

#include "pdriveclient.h"

class PDriveActionsPlugin : public KAbstractFileItemActionPlugin
{
    Q_OBJECT
public:
    explicit PDriveActionsPlugin(QObject *parent = nullptr);

    QList<QAction *> actions(const KFileItemListProperties &fileItemInfos, QWidget *parentWidget) override;

private:
    PDriveClient *m_client;
};
