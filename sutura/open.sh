#!/usr/bin/env bash
SUTURA="$HOME/.local/bin/sutura"
GUI_PY="$HOME/.local/share/sutura/gui.py"
VENV_PY="$HOME/.local/share/sutura/venv/bin/python"

if [ "$#" -eq 1 ]; then
    exec "$VENV_PY" "$GUI_PY" "$1"
fi

N="$#"
# kdialog --progressbar returns "service /ProgressDialog" as one string;
# the classic idiom is to use $dbus UNQUOTED so the shell splits it.
dbus=$(kdialog --progressbar "Onarılıyor: 0/$N" "$N")
qdbus $dbus org.kde.kdialog.ProgressDialog.showCancelButton true >/dev/null 2>&1

report=""
i=0
for f in "$@"; do
    i=$((i + 1))
    qdbus $dbus org.kde.kdialog.ProgressDialog.setLabelText \
        "Onarılıyor: $i/$N — $(basename "$f")" >/dev/null 2>&1
    qdbus $dbus org.kde.kdialog.ProgressDialog.value "$i" >/dev/null 2>&1
    report+="--------------------------------\n"
    report+="$(basename "$f")\n"
    report+="$("$SUTURA" "$f" --human 2>/dev/null || echo 'repair failed')\n"
    if qdbus $dbus org.kde.kdialog.ProgressDialog.wasCancelled 2>/dev/null | grep -qi true; then
        report+="(cancelled)\n"
        break
    fi
done

qdbus $dbus org.kde.kdialog.ProgressDialog.close >/dev/null 2>&1
kdialog --title "Sutura - Summary" --msgbox "$report"