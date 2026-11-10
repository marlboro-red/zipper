---- MODULE MCBlocks ----
EXTENDS Blocks
\* Five candidates in every layout of true block starts and false magics,
\* with every set of corrupt blocks. `blocks.cfg` runs two workers within a
\* budget of two blocks, so a thread runs behind the front and a false cut
\* finds a result to drop. `no-budget.cfg` gives three workers no budget,
\* so only the one block that may always start is ever queued.
====
