---- MODULE MCSegments ----
EXTENDS Segments
\* Six segments against a budget of four units and a window of two
\* decoders. The first streams while one thread runs, so the window stops a
\* third decoder. Threads leave room for the largest front dictionary, two
\* units, which stops a fourth unit of threads the budget alone would allow.
\* The fifth needs six units in a thread, more than the whole budget, so it
\* can only stream as the front.
SizesModel == <<3, 1, 1, 1, 6, 1>>
StreamedModel == <<2, 1, 1, 1, 2, 1>>
====
