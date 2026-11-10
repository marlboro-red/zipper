---- MODULE MCWindows ----
EXTENDS FolderWriters
\* As MCUnix, with one lane per writer, as on Windows.
FilesModel == <<2, 2>>
LanesModel == 2
====
