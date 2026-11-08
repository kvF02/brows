# Interrupted profile-data recovery

Brows takes an exclusive OS file lock on its application data directory before opening production services. The lease is retained by the browser runtime and background data adapters, so a second instance cannot clean or modify data while the first owner is active. OS process termination releases the lock; a leftover lock file alone does not block startup.

Restore, MoreLogin migration and rollback persist a bounded per-profile intent before publishing directory changes. Pending intents block browser launch. Startup reconciles the filesystem with the SQLite publication state before exposing sessions:

| Interrupted operation | Startup action |
| --- | --- |
| Restore without a profile record | Remove only the unpublished restored userdata |
| Restore with a committed record | Preserve the restored userdata and clear the intent |
| Migration without completed metadata | Restore the original userdata; retain a complete replacement as `userdata.after-rollback-*` when present; mark the operation failed |
| Migration with completed metadata | Preserve active data and the rollback backup |
| Rollback before metadata completion | Finish the authorized directory swap and mark the record rolled back |

Recovery is repeatable. Records still marked running from older releases or a pre-intent crash are reconciled using their adapter-issued operation identities. Unknown versions, mismatched identities, links/junctions, conflicting generations and unprovable states preserve data and stop recovery. They are not resolved by deleting an arbitrary directory.

Only adapter-owned `.browsbak.partial` files and generated 32-hex extraction directories are removed during exclusive startup. Completed archives, unrelated files, existing profiles and rollback generations are retained. Intents contain identifiers only, never archive passwords, proxy credentials or profile contents. No browser is launched until its pending data operation has been reconciled.

Profile publication uses SQLite WAL with FULL synchronous commits. Intent files and copied profile files are flushed before directory publication; supported Unix directory flushes preserve rename ordering. Windows has no portable directory-fsync contract in this adapter. These measures and automated forced-process-termination/rename-boundary tests establish process-crash recovery, not a guarantee against every physical power cut, disk/controller failure or external modification. Real power-cut and storage-failure acceptance remains separate.

The migration port clears an intent only after its metadata transaction succeeds. A metadata failure leaves the profile blocked for restart recovery. A post-commit intent-cleanup failure does not report a successful import as failed and invite a duplicate import; the next startup consults the committed record.
