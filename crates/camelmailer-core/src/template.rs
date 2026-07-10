//! A deliberately small Mustache-subset renderer for message templates.
//!
//! Supported:
//! - `{{ name }}`   — HTML-escaped interpolation
//! - `{{{ name }}}` / `{{& name }}` — raw (unescaped) interpolation
//! - `{{# section }} … {{/ section }}` — sections (truthy value / iterate array)
//! - `{{^ section }} … {{/ section }}` — inverted sections
//! - `{{! comment }}` — comments
//! - dotted paths (`a.b.c`) and `.` (the current item) over a context stack
//!
//! Deliberately NOT supported: partials (`{{> …}}`), lambdas, set-delimiter,
//! any IO. The `model` is untrusted end-user data, so output is HTML-escaped
//! by default and both section nesting depth and total output size are capped
//! to bound the work an attacker-supplied template + model can cause.

use serde_json::Value;

/// Maximum section-nesting depth allowed in a template.
const MAX_DEPTH: usize = 32;
/// Maximum rendered output size (bytes) before rendering is aborted.
const MAX_OUTPUT: usize = 512 * 1024;

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum RenderError {
    #[error("template has an unclosed tag")]
    UnclosedTag,
    #[error("template has an unclosed section")]
    UnclosedSection,
    #[error("unexpected closing tag {0:?}")]
    UnexpectedClose(String),
    #[error("mismatched closing tag: expected {expected:?}, found {found:?}")]
    MismatchedClose { expected: String, found: String },
    #[error("template nests sections deeper than the limit of {MAX_DEPTH}")]
    TooDeep,
    #[error("rendered output exceeds the limit of {MAX_OUTPUT} bytes")]
    OutputTooLarge,
}

#[derive(Debug, PartialEq)]
enum Node {
    Text(String),
    Var {
        path: String,
        escaped: bool,
    },
    Section {
        path: String,
        inverted: bool,
        children: Vec<Node>,
    },
}

struct Frame {
    path: String,
    inverted: bool,
    children: Vec<Node>,
}

fn sink<'a>(root: &'a mut Vec<Node>, stack: &'a mut [Frame]) -> &'a mut Vec<Node> {
    match stack.last_mut() {
        Some(frame) => &mut frame.children,
        None => root,
    }
}

fn parse(template: &str) -> Result<Vec<Node>, RenderError> {
    let mut root: Vec<Node> = Vec::new();
    let mut stack: Vec<Frame> = Vec::new();
    let mut rest = template;

    loop {
        let Some(pos) = rest.find("{{") else {
            if !rest.is_empty() {
                sink(&mut root, &mut stack).push(Node::Text(rest.to_string()));
            }
            break;
        };
        if pos > 0 {
            sink(&mut root, &mut stack).push(Node::Text(rest[..pos].to_string()));
        }
        let after = &rest[pos + 2..];

        // Triple-mustache raw interpolation: {{{ path }}}
        if let Some(triple) = after.strip_prefix('{') {
            let end = triple.find("}}}").ok_or(RenderError::UnclosedTag)?;
            let path = triple[..end].trim().to_string();
            sink(&mut root, &mut stack).push(Node::Var {
                path,
                escaped: false,
            });
            rest = &triple[end + 3..];
            continue;
        }

        let end = after.find("}}").ok_or(RenderError::UnclosedTag)?;
        let tag = after[..end].trim();
        rest = &after[end + 2..];

        match tag.chars().next() {
            None => {}      // `{{}}` — ignore
            Some('!') => {} // comment
            Some('#') | Some('^') => {
                if stack.len() >= MAX_DEPTH {
                    return Err(RenderError::TooDeep);
                }
                stack.push(Frame {
                    path: tag[1..].trim().to_string(),
                    inverted: tag.starts_with('^'),
                    children: Vec::new(),
                });
            }
            Some('/') => {
                let path = tag[1..].trim().to_string();
                let frame = stack
                    .pop()
                    .ok_or_else(|| RenderError::UnexpectedClose(path.clone()))?;
                if frame.path != path {
                    return Err(RenderError::MismatchedClose {
                        expected: frame.path,
                        found: path,
                    });
                }
                let node = Node::Section {
                    path: frame.path,
                    inverted: frame.inverted,
                    children: frame.children,
                };
                sink(&mut root, &mut stack).push(node);
            }
            Some('&') => {
                sink(&mut root, &mut stack).push(Node::Var {
                    path: tag[1..].trim().to_string(),
                    escaped: false,
                });
            }
            _ => {
                sink(&mut root, &mut stack).push(Node::Var {
                    path: tag.to_string(),
                    escaped: true,
                });
            }
        }
    }

    if !stack.is_empty() {
        return Err(RenderError::UnclosedSection);
    }
    Ok(root)
}

fn is_truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::String(s) => !s.is_empty(),
        Value::Array(a) => !a.is_empty(),
        Value::Number(_) | Value::Object(_) => true,
    }
}

/// Scalar rendering of a value; non-scalars render as empty.
fn scalar(value: &Value) -> String {
    match value {
        Value::String(s) => s.clone(),
        Value::Number(n) => n.to_string(),
        Value::Bool(b) => b.to_string(),
        _ => String::new(),
    }
}

fn html_escape(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    for ch in input.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            _ => out.push(ch),
        }
    }
    out
}

/// Resolve a dotted path against the context stack (innermost scope first).
fn lookup<'a>(stack: &[&'a Value], path: &str) -> Option<&'a Value> {
    if path == "." {
        return stack.last().copied();
    }
    let mut segments = path.split('.');
    let first = segments.next()?;
    let mut value: Option<&Value> = None;
    for scope in stack.iter().rev() {
        if let Value::Object(map) = scope {
            if let Some(found) = map.get(first) {
                value = Some(found);
                break;
            }
        }
    }
    let mut value = value?;
    for segment in segments {
        match value {
            Value::Object(map) => value = map.get(segment)?,
            _ => return None,
        }
    }
    Some(value)
}

fn push_capped(out: &mut String, text: &str) -> Result<(), RenderError> {
    if out.len() + text.len() > MAX_OUTPUT {
        return Err(RenderError::OutputTooLarge);
    }
    out.push_str(text);
    Ok(())
}

fn render_nodes(
    nodes: &[Node],
    stack: &mut Vec<&Value>,
    out: &mut String,
) -> Result<(), RenderError> {
    for node in nodes {
        match node {
            Node::Text(text) => push_capped(out, text)?,
            Node::Var { path, escaped } => {
                if let Some(value) = lookup(stack, path) {
                    let rendered = scalar(value);
                    if *escaped {
                        push_capped(out, &html_escape(&rendered))?;
                    } else {
                        push_capped(out, &rendered)?;
                    }
                }
            }
            Node::Section {
                path,
                inverted,
                children,
            } => {
                let value = lookup(stack, path);
                let truthy = value.is_some_and(is_truthy);
                if *inverted {
                    if !truthy {
                        render_nodes(children, stack, out)?;
                    }
                } else if let Some(value) = value {
                    match value {
                        Value::Array(items) => {
                            for item in items {
                                stack.push(item);
                                let result = render_nodes(children, stack, out);
                                stack.pop();
                                result?;
                            }
                        }
                        _ if is_truthy(value) => {
                            stack.push(value);
                            let result = render_nodes(children, stack, out);
                            stack.pop();
                            result?;
                        }
                        _ => {}
                    }
                }
            }
        }
    }
    Ok(())
}

/// Render `template` against `model`, returning the produced string.
pub fn render(template: &str, model: &Value) -> Result<String, RenderError> {
    let nodes = parse(template)?;
    let mut out = String::new();
    let mut stack: Vec<&Value> = vec![model];
    render_nodes(&nodes, &mut stack, &mut out)?;
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn interpolates_and_html_escapes_by_default() {
        let model = json!({ "name": "<script>alert('x')</script>" });
        let out = render("Hi {{ name }}", &model).unwrap();
        assert_eq!(out, "Hi &lt;script&gt;alert(&#39;x&#39;)&lt;/script&gt;");
    }

    #[test]
    fn triple_and_ampersand_pass_through_raw() {
        let model = json!({ "html": "<b>bold</b>" });
        assert_eq!(render("{{{ html }}}", &model).unwrap(), "<b>bold</b>");
        assert_eq!(render("{{& html }}", &model).unwrap(), "<b>bold</b>");
    }

    #[test]
    fn sections_iterate_arrays_and_use_dotted_paths() {
        let model = json!({ "items": [ { "n": "a" }, { "n": "b" } ] });
        let out = render("{{# items }}[{{ n }}]{{/ items }}", &model).unwrap();
        assert_eq!(out, "[a][b]");
    }

    #[test]
    fn truthy_section_and_inverted_section() {
        let present = json!({ "on": true, "list": [1] });
        assert_eq!(render("{{# on }}yes{{/ on }}", &present).unwrap(), "yes");
        let absent = json!({ "list": [] });
        assert_eq!(
            render("{{^ list }}empty{{/ list }}", &absent).unwrap(),
            "empty"
        );
        assert_eq!(
            render("{{# missing }}x{{/ missing }}", &absent).unwrap(),
            ""
        );
    }

    #[test]
    fn dotted_path_and_current_item() {
        let model = json!({ "user": { "name": "Ada" }, "tags": ["x", "y"] });
        assert_eq!(render("{{ user.name }}", &model).unwrap(), "Ada");
        assert_eq!(render("{{# tags }}{{.}}{{/ tags }}", &model).unwrap(), "xy");
    }

    #[test]
    fn missing_variable_renders_empty() {
        assert_eq!(render("a{{ nope }}b", &json!({})).unwrap(), "ab");
    }

    #[test]
    fn unclosed_tag_and_section_are_errors() {
        assert_eq!(render("{{ oops", &json!({})), Err(RenderError::UnclosedTag));
        assert_eq!(
            render("{{# s }}x", &json!({})),
            Err(RenderError::UnclosedSection)
        );
        assert!(matches!(
            render("{{# a }}x{{/ b }}", &json!({})),
            Err(RenderError::MismatchedClose { .. })
        ));
    }

    #[test]
    fn depth_cap_rejects_deeply_nested_sections() {
        let mut template = String::new();
        for _ in 0..(MAX_DEPTH + 1) {
            template.push_str("{{# s }}");
        }
        assert_eq!(parse(&template).err(), Some(RenderError::TooDeep));
    }

    #[test]
    fn output_size_cap_rejects_runaway_expansion() {
        // a small array whose body repeats a large chunk many times
        let big = "x".repeat(50_000);
        let model = json!({ "rows": [ {}, {}, {}, {}, {}, {}, {}, {}, {}, {}, {}, {} ] });
        let template = format!("{{{{# rows }}}}{big}{{{{/ rows }}}}");
        assert_eq!(render(&template, &model), Err(RenderError::OutputTooLarge));
    }
}
