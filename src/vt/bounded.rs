//! Bounded queues for escape-sequence side effects driven by untrusted PTY output.

/// Maximum queued OSC 52 sequences awaiting forwarding to the host terminal / clipboard.
pub const MAX_PENDING_OSC52: usize = 64;
/// Maximum total bytes held in one pending OSC 52 queue.
pub const MAX_PENDING_OSC52_BYTES: usize = 4 * 1024 * 1024;

/// Pushes `item`, dropping the oldest entries until both the count and byte caps hold.
/// An item larger than `max_bytes` on its own is discarded.
pub fn push_bounded(queue: &mut Vec<String>, item: String, max_items: usize, max_bytes: usize) {
    if item.len() > max_bytes {
        return;
    }
    let mut bytes: usize = queue.iter().map(String::len).sum::<usize>() + item.len();
    let mut drop_n = 0;
    while drop_n < queue.len() && (queue.len() - drop_n + 1 > max_items || bytes > max_bytes) {
        bytes -= queue[drop_n].len();
        drop_n += 1;
    }
    queue.drain(..drop_n);
    queue.push(item);
}

/// [`push_bounded`] with the OSC 52 default caps.
pub fn push_osc52(queue: &mut Vec<String>, item: String) {
    push_bounded(queue, item, MAX_PENDING_OSC52, MAX_PENDING_OSC52_BYTES);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn caps_by_count_dropping_oldest() {
        let mut q = Vec::new();
        for i in 0..10 {
            push_bounded(&mut q, format!("{i}"), 3, 1024);
        }
        assert_eq!(q, vec!["7", "8", "9"]);
    }

    #[test]
    fn caps_by_bytes_dropping_oldest() {
        let mut q = Vec::new();
        for c in ['a', 'b', 'c'] {
            push_bounded(&mut q, c.to_string().repeat(40), 100, 100);
        }
        assert_eq!(q.len(), 2);
        assert!(q[0].starts_with('b') && q[1].starts_with('c'));
        push_bounded(&mut q, "x".repeat(101), 100, 100);
        assert_eq!(q.len(), 2, "oversized item is discarded, queue untouched");
    }
}
