---- MODULE MCZip ----
EXTENDS ZipDecoders
\* Five entries, largest first as `read_archive_with_options` orders them.
\* The first asks for more than the whole budget, as a huge XZ entry does,
\* and writes behind; two reserve part of it; two reserve nothing.
NeedModel == <<6, 3, 0, 2, 0>>
BehindModel == <<TRUE, FALSE, TRUE, FALSE, FALSE>>
====
