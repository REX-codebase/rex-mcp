#!/bin/sh
# score.sh TASK WORKDIR: run the task's hidden test inside WORKDIR (a copy of
# tasks/TASK/repo after an agent worked in it). Exit 0 = pass.
set -eu
here=$(cd "$(dirname "$0")" && pwd)
task=$1; work=$2
cp "$here/tasks/$task/hidden_test.py" "$work/hidden_test.py"
cd "$work" && timeout 60 python3 hidden_test.py >/dev/null 2>&1
