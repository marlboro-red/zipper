---- MODULE MCUnix ----
EXTENDS FolderWriters
\* Two solid folders of two small files each, two folder workers and two
\* writers sharing one lane, as on Unix. Budget of two chunks, one job per
\* lane queue.
FilesModel == <<2, 2>>
LanesModel == 1
====
