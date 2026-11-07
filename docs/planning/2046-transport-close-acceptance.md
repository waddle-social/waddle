# Write acceptance after a failed WebSocket send

Status: focused production fix; the broader #2046 output-custody and binding-cutover gates remain open.

An outbound direct frame can be recorded in the live resumable SM queue before the socket write. A transport failure then used to acknowledge its producer when that queue was gap-free. This queue is process memory: ordinary superseded cleanup can return without persisting it, so it is not proof of writer acceptance or durable replay ownership.

For `OutboundWriteAcceptance`, successful socket send remains the positive acceptance point. A transport error, readiness failure, flush error, or revoked node authority closes the token without acknowledging it. The local room-effect outbox retains its existing leased retry opportunity; remote write-accepted relay reports a closed or pending token as retryable. This is at-least-once: if a frame started before flush failed and the old SM queue later persists, producer retry can duplicate delivery. The change does not claim peer receipt, recover queued tail frames, or select the ordinary local-binding fence.

Verification covers countable resumable frames failing before `start_send` and after `start_send` during flush, their unchanged SM queue inventory, a closed acceptance token, and the existing successful, replay-gap, and authority-revoked paths. The full #2046 release matrix and disabled GitHub workflows remain separate gates.
