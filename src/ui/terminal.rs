use avt::{Color, Pen, Vt};
use leptos::prelude::*;

/// Upper bound on terminal grid dimensions. The pane width/height come from the
/// remote (a possibly hostile server/bridge) and feed `Vt::new`, which eagerly
/// allocates the whole `cols * rows` cell grid. Without a cap, a crafted pane
/// size (e.g. 65535x65535) forces a multi-gigabyte allocation that crashes the
/// tab. No real terminal is this large; clamp well above any legitimate size.
const MAX_TERM_DIM: usize = 1000;

/// Terminal view: feeds captured pane text (with ANSI escapes) into an
/// `avt::Vt` sized to the pane, then renders the visible screen as
/// run-merged `<span>`s with gruvbox ANSI color classes. All content goes
/// through Leptos text nodes — never inner_html.
#[component]
pub fn TerminalView(
    /// Raw `capture-pane -e -p` output (reactive so scroll position survives
    /// poll updates).
    #[prop(into)]
    text: Signal<String>,
    width: u16,
    height: u16,
) -> impl IntoView {
    let cols = (width.max(1) as usize).min(MAX_TERM_DIM);
    let rows = (height.max(1) as usize).min(MAX_TERM_DIM);
    view! {
        <div class="terminal-scroll">
            <pre class="terminal">{move || render_screen(&text.get(), cols, rows)}</pre>
        </div>
    }
}

fn render_screen(text: &str, cols: usize, rows: usize) -> impl IntoView {
    let mut vt = Vt::new(cols, rows);
    vt.feed_str(text);
    vt.view()
        .map(|line| {
            let spans = line
                .chunks(|a, b| a.pen() != b.pen())
                .map(|cells| {
                    let run: String = cells.iter().map(|c| c.char()).collect();
                    let (class, style) = pen_class_style(cells[0].pen());
                    view! { <span class=class style=style>{run}</span> }
                })
                .collect::<Vec<_>>();
            view! { <div class="tline">{spans}</div> }
        })
        .collect::<Vec<_>>()
}

/// Maps a pen to gruvbox classes (`t0..t15` fg, `b0..b15` bg, attribute
/// classes) plus an inline style for indexed>15 / RGB colors.
fn pen_class_style(pen: &Pen) -> (String, String) {
    let mut fg = pen.foreground();
    let mut bg = pen.background();
    if pen.is_inverse() {
        // Terminal defaults: fg = ansi 15, bg = ansi 0.
        let f = fg.unwrap_or(Color::Indexed(15));
        let b = bg.unwrap_or(Color::Indexed(0));
        fg = Some(b);
        bg = Some(f);
    }
    // Classic bold-implies-bright for the base 8 colors.
    if pen.is_bold() {
        if let Some(Color::Indexed(i)) = fg {
            if i < 8 {
                fg = Some(Color::Indexed(i + 8));
            }
        }
    }

    let mut class = String::new();
    let mut style = String::new();
    match fg {
        None => {}
        Some(Color::Indexed(i)) if i < 16 => class.push_str(&format!("t{i} ")),
        Some(c) => style.push_str(&format!("color:{};", css_color(c))),
    }
    match bg {
        None => {}
        Some(Color::Indexed(i)) if i < 16 => class.push_str(&format!("b{i} ")),
        Some(c) => style.push_str(&format!("background-color:{};", css_color(c))),
    }
    if pen.is_bold() {
        class.push_str("bold ");
    }
    if pen.is_faint() {
        class.push_str("faint ");
    }
    if pen.is_italic() {
        class.push_str("italic ");
    }
    if pen.is_underline() {
        class.push_str("underline ");
    }
    if pen.is_strikethrough() {
        class.push_str("strike ");
    }
    (class.trim_end().to_string(), style)
}

fn css_color(c: Color) -> String {
    match c {
        Color::Indexed(i) => {
            let (r, g, b) = indexed_rgb(i);
            format!("#{r:02x}{g:02x}{b:02x}")
        }
        Color::RGB(rgb) => format!("#{:02x}{:02x}{:02x}", rgb.r, rgb.g, rgb.b),
    }
}

/// xterm-256 palette for indices 16..=255 (0..16 use CSS classes).
fn indexed_rgb(i: u8) -> (u8, u8, u8) {
    if i < 16 {
        // Unreachable via pen_class_style; approximate with gruvbox-ish gray.
        return (0xa8, 0x99, 0x84);
    }
    if i < 232 {
        let i = i - 16;
        let level = |v: u8| if v == 0 { 0 } else { 55 + 40 * v };
        (level(i / 36), level((i / 6) % 6), level(i % 6))
    } else {
        let v = 8 + 10 * (i - 232);
        (v, v, v)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn first_pen_of(input: &str) -> Pen {
        let mut vt = Vt::new(10, 2);
        vt.feed_str(input);
        let line = vt.view().next().unwrap().clone();
        *line.cells()[0].pen()
    }

    #[test]
    fn indexed_rgb_cube_and_gray() {
        assert_eq!(indexed_rgb(16), (0, 0, 0));
        assert_eq!(indexed_rgb(231), (255, 255, 255));
        assert_eq!(indexed_rgb(232), (8, 8, 8));
        assert_eq!(indexed_rgb(255), (238, 238, 238));
    }

    #[test]
    fn sgr_red_maps_to_class() {
        let pen = first_pen_of("\x1b[31mx");
        let (class, style) = pen_class_style(&pen);
        assert_eq!(class, "t1");
        assert_eq!(style, "");
    }

    #[test]
    fn bold_base_color_brightens() {
        let pen = first_pen_of("\x1b[1;31mx");
        let (class, _) = pen_class_style(&pen);
        assert_eq!(class, "t9 bold");
    }

    #[test]
    fn indexed_256_uses_inline_style() {
        let pen = first_pen_of("\x1b[38;5;208mx");
        let (class, style) = pen_class_style(&pen);
        assert_eq!(class, "");
        assert_eq!(style, "color:#ff8700;");
    }

    #[test]
    fn inverse_swaps_defaults() {
        let pen = first_pen_of("\x1b[7mx");
        let (class, _) = pen_class_style(&pen);
        assert_eq!(class, "t0 b15");
    }
}
