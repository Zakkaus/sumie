use sumie_vt::SearchMatch;

/// Project the union of matching intervals, not each individual match. Starts and ends are
/// both monotonic. At any cell only the last hit starting before it can extend coverage.
/// Even when a long query overlaps itself in every column, each visible cell is emitted once.
pub(super) fn visible_spans(
    spans: &mut Vec<(u16, u16, u16, bool)>,
    matches: &[SearchMatch],
    current: Option<SearchMatch>,
    top: u32,
    rows: u16,
    cols: u16,
) {
    spans.clear();
    for y in 0..rows {
        let row = top + u32::from(y);
        let selected = current.and_then(|m| columns(m, row, cols));
        let mut col = 0;
        while col < cols {
            let after = matches.partition_point(|m| m.start <= (row, col));
            if let Some(last) = after.checked_sub(1).and_then(|i| matches.get(i))
                && last.end >= (row, col)
            {
                let end = if last.end.0 == row {
                    last.end.1
                } else {
                    cols - 1
                };
                let (end, active) = match selected {
                    Some((left, _)) if col < left => (end.min(left - 1), false),
                    Some((_, right)) if col <= right => (end.min(right), true),
                    _ => (end, false),
                };
                if let Some((prev_row, _, prev_end, prev_active)) = spans.last_mut()
                    && *prev_row == y
                    && *prev_end + 1 == col
                    && *prev_active == active
                {
                    *prev_end = end;
                } else {
                    spans.push((y, col, end, active));
                }
                col = end + 1;
            } else if let Some(next) = matches.get(after).filter(|m| m.start.0 == row) {
                col = next.start.1;
            } else {
                break;
            }
        }
    }
}

fn columns(m: SearchMatch, row: u32, cols: u16) -> Option<(u16, u16)> {
    (m.start.0 <= row && row <= m.end.0).then(|| {
        (
            if m.start.0 == row { m.start.1 } else { 0 },
            if m.end.0 == row { m.end.1 } else { cols - 1 },
        )
    })
}
