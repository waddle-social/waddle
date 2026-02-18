//! Unacknowledged Stanza Queue for XEP-0198 Stream Management
//!
//! This module provides a queue for storing outbound stanzas that haven't been
//! acknowledged by the client yet. When a stream is resumed, these stanzas
//! can be resent to ensure reliable delivery.

use std::collections::VecDeque;
use std::time::Instant;

/// An unacknowledged stanza waiting for client acknowledgment.
#[derive(Debug, Clone)]
pub struct UnackedStanza {
    /// The sequence number of this stanza (outbound count when sent)
    pub sequence: u32,
    /// The XML content of the stanza
    pub stanza_xml: String,
    /// When the stanza was sent
    pub sent_at: Instant,
}

impl UnackedStanza {
    /// Create a new unacked stanza.
    pub fn new(sequence: u32, stanza_xml: String) -> Self {
        Self {
            sequence,
            stanza_xml,
            sent_at: Instant::now(),
        }
    }

    /// Get the age of this stanza (time since it was sent).
    pub fn age(&self) -> std::time::Duration {
        self.sent_at.elapsed()
    }
}

/// Queue of unacknowledged outbound stanzas.
///
/// Maintains stanzas in FIFO order, removing them when acknowledged.
/// Has a maximum size to prevent unbounded memory growth.
#[derive(Debug)]
pub struct UnackedQueue {
    /// The queue of unacked stanzas
    stanzas: VecDeque<UnackedStanza>,
    /// Maximum number of stanzas to store
    max_size: usize,
}

impl UnackedQueue {
    /// Create a new unacked queue with the specified maximum size.
    pub fn new(max_size: usize) -> Self {
        Self {
            stanzas: VecDeque::with_capacity(max_size.min(1024)),
            max_size,
        }
    }

    /// Push a new stanza onto the queue.
    ///
    /// If the queue is at capacity, the oldest stanza is removed.
    pub fn push(&mut self, sequence: u32, stanza_xml: String) {
        // If at capacity, remove oldest
        if self.stanzas.len() >= self.max_size {
            self.stanzas.pop_front();
        }

        self.stanzas
            .push_back(UnackedStanza::new(sequence, stanza_xml));
    }

    /// Remove all stanzas with sequence <= h (they've been acknowledged).
    pub fn acknowledge(&mut self, h: u32) {
        // Remove stanzas from front while their sequence <= h
        while let Some(front) = self.stanzas.front() {
            if sequence_lte(front.sequence, h) {
                self.stanzas.pop_front();
            } else {
                break;
            }
        }
    }

    /// Get all stanzas with sequence > h that need to be resent.
    ///
    /// This is used during stream resumption when the client reports
    /// which stanza it last received.
    pub fn get_unacked_after(&self, h: u32) -> Vec<String> {
        self.stanzas
            .iter()
            .filter(|s| sequence_gt(s.sequence, h))
            .map(|s| s.stanza_xml.clone())
            .collect()
    }

    /// Get all stanzas in the queue (for detached session storage).
    pub fn get_all_unacked(&self) -> Vec<(u32, String)> {
        self.stanzas
            .iter()
            .map(|s| (s.sequence, s.stanza_xml.clone()))
            .collect()
    }

    /// Restore the queue from a list of (sequence, xml) pairs.
    ///
    /// This is used when resuming a stream from a detached session.
    pub fn restore(&mut self, stanzas: &[(u32, String)]) {
        self.stanzas.clear();
        for (seq, xml) in stanzas {
            if self.stanzas.len() < self.max_size {
                self.stanzas
                    .push_back(UnackedStanza::new(*seq, xml.clone()));
            }
        }
    }

    /// Get the number of stanzas in the queue.
    pub fn len(&self) -> usize {
        self.stanzas.len()
    }

    /// Check if the queue is empty.
    pub fn is_empty(&self) -> bool {
        self.stanzas.is_empty()
    }

    /// Clear the queue.
    pub fn clear(&mut self) {
        self.stanzas.clear();
    }

    /// Get the sequence number of the oldest stanza (if any).
    pub fn oldest_sequence(&self) -> Option<u32> {
        self.stanzas.front().map(|s| s.sequence)
    }

    /// Get the sequence number of the newest stanza (if any).
    pub fn newest_sequence(&self) -> Option<u32> {
        self.stanzas.back().map(|s| s.sequence)
    }
}

/// Check if sequence a <= b, handling wrap-around.
///
/// XEP-0198 specifies that sequence numbers wrap at 2^32.
/// Per RFC 1982-like comparison, we use a window of 2^31.
fn sequence_lte(a: u32, b: u32) -> bool {
    // If a == b, it's equal
    if a == b {
        return true;
    }

    // Otherwise, a < b if (b - a) mod 2^32 < 2^31
    let diff = b.wrapping_sub(a);
    diff < 0x8000_0000
}

/// Check if sequence a > b, handling wrap-around.
fn sequence_gt(a: u32, b: u32) -> bool {
    !sequence_lte(a, b)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_unacked_queue_basic() {
        let mut queue = UnackedQueue::new(10);
        assert!(queue.is_empty());

        queue.push(1, "<msg1/>".to_string());
        queue.push(2, "<msg2/>".to_string());
        queue.push(3, "<msg3/>".to_string());

        assert_eq!(queue.len(), 3);
        assert_eq!(queue.oldest_sequence(), Some(1));
        assert_eq!(queue.newest_sequence(), Some(3));
    }

    #[test]
    fn test_unacked_queue_acknowledge() {
        let mut queue = UnackedQueue::new(10);

        queue.push(1, "<msg1/>".to_string());
        queue.push(2, "<msg2/>".to_string());
        queue.push(3, "<msg3/>".to_string());
        queue.push(4, "<msg4/>".to_string());

        // Acknowledge up to 2
        queue.acknowledge(2);
        assert_eq!(queue.len(), 2);
        assert_eq!(queue.oldest_sequence(), Some(3));

        // Acknowledge up to 4
        queue.acknowledge(4);
        assert!(queue.is_empty());
    }

    #[test]
    fn test_unacked_queue_get_unacked_after() {
        let mut queue = UnackedQueue::new(10);

        queue.push(1, "<msg1/>".to_string());
        queue.push(2, "<msg2/>".to_string());
        queue.push(3, "<msg3/>".to_string());
        queue.push(4, "<msg4/>".to_string());

        // Get stanzas after h=2 (should be 3 and 4)
        let unacked = queue.get_unacked_after(2);
        assert_eq!(unacked.len(), 2);
        assert_eq!(unacked[0], "<msg3/>");
        assert_eq!(unacked[1], "<msg4/>");

        // Get stanzas after h=0 (all of them)
        let unacked = queue.get_unacked_after(0);
        assert_eq!(unacked.len(), 4);
    }

    #[test]
    fn test_unacked_queue_max_size() {
        let mut queue = UnackedQueue::new(3);

        queue.push(1, "<msg1/>".to_string());
        queue.push(2, "<msg2/>".to_string());
        queue.push(3, "<msg3/>".to_string());
        assert_eq!(queue.len(), 3);

        // Adding a 4th should remove the oldest
        queue.push(4, "<msg4/>".to_string());
        assert_eq!(queue.len(), 3);
        assert_eq!(queue.oldest_sequence(), Some(2));
    }

    #[test]
    fn test_unacked_queue_restore() {
        let mut queue = UnackedQueue::new(10);

        let stanzas = vec![
            (5, "<msg5/>".to_string()),
            (6, "<msg6/>".to_string()),
            (7, "<msg7/>".to_string()),
        ];

        queue.restore(&stanzas);
        assert_eq!(queue.len(), 3);
        assert_eq!(queue.oldest_sequence(), Some(5));
        assert_eq!(queue.newest_sequence(), Some(7));
    }

    #[test]
    fn test_sequence_comparison() {
        // Normal cases
        assert!(sequence_lte(1, 2));
        assert!(sequence_lte(1, 1));
        assert!(!sequence_lte(2, 1));

        assert!(sequence_gt(2, 1));
        assert!(!sequence_gt(1, 1));
        assert!(!sequence_gt(1, 2));

        // Wrap-around cases
        assert!(sequence_lte(0xFFFF_FFFF, 0)); // Wrapped
        assert!(sequence_gt(0, 0xFFFF_FFFF)); // 0 is after max when wrapped
    }

    #[test]
    fn test_unacked_queue_clear() {
        let mut queue = UnackedQueue::new(10);

        queue.push(1, "<msg1/>".to_string());
        queue.push(2, "<msg2/>".to_string());
        assert!(!queue.is_empty());

        queue.clear();
        assert!(queue.is_empty());
    }
}
