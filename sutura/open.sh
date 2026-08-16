#!/usr/bin/env bash
SUTURA="$HOME/.local/bin/sutura"
GUI_PY="$HOME/.local/share/sutura/gui.py"
VENV_PY="$HOME/.local/share/sutura/venv/bin/python"

if [ "$#" -eq 1 ]; then
    exec "$VENV_PY" "$GUI_PY" "$1"
fi

report=""
for f in "$@"; do
    report+="--------------------------------\n"
    report+="$(basename "$f")\n"
    report+="$("$SUTURA" "$f" --human 2>/dev/null || echo 'repair failed')\n"
done

kdialog --title "Sutura - Summary" --msgbox "$report"