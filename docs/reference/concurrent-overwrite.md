# Concurrent extractions into one folder

Several `--overwrite` extractions can run at once into one destination folder on Linux and macOS. This page states what holds then and the conditions it needs.

## Conditions

- Every run uses `Existing::Overwrite`, which is `--overwrite` or `--existing overwrite`.
- The archives hold regular files and folders only. The guarantee covers no symlinks and no hard links. An archive with links keeps the contract that [usage](../usage.md#extract-into-an-existing-folder) gives for one extraction.
- The runs are on Linux or macOS. Windows is not tested here.
- Only Zipper runs change the destination during the extractions. The [usage page](../usage.md) asks for a private destination parent, and that still applies.
- The destination may exist before the runs start, or one of the runs may create it.
- Every folder the archives name lets its owner write into it. A run that creates a shared folder with a mode such as `0555` stops the other runs from placing files there, even when the runs follow one another.

## How runs take turns

A run holds a lock on the destination folder while it publishes, taken with `flock`. The lock is advisory, so it binds Zipper runs only.

- A run that can [replace a whole folder in one step](../usage.md#extract-into-an-existing-folder) needs the destination to itself. It tries for an exclusive lock without waiting, and it replaces folders only if it gets one. If its plan then holds no such folder, it changes to a shared lock before it moves anything.
- Every other run takes a shared lock, so runs that merge file by file publish at the same time. Such a run waits while another run holds the exclusive lock. It checks its cancel flag about every millisecond while it waits. After 10 seconds it stops waiting and merges file by file without a lock, so a lock that another program holds on the folder delays a run and never blocks it.
- A run takes no lock on a filesystem that has none. It then merges file by file.

Without the lock, a run that swapped a folder could take away the files another run had just placed in it. A run of an older Zipper takes no lock. A file it places in a folder just before a newer run swaps that folder is moved into the new folder, as the usage page describes.

## What holds when no file path is shared

The archives may share folder names, such as `bin/` and `lib/`, but no file path.

- No run fails because another run created a shared folder first. A run that finds a folder another run created after its plan merges into that folder.
- No run fails because another run's undo removed a shared folder. A run that finds such a folder gone makes it again and continues, and its own undo removes it again only if it is then empty. For each file it places, a run makes each parent folder again at most 64 times. A run whose new file or folder appears and vanishes again before it can merge tries that step again within the same bound. A run that needs more fails with the error of its last rename and undoes its work.
- Every run that succeeds has published every one of its files with the archive's bytes.
- No hidden `.zipper-` folder is left after a run that succeeds or fails, unless an undo fails as the usage page describes.
- A run that fails before publication, such as one with a damaged archive, changes nothing in the destination. The other runs are not affected.
- A run that fails or is cancelled during publication undoes only its own work. It moves back the files it placed and removes only the folders it created that are empty afterwards. A folder it created that another run has since put files into stays, with those files and their modes. It tells its own entries from the others by inode number and path. Where the system gives no inode number, as on Windows, it tells them by path alone.
- A folder that holds none of a successful run's files is not kept for that run. If another run created that folder and then fails or is cancelled, its undo removes the folder when it is empty. This holds even when the successful run's archive names the folder. Its files are always kept, along with every folder that holds them.

## What happens when a file path is shared

When two runs write the same file path, the path ends with the whole file from one archive. It never holds a mix of the bytes of both.

- If the path existed when both runs planned, each run replaces it by rename, and the last rename wins.
- Where the filesystem cannot swap two names, a run moves the old file into its hidden folder and then moves its own file in. A run that finds the old file already taken by another run moves its own file in. A run that finds another run's file in the way sets that file aside in turn, at most 64 times. An undo leaves a set-aside file in the hidden folder when another run's file now holds its path. On Windows two runs can each be told that they moved the same old file aside, and only one of them holds it. The other carries on as if the file had already been taken.
- If neither run saw the path, the first rename creates it. The other run stops with an error that the path appeared in the destination during extraction, and undoes its other files.

## How it is tested

`tests/concurrent_overwrite.rs` runs 8 ZIP archives at once into one folder for 6 rounds by default. Each archive has 121 files in shared folders. The rounds start from an empty folder, a folder that already holds the shared folders, a missing folder, and a folder that already holds every archive's files with other bytes. In that last start each archive names every entry of its own folder under `share/doc`, so the run that gets the exclusive lock replaces that folder in one step while the others wait. Every round compares every file on disk with its source and checks that no `.zipper-` folder is left. One test damages one archive so that it fails before publication. Another gives 4 archives a shared 1 MiB file. A fourth cancels every second run at varied times during its publication. The others must still publish every file, and a cancelled run must leave none of its own. `ZIPPER_CONCURRENT_ROUNDS` and `ZIPPER_CONCURRENT_ARCHIVES` raise the counts for a longer run. The cases that need an exact moment are tested in `src/staging.rs`. These are an undo in a folder that another run has used, a folder removed by another run's undo, a cancel before a merged step, and the mode unlock after a cancel. They also test a run that finds the destination shared and merges file by file, and a run that waits for the exclusive lock and is cancelled. One more test holds a lock that is never released, and the run stops waiting.
