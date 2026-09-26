# Chat question score marker

## Goal

Show a separate question marker for room messages when the server's `is_question` probability is at least `0.75`.

## Plan

- Render the question signal independently from content-safety warnings.
- Preserve the existing safety-score marker threshold and behavior.
- Keep the XEP-0422 payload and server classifier unchanged.

## Acceptance

- A question probability below `0.75` does not show the question marker.
- A probability of exactly `0.75` or higher shows a blue question marker, including when there are no notable safety scores.
- Safety markers remain governed by their existing rules.
