//! Readable text from HTML, in one linear pass without a DOM. Scripts, styles
//! and other non-text elements are dropped; block elements become line
//! breaks; entities are decoded.

/// Elements whose content is never text for a reader.
const SKIPPED: [&str; 8] = [
    "script", "style", "noscript", "template", "svg", "iframe", "canvas", "object",
];
/// Elements that start a new line.
const BLOCKS: [&str; 32] = [
    "address",
    "article",
    "aside",
    "blockquote",
    "br",
    "dd",
    "details",
    "div",
    "dl",
    "dt",
    "fieldset",
    "figcaption",
    "figure",
    "footer",
    "form",
    "h1",
    "h2",
    "h3",
    "h4",
    "h5",
    "h6",
    "header",
    "hr",
    "main",
    "nav",
    "ol",
    "p",
    "pre",
    "section",
    "table",
    "tr",
    "ul",
];

/// The page title (if any) and its readable text.
pub fn extract(html: &str) -> (Option<String>, String) {
    let mut text = String::with_capacity(html.len() / 2);
    let mut title = None;
    let mut rest = html;
    while let Some(start) = rest.find('<') {
        text.push_str(&decode(&rest[..start]));
        rest = &rest[start..];
        if let Some(comment) = rest.strip_prefix("<!--") {
            rest = comment.find("-->").map_or("", |end| &comment[end + 3..]);
            continue;
        }
        let Some(end) = tag_end(rest) else {
            // An unterminated tag ends the document.
            rest = "";
            break;
        };
        let tag = &rest[1..end];
        rest = &rest[end + 1..];
        let closing = tag.starts_with('/');
        let name = tag
            .trim_start_matches('/')
            .split(|c: char| c.is_ascii_whitespace() || c == '/' || c == '>')
            .next()
            .unwrap_or_default()
            .to_ascii_lowercase();
        if closing {
            if BLOCKS.contains(&name.as_str()) {
                text.push('\n');
            }
            continue;
        }
        if SKIPPED.contains(&name.as_str()) && !tag.ends_with('/') {
            rest = skip_element(rest, &name);
        } else if name == "title" {
            let (inner, after) = element_text(rest, "title");
            if title.is_none() {
                let heading = collapse(&decode(inner));
                title = (!heading.is_empty()).then_some(heading);
            }
            rest = after;
        } else if name == "li" {
            text.push_str("\n- ");
        } else if name == "td" || name == "th" {
            text.push('\t');
        } else if BLOCKS.contains(&name.as_str()) {
            text.push('\n');
        }
    }
    text.push_str(&decode(rest));
    (title, tidy(&text))
}

/// Index of the `>` that ends the tag at the start of `input`, ignoring `>`
/// inside quoted attribute values.
fn tag_end(input: &str) -> Option<usize> {
    let mut quote = None;
    for (index, character) in input.char_indices().skip(1) {
        match (quote, character) {
            (None, '"' | '\'') => quote = Some(character),
            (Some(open), _) if character == open => quote = None,
            (None, '>') => return Some(index),
            _ => {}
        }
    }
    None
}

/// The rest of the document after `</name>`, or nothing if it never closes.
fn skip_element<'a>(input: &'a str, name: &str) -> &'a str {
    element_text(input, name).1
}

/// The raw content before `</name>` and the rest after it.
fn element_text<'a>(input: &'a str, name: &str) -> (&'a str, &'a str) {
    let lower = input.to_ascii_lowercase();
    let closing = format!("</{name}");
    match lower.find(&closing) {
        Some(start) => {
            let after = &input[start..];
            let end = after.find('>').map_or(after.len(), |index| index + 1);
            (&input[..start], &after[end..])
        }
        None => (input, ""),
    }
}

fn decode(text: &str) -> String {
    let mut output = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find('&') {
        output.push_str(&rest[..start]);
        rest = &rest[start..];
        let end = rest[1..]
            .char_indices()
            .take(32)
            .find(|&(_, character)| character == ';')
            .map(|(index, _)| index + 1);
        let decoded = end.and_then(|end| entity(&rest[1..end]).map(|value| (value, end)));
        match decoded {
            Some((value, end)) => {
                output.push(value);
                rest = &rest[end + 1..];
            }
            None => {
                output.push('&');
                rest = &rest[1..];
            }
        }
    }
    output.push_str(rest);
    output
}

fn entity(name: &str) -> Option<char> {
    if let Some(number) = name.strip_prefix('#') {
        let code = match number.strip_prefix(['x', 'X']) {
            Some(hex) => u32::from_str_radix(hex, 16).ok()?,
            None => number.parse().ok()?,
        };
        return char::from_u32(code).filter(|character| *character != '\0');
    }
    Some(match name {
        "amp" => '&',
        "lt" => '<',
        "gt" => '>',
        "quot" => '"',
        "apos" => '\'',
        "nbsp" => ' ',
        "ndash" => '–',
        "mdash" => '—',
        "hellip" => '…',
        "lsquo" => '‘',
        "rsquo" => '’',
        "ldquo" => '“',
        "rdquo" => '”',
        "laquo" => '«',
        "raquo" => '»',
        "middot" => '·',
        "bull" => '•',
        "copy" => '©',
        "reg" => '®',
        "trade" => '™',
        "deg" => '°',
        "times" => '×',
        "euro" => '€',
        _ => return None,
    })
}

fn collapse(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Collapse spaces within lines and blank lines between paragraphs.
fn tidy(text: &str) -> String {
    let mut output = String::with_capacity(text.len());
    let mut blank = false;
    for line in text.lines() {
        let line = collapse(line);
        if line.is_empty() || line == "-" {
            blank = !output.is_empty();
            continue;
        }
        if blank {
            output.push_str("\n\n");
        } else if !output.is_empty() {
            output.push('\n');
        }
        blank = false;
        output.push_str(&line);
    }
    output
}

#[cfg(test)]
mod tests {
    use super::extract;

    #[test]
    fn readable_text_drops_code_and_keeps_structure() {
        let (title, text) = extract(
            "<!doctype html><html><head><title> Ditto &amp; Co </title>\
             <style>p { color: red }</style><script>var x = '<p>';</script></head>\
             <body><h1>Hello</h1><p>First&nbsp;line with <b>bold</b> &lt;tag&gt;.</p>\
             <!-- hidden <p>comment</p> --><ul><li>one</li><li>two &#x1F600;</li></ul>\
             <div data-x=\"a > b\">Quoted attribute</div><table><tr><td>a</td><td>b</td></tr></table>\
             <p>Unknown &bogus; and &#0; stay</p><SCRIPT>alert(1)</SCRIPT>tail",
        );
        assert_eq!(title.as_deref(), Some("Ditto & Co"));
        assert_eq!(
            text,
            "Hello\n\nFirst line with bold <tag>.\n\n- one\n- two 😀\n\nQuoted attribute\n\na b\n\nUnknown &bogus; and &#0; stay\ntail"
        );
    }

    #[test]
    fn unterminated_and_empty_documents_are_safe() {
        assert_eq!(extract(""), (None, String::new()));
        assert_eq!(extract("plain text"), (None, "plain text".into()));
        assert_eq!(extract("before <p unterminated"), (None, "before".into()));
        assert_eq!(extract("<script>never closed").1, "");
        assert_eq!(extract("<title></title>body"), (None, "body".into()));
    }
}
