const LOG_PREVIEW_CHARS: usize = 120;

pub fn preview(text: &str) -> String {
    let mut s: String = text.chars().take(LOG_PREVIEW_CHARS).collect();
    if text.chars().count() > LOG_PREVIEW_CHARS {
        s.push('…');
    }

    s
}
