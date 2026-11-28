use super::{Event, PasteQueue, VecDeque, text_mime};

#[test]
fn paste_results_keep_request_order_after_failure() {
    let mut queue = PasteQueue::default();
    let mut events = VecDeque::new();
    let [first, failed, last] = [0, 1, 2].map(|generation| {
        let sequence = queue.next_sequence();
        queue.push(generation, ());
        sequence
    });

    queue.finish(last, Some("last".into()), &mut events);
    assert!(events.is_empty());
    queue.finish(first, Some("first".into()), &mut events);
    assert!(matches!(
        events.pop_front(),
        Some(Event::ClipboardText { text, generation: 0 }) if text == "first"
    ));
    assert!(events.is_empty());
    queue.finish(failed, None, &mut events);
    assert!(matches!(
        events.pop_front(),
        Some(Event::ClipboardText { text, generation: 2 }) if text == "last"
    ));
    assert!(events.is_empty());
}

#[test]
fn empty_mime_list_is_unknown_not_unsupported() {
    assert_eq!(text_mime(&[]), Some("text/plain;charset=utf-8"));
    assert_eq!(text_mime(&["image/png".into()]), None);
}
