---- MODULE MCWorkers ----
EXTENDS FolderWorkers
\* Four folders, largest first as `read_archive` sorts them. The first asks
\* for more than the whole budget, as a solid folder with a huge dictionary
\* does, so it reserves the whole budget.
NeedModel == <<6, 3, 2, 1>>
====
