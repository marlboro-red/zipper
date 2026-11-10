# M8. Extraction API for applications

An application can call Zipper as a Rust library to update an installed folder, extract part of an archive, show progress and stop midway.

Scope:

- Extraction into an existing folder, where each archive file replaces the file of the same name by rename. Done with `zipper::Existing::Overwrite` and `zipper extract --overwrite`, in every reader.
- Selected folders and files from a ZIP archive. Done with `paths` in `native_zip::ReadOptions` and `zipper extract --path`.
- A progress callback that reports files and decoded bytes. Done for ZIP and tar with `zipper::Control`.
- Cancellation from another thread. A cancelled extraction changes nothing in the destination. Done for ZIP and tar, with `tests/control.rs`.
- One library function that picks the reader for an archive, as `zipper extract` does. Done with `zipper::extract_archive`.

Exit checks:

1. Tests show that a failed, rejected or cancelled extraction leaves an existing destination unchanged and leaves no staging folder.
2. The adversarial review finds no write outside the destination, including through links that already exist in it.
3. `docs/usage.md` and the reader references describe each option.
