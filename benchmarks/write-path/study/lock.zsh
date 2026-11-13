# Sourced by each remote script: one remote run uses the second machine at a time,
# the same folder lock as `remote_lock` in xtask/src/verify.rs.
base=${ZIPPER_CHECKOUT:?}
W=$base/target/wpath
mkdir -p $W
until mkdir $base/target/remote-run.lock 2>/dev/null; do
  find $base/target/remote-run.lock -maxdepth 0 -mmin +180 -exec rmdir {} \; 2>/dev/null
  sleep 5
done
trap 'rmdir $base/target/remote-run.lock' EXIT
