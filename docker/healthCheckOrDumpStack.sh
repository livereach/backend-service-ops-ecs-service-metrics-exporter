#!/bin/bash

curl -sf http://127.0.0.1:9102/health > /dev/null

if [ $? -ne 0 ]
then
  echo "Health check failed" > /proc/1/fd/2
  exit 1
fi

exit 0
