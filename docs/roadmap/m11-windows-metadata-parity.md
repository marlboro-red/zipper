# M11. Windows metadata parity

Files extracted on Windows carry the same safety marks and attributes as with 7-Zip.

Scope:

- Mark of the Web. When the archive has a `Zone.Identifier` stream, every extracted file gets one, as 7-Zip's `-snz` does. This comes first, because without it Windows stops warning about downloaded files once they leave the archive.
- The read-only, hidden, system and archive attributes from ZIP, 7z and tar entries made on Windows.
- Symlinks on Windows. Zipper creates links that stay inside the destination when the process may create them, and refuses the archive otherwise, with a clear error. Junctions are never created.
- NTFS alternate data streams stored by 7-Zip's `-sns`, behind an option, with names checked like paths. 7-Zip 26.05 stores them only in WIM, so they move to M17, and `src/ntfs_streams.rs` holds the stream writer they will use.

Exit checks:

1. A Windows CI test downloads a file through a path that sets `Zone.Identifier`, extracts it, and finds the mark on every output file.
2. A Windows corpus round-trips attributes and links, and its output matches 7-Zip's.
3. The adversarial review finds no stream name or link that writes outside the destination.
