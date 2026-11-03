---- MODULE MCSegments ----
EXTENDS Segments
\* Five segments of 1, 1, 1, 2 and 2 units, a window of two decoders and a
\* budget of three units. The window stops a third small segment from
\* starting, and the budget stops a large one while only one decoder runs.
SizesModel == <<1, 1, 1, 2, 2>>
====
