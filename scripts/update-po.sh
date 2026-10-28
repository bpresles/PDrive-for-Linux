#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-or-later
# Copyright (C) 2026 Bertrand Presles
# Regenerates po/pdrive.pot from the sources listed in po/POTFILES, then merges it into the
# catalogue of each language of po/LINGUAS. Start a new language with:
#   msginit --no-translator -i po/pdrive.pot -l LANG -o po/LANG.po   (and add LANG to po/LINGUAS)
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

mapfile -t RUST < <(grep '\.rs$' po/POTFILES)
mapfile -t C_FILES < <(grep '\.c$' po/POTFILES)
COMMON=(--from-code=UTF-8 --package-name=pdrive --msgid-bugs-address=https://github.com/bpresles/PDrive-for-Linux/issues --copyright-holder="the PDrive for Linux authors" --add-comments=TRANSLATORS --sort-by-file)

# Rust: tr and tr_fmt (message), tr_n (singular, plural). C: _().
rm -f po/pdrive.pot
xgettext "${COMMON[@]}" --language=Rust --keyword=tr:1 --keyword=tr_fmt:1 --keyword=tr_n:1,2 \
    --output=po/pdrive.pot "${RUST[@]}"
xgettext "${COMMON[@]}" --language=C --keyword=_ --join-existing --output=po/pdrive.pot "${C_FILES[@]}"

while read -r lang; do
    [[ -z "$lang" || "$lang" == \#* ]] && continue
    msgmerge --quiet --update --backup=none po/"$lang".po po/pdrive.pot
done < po/LINGUAS
