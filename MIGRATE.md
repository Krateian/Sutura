# Moving from the Python version

[English](MIGRATE.md) · [Türkçe](MIGRATE.tr.md)

Sutura used to be a Python application. It is now a native program that does not
need Python, a virtualenv or conda. This page tells you what happens to an
existing Python install, what is kept, and how to get back if something goes
wrong.

Most people do not need to do anything: the last Python release can hand over to
the new build by itself. The rest of this page is for when you want to run it
yourself, or want to know exactly what it touched.

## The short version

* Your **history and learning data are kept**.
* The **previous install is saved** before anything is replaced.
* If the new build does not start, **the Python install is put back**.
* Nothing is removed until the new build has proved it runs.

## What is kept

| What | Where it ends up |
|---|---|
| Repair history | `~/.local/share/sutura/data/history.jsonl` |
| Triage learning counters | `~/.local/share/sutura/data/triage_learning.json` |
| Your settings | unchanged in `~/.config/sutura` |
| Anything else in `data/` | left exactly where it is |

The `data/` directory is never copied into a backup, never deleted during the
swap, and never restored over. That rule is the same one updates use: user data
belongs to you, not to the installer.

## Where the old version goes

**This is the part worth reading.** Before anything is replaced, the previous
install is saved as a backup. A Python-era backup has a different shape from the
new ones, so it is *quarantined* rather than mixed in with them:

```
~/.local/share/sutura.backup.python-<stamp>/
```

The stamp is the date and time of the migration. It is deliberately kept
**outside** the rotating `sutura.backup/` directory, because that directory keeps
only the two newest versions and silently deletes older ones. A Python backup
that got caught in that rotation would disappear the first time you updated
twice — and it is precisely the thing you want to still exist if you decide to go
back.

So after a migration you will have:

```
~/.local/share/sutura/                        the new install
~/.local/share/sutura/data/                   your data, untouched
~/.local/share/sutura.backup.python-<stamp>/  the whole Python install, kept
```

A later `--purge` uninstall removes that Python backup too. Until then it stays.

## Running the migration yourself

Normally the bridge release's updater does this for you. To run it by hand:

```sh
sutura --migrate-from-python --from-artifact /path/to/Sutura-rust-v<ver>-<arch>.<ext> --yes
```

Two things are required, and both refuse with an explanation rather than doing
something surprising:

* **`--from-artifact`** — the downloaded release package. The migration will not
  guess or fetch one for you, because it needs a specific verified tree.
* **`--yes`** — this removes the old runtime. Without it the command stops and
  says so. It does not prompt, so it is safe to run from a script that does not
  have one.

If there is no Python install to migrate, the command reports
`Nothing to migrate` and exits successfully. That is not an error.

## What actually happens, in order

1. **Quarantine.** A Python-era backup is moved to `sutura.backup.python-<stamp>/`.
2. **Data.** History and learning files are moved into the new `data/` directory.
3. **Backup and swap.** The current tree is backed up, then the new build is
   unpacked and put in place. `data/` is preserved throughout.
4. **Health check.** The new binary is run with `--version`. The output must
   match the expected version.
5. **Removal, only now.** The virtualenv (`venv`, `venv311`), any bundled Python
   and the Python sources are deleted. This is what makes the removal permanent —
   it happens after the check, never before.

If step 4 fails, the Python install comes back and the command tells you where it
was restored from. Nothing was lost.

## Licences

The migration does not ask you to accept a new licence, because it is not a
licence change — it installs the same PolyForm Noncommercial terms you already
had. A separate agreement is only needed for commercial use, as before.

## If you want to go back

Do nothing else and use the quarantined directory: it is a complete copy of the
Python install. Restore it over `~/.local/share/sutura` (keeping `data/`), or run
it from where it sits.

The new build's own `--rollback` flag restores the newest *update* backup, not the
Python one — that is why the Python backup is kept separately and not rotated.

## After migrating

* Check the version: `sutura --version`.
* Read the notes for the release you landed on: `sutura --whats-new`.
* Check for newer releases later: `sutura --check-update` (it only reports; it
  never installs anything).

Updates from now on are installed by `sutura --update`, which asks first and
answers "no" unless you say otherwise. See
[Install and requirements](docs/guide/install.md).

## A note on older saved release notes

Release notes are stored under `~/.local/share/sutura/data/whatsnew/`, which the
migration carries over with the rest of `data/`. If your Python install had saved
notes, `sutura --whats-new` may show those until the first update replaces them.
That is left alone on purpose rather than deleted.
