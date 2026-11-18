use crate::SqlPart;

const RESERVED: [&str; 4] = ["where", "order_by", "limit", "offset"];

pub(crate) fn argument_name_valid(name: &str) -> bool {
    let mut chars = name.chars();
    chars
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
        && !name.starts_with("__")
        && !RESERVED.contains(&name)
}

pub(crate) fn check_arguments(arguments: &[String]) -> Result<(), String> {
    if let Some(name) = arguments.iter().find(|a| !argument_name_valid(a)) {
        return Err(format!(
            "argument name `{name}` must be a GraphQL name other than {}",
            RESERVED.join(", ")
        ));
    }
    if arguments
        .iter()
        .collect::<std::collections::BTreeSet<_>>()
        .len()
        != arguments.len()
    {
        return Err("argument names must be distinct".into());
    }
    Ok(())
}

pub(crate) fn merged(parts: &[SqlPart]) -> Vec<SqlPart> {
    let mut out: Vec<SqlPart> = Vec::new();
    for part in parts {
        match (out.last_mut(), part) {
            (_, SqlPart::Text(text)) if text.is_empty() => {}
            (Some(SqlPart::Text(last)), SqlPart::Text(text)) => last.push_str(text),
            _ => out.push(part.clone()),
        }
    }
    out
}

pub(crate) fn parse(sql: &str, arguments: &[String]) -> Result<Vec<SqlPart>, String> {
    check_arguments(arguments)?;
    if sql.trim().is_empty() {
        return Err("the statement is empty".into());
    }
    let mut parts = Vec::new();
    let mut text = String::new();
    let mut chars = sql.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\'' | '"' => {
                text.push(c);
                loop {
                    let Some(d) = chars.next() else {
                        return Err("the statement has an unterminated quote".into());
                    };
                    if d == '\\' {
                        return Err("backslashes are not allowed in the statement".into());
                    }
                    if d == '{' && chars.peek() == Some(&'{') {
                        return Err("`{{` is not allowed inside quoted text".into());
                    }
                    text.push(d);
                    if d == c {
                        if chars.peek() == Some(&c) {
                            text.push(chars.next().unwrap());
                        } else {
                            break;
                        }
                    }
                }
            }
            '{' if chars.peek() == Some(&'{') => {
                chars.next();
                let mut name = String::new();
                loop {
                    match chars.next() {
                        Some('}') if chars.peek() == Some(&'}') => {
                            chars.next();
                            break;
                        }
                        Some(d) => name.push(d),
                        None => return Err("the statement has an unterminated `{{`".into()),
                    }
                }
                if !arguments.contains(&name) {
                    return Err(format!("`{{{{{name}}}}}` names no declared argument"));
                }
                if !text.is_empty() {
                    parts.push(SqlPart::Text(std::mem::take(&mut text)));
                }
                parts.push(SqlPart::Argument(name));
            }
            '$' => return Err("`$` is reserved for the parameters of the statement".into()),
            ';' => return Err("the statement must be a single statement without `;`".into()),
            '\\' => return Err("backslashes are not allowed in the statement".into()),
            '-' if chars.peek() == Some(&'-') => {
                return Err("comments are not allowed in the statement".into());
            }
            '/' if chars.peek() == Some(&'*') => {
                return Err("comments are not allowed in the statement".into());
            }
            _ => text.push(c),
        }
    }
    if !text.is_empty() {
        parts.push(SqlPart::Text(text));
    }
    Ok(parts)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(names: &[&str]) -> Vec<String> {
        names.iter().map(|n| n.to_string()).collect()
    }

    #[test]
    fn markers_become_arguments_outside_quotes() {
        assert_eq!(
            parse(
                "SELECT * FROM a WHERE x = {{x}} AND y <> 'it''s {x}' AND z = {{x}}",
                &args(&["x"])
            )
            .unwrap(),
            vec![
                SqlPart::Text("SELECT * FROM a WHERE x = ".into()),
                SqlPart::Argument("x".into()),
                SqlPart::Text(" AND y <> 'it''s {x}' AND z = ".into()),
                SqlPart::Argument("x".into()),
            ]
        );
    }

    #[test]
    fn text_the_placeholder_rewriter_cannot_read_is_rejected() {
        for sql in [
            "SELECT $1",
            "SELECT $$a$$",
            "SELECT 1; SELECT 2",
            "SELECT 1 -- note",
            "SELECT /* note */ 1",
            "SELECT E'a\\'b'",
            "SELECT 'a",
            "SELECT '{{x}}'",
            "SELECT {{y}}",
            "SELECT {{x",
            "  ",
        ] {
            assert!(parse(sql, &args(&["x"])).is_err(), "{sql}");
        }
        assert!(parse("SELECT '$1' AS \"$2\"", &[]).is_ok());
        for names in [&["where"][..], &["a", "a"], &["1a"], &["__a"]] {
            assert!(parse("SELECT 1", &args(names)).is_err(), "{names:?}");
        }
    }
}
