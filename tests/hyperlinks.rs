//! Why kobold's transcript does not use OSC 8 hyperlinks.
//!
//! Kept as a test rather than a comment because it is the kind of constraint
//! that gets "fixed" by someone who assumes it was an oversight. If ratatui
//! ever gains hyperlink support this test fails, which is the signal to build
//! the feature.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Paragraph, Widget};

#[test]
fn ratatui_cannot_carry_escape_sequences_through_a_span() {
    let link = kobold::osc::link("https://example.com", "docs");
    let area = Rect::new(0, 0, 60, 1);
    let mut buf = Buffer::empty(area);
    Paragraph::new(Line::from(Span::raw(link))).render(area, &mut buf);

    let rendered: String = (0..area.width)
        .map(|x| buf[(x, 0)].symbol())
        .collect::<String>()
        .trim_end()
        .to_owned();

    // The buffer stores one grapheme per cell, so the ESC bytes are dropped and
    // the rest of the sequence becomes visible text.
    assert!(
        rendered.contains("]8;;https://example.com"),
        "got {rendered:?}"
    );
    assert_ne!(
        rendered, "docs",
        "if this passes, ratatui learned about hyperlinks"
    );
}
