---- MODULE MCUnix ----
EXTENDS WriterPool
\* Four files of 2, 3, 1 and 2 chunks; the fourth repeats an earlier name.
\* Three writers share one lane, as on Unix. Budget of three chunks, two
\* jobs per lane queue and two chunks per file stream.
FilesModel == <<2, 3, 1, 2>>
LanesModel == 1
====
