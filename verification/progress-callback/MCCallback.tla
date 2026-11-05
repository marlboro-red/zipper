---- MODULE MCCallback ----
EXTENDS ProgressCallback
\* Two LZMA entries that each reserve more than half the budget, largest
\* first, then one that reserves a little and one that reserves none.
NeedModel == <<3, 3, 1, 0>>
====
