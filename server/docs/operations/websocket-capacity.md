# WebSocket admission capacity

`WADDLE_MAX_WEBSOCKET_CONNECTIONS` sets the maximum number of pending and
live WebSocket connections held by one server process. It must be a positive
integer; invalid values fail startup. The default is 4096. This default is a
policy choice and is not based on workload measurements; operators should size
the limit for each host and workload. At capacity, additional valid WebSocket
upgrade requests receive HTTP 503. The slot remains held through the connection
task's normal shutdown and cleanup, and is released when that owner is dropped.

This limit is per process, not cluster-wide. It is a capacity reservation for
the current WebSocket loop; it does not by itself prove independent I/O task
cleanup or fence legacy writes when a clustered identity changes.
