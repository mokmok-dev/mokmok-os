#!/bin/sh
# Print the first ten Fibonacci numbers, one per line.
a=1
b=1
i=0
while [ "$i" -lt 10 ]; do
    printf '%s\n' "$a"
    tmp=$((a + b))
    a=$b
    b=$tmp
    i=$((i + 1))
done
