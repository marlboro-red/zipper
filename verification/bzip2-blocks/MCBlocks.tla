---- MODULE MCBlocks ----
EXTENDS Blocks
\* Five candidates in every layout of true block starts and false magics,
\* with every set of corrupt blocks. `blocks.cfg` runs two workers within a
\* budget of three blocks, so the workers are the limit, a thread runs
\* behind the front and a false cut finds a result to drop. `no-budget.cfg`
\* gives three workers no budget, so the budget is the limit and only the
\* one block that may always start is ever queued.
====
