---- MODULE MCHugeFront ----
EXTENDS Segments
\* The third segment's dictionary alone is larger than the budget of four
\* units, so no thread ever starts and every segment streams in turn.
SizesModel == <<2, 1, 6, 1>>
StreamedModel == <<1, 1, 5, 1>>
====
