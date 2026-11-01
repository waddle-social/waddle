# systemd v262 regression transcript

`systemd-v262-failed-readback.txt` derives from the saved read-only
`benign-resource-observation/exact-fields-readback.txt` from the single failed
2026-10-31 observation. Its original SHA-256 is `a85635045768372ba7bee33a3a116b3804284cb4b884f5ff313975104c8ad50f`.

The nonce unit, invocation and ExecMainPID are explicit fixture identities:
unit `waddle-resource-1000-00000000000000000000000000000001.service`, invocation
`11111111111111111111111111111111`, PID `4242`. The fragment basename was changed
together with the unit. Every other field/value and the missing deprecated
CPUAccounting field are preserved. These identities must never authorize a real
manager operation, cleanup or recovery. The failed timeout state is intentional;
this fixture proves parser compatibility, not a successful observation.

The original record reports systemd v262 omitting deprecated CPUAccounting from
GetAll. The fixture has no runtime dependency on the external observation folder.
The already-completed manager cleanup is outside these tests and is not repeated.
