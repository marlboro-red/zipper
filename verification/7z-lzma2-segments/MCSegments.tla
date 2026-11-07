---- MODULE MCSegments ----
EXTENDS Segments
\* Five segments against a budget of three units and a window of two
\* decoders. The second segment needs four units in a thread, more than the
\* budget, so it can only stream as the front. Streaming any segment takes
\* one unit. The window stops a third decoder, and the budget stops the last
\* segment while two small ones wait to be taken.
SizesModel == <<2, 4, 1, 1, 2>>
StreamedModel == <<1, 1, 1, 1, 1>>
====
