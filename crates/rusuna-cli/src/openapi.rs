mod types;

use heck::{ToSnakeCase, ToUpperCamelCase};
use percent_encoding::percent_decode_str;
use serde_json::{Map, Value as Json};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write;
use std::path::Path;
use types::{Direction, Types, object_properties, schema_type};

const METHODS: [&str; 8] = [
    "get", "post", "put", "patch", "delete", "head", "options", "trace",
];

pub fn import_openapi(
    path: &Path,
    source: &str,
    roles: &[String],
    tags: &[String],
) -> Result<String, String> {
    let text = std::fs::read_to_string(path)
        .map_err(|error| format!("cannot read {}: {error}", path.display()))?;
    let json = || serde_json::from_str(&text).map_err(|error| format!("invalid JSON: {error}"));
    let yaml = || serde_norway::from_str(&text).map_err(|error| format!("invalid YAML: {error}"));
    let document: Json = match path.extension().and_then(|ext| ext.to_str()) {
        Some(ext) if ext.eq_ignore_ascii_case("json") => json(),
        Some(ext) if ext.eq_ignore_ascii_case("yaml") || ext.eq_ignore_ascii_case("yml") => yaml(),
        _ => json().or_else(|json_error| yaml().map_err(|error| format!("{json_error}; {error}"))),
    }?;
    let version = document["openapi"].as_str().unwrap_or_default();
    if !version.starts_with("3.0.") && !version.starts_with("3.1.") {
        return Err("expected an OpenAPI 3.0 or 3.1 document".into());
    }
    check_references(&document)?;
    let sdl = Importer {
        document: &document,
        types: Types::new(&document),
        names: BTreeSet::new(),
        skipped: Vec::new(),
        source,
        roles,
    }
    .render(tags)?;
    rusuna::Schema::from_sdl(&sdl).map_err(|error| format!("invalid generated SDL: {error}"))?;
    Ok(sdl)
}

fn check_references(value: &Json) -> Result<(), String> {
    match value {
        Json::Object(object) => {
            if let Some(reference) = object.get("$ref").and_then(Json::as_str)
                && !reference.starts_with('#')
            {
                return Err(format!("external reference {reference:?} is unsupported"));
            }
            for (key, value) in object {
                match key.as_str() {
                    "properties" | "patternProperties" | "schemas" | "$defs" | "examples"
                    | "responses" | "requestBodies" | "headers" | "links" | "pathItems"
                    | "securitySchemes" => {
                        for child in properties(value).map(|(_, child)| child) {
                            check_references(child)?;
                        }
                    }
                    "parameters" if value.is_object() => {
                        for child in properties(value).map(|(_, child)| child) {
                            check_references(child)?;
                        }
                    }
                    "example" | "default" | "enum" | "const" | "value" => {}
                    key if key.starts_with("x-") => {}
                    _ => check_references(value)?,
                }
            }
        }
        Json::Array(values) => {
            for value in values {
                check_references(value)?;
            }
        }
        _ => {}
    }
    Ok(())
}

fn resolve<'a>(document: &'a Json, mut value: &'a Json) -> Result<&'a Json, String> {
    let mut seen = BTreeSet::new();
    while let Some(reference) = value.get("$ref") {
        let reference = reference.as_str().ok_or("$ref must be a string")?;
        let fragment = reference
            .strip_prefix('#')
            .ok_or_else(|| format!("external reference {reference:?} is unsupported"))?;
        let pointer = percent_decode_str(fragment)
            .decode_utf8()
            .map_err(|_| format!("invalid reference {reference:?}"))?;
        if !seen.insert(reference) {
            return Err(format!("cyclic reference {reference:?}"));
        }
        value = document
            .pointer(&pointer)
            .ok_or_else(|| format!("unresolved reference {reference:?}"))?;
    }
    Ok(value)
}

fn properties(value: &Json) -> impl Iterator<Item = (&str, &Json)> {
    value
        .as_object()
        .into_iter()
        .flat_map(Map::iter)
        .map(|(name, value)| (name.as_str(), value))
}

fn required(schema: &Json, property: &str) -> bool {
    schema["required"]
        .as_array()
        .is_some_and(|required| required.iter().any(|name| name.as_str() == Some(property)))
}

fn parameter_schema(parameter: &Json) -> &Json {
    parameter
        .get("schema")
        .unwrap_or(&parameter["content"]["application/json"]["schema"])
}

fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && !name.starts_with("__")
        && name.bytes().enumerate().all(|(index, byte)| {
            byte == b'_' || byte.is_ascii_alphabetic() || index > 0 && byte.is_ascii_digit()
        })
}

fn ascii_words(name: &str) -> String {
    name.chars()
        .map(|ch| if ch.is_ascii_alphanumeric() { ch } else { '_' })
        .collect()
}

fn graphql_name(mut name: String, fallback: &str) -> String {
    if name.is_empty() {
        return fallback.to_owned();
    }
    if name.starts_with(|ch: char| ch.is_ascii_digit()) {
        name.insert(0, '_');
    }
    name
}

fn type_name(name: &str) -> String {
    graphql_name(ascii_words(name).to_upper_camel_case(), "Value")
}

fn field_name(name: &str) -> String {
    graphql_name(ascii_words(name).to_snake_case(), "field")
}

fn unique(names: &mut BTreeSet<String>, base: String) -> String {
    if names.insert(base.clone()) {
        return base;
    }
    (2..)
        .map(|number| format!("{base}_{number}"))
        .find(|name| names.insert(name.clone()))
        .unwrap()
}

fn quote(text: &str) -> String {
    serde_json::to_string(text).unwrap()
}

fn mapped_name(name: &str, names: &mut BTreeSet<String>) -> String {
    if valid_name(name) {
        name.to_owned()
    } else {
        unique(names, field_name(name))
    }
}

fn column(name: &str, key: &str) -> String {
    if name == key {
        String::new()
    } else {
        format!(" @column(name: {})", quote(key))
    }
}

fn marker(name: &str) -> String {
    format!("{{{{{name}}}}}")
}

fn request_path(path: &str, names: &BTreeMap<&str, String>) -> Result<String, String> {
    let mut result = String::new();
    let mut tail = path;
    while let Some((prefix, rest)) = tail.split_once('{') {
        let (name, rest) = rest.split_once('}').ok_or("unclosed path parameter")?;
        let argument = names
            .get(name)
            .ok_or_else(|| format!("undeclared path parameter {name:?}"))?;
        result.push_str(prefix);
        result.push_str(&marker(argument));
        tail = rest;
    }
    result.push_str(tail);
    Ok(result)
}

struct Importer<'a> {
    document: &'a Json,
    types: Types<'a>,
    names: BTreeSet<String>,
    skipped: Vec<String>,
    source: &'a str,
    roles: &'a [String],
}

#[derive(Default)]
struct Parameters<'a> {
    path: BTreeMap<&'a str, &'a Json>,
    query: BTreeMap<&'a str, &'a Json>,
}

impl<'a> Importer<'a> {
    fn render(mut self, tags: &[String]) -> Result<String, String> {
        let mut queries = String::new();
        let mut mutations = String::new();
        if let Some(paths) = self.document.get("paths") {
            let paths = paths.as_object().ok_or("OpenAPI paths must be an object")?;
            for (path, item) in paths.iter().filter(|(path, _)| path.starts_with('/')) {
                let item = resolve(self.document, item)?;
                for method in METHODS {
                    let Some(operation) = item.get(method) else {
                        continue;
                    };
                    if !tags.is_empty()
                        && !operation["tags"].as_array().is_some_and(|values| {
                            values.iter().any(|value| {
                                value
                                    .as_str()
                                    .is_some_and(|tag| tags.iter().any(|include| include == tag))
                            })
                        })
                    {
                        continue;
                    }
                    let context = format!("{} {path}", method.to_ascii_uppercase());
                    let field = self
                        .operation(method, path, item, operation, &context)
                        .map_err(|error| format!("{context}: {error}"))?;
                    if let Some(field) = field {
                        if method == "get" {
                            &mut queries
                        } else {
                            &mut mutations
                        }
                        .push_str(&field);
                    }
                }
            }
        }
        let mut output = String::new();
        for entry in self.skipped {
            for line in entry.split(['\r', '\n']) {
                writeln!(output, "# {line}").unwrap();
            }
        }
        if !output.is_empty() {
            output.push('\n');
        }
        if queries.is_empty() {
            output.push_str("type Query\n");
        } else {
            write!(output, "type Query {{\n{queries}}}\n").unwrap();
        }
        if !mutations.is_empty() {
            write!(output, "\ntype Mutation {{\n{mutations}}}\n").unwrap();
        }
        self.types.render(&mut output);
        Ok(output)
    }

    fn skip(&mut self, context: &str, reason: &str) -> Option<String> {
        self.skipped.push(format!("Skipped {context}: {reason}"));
        None
    }

    fn parameters(
        &mut self,
        item: &'a Json,
        operation: &'a Json,
        context: &str,
    ) -> Result<Parameters<'a>, String> {
        let mut effective = BTreeMap::new();
        for owner in [item, operation] {
            for parameter in owner["parameters"].as_array().into_iter().flatten() {
                let parameter = resolve(self.document, parameter)?;
                let name = parameter["name"]
                    .as_str()
                    .ok_or("parameter requires a name")?;
                let location = parameter["in"]
                    .as_str()
                    .ok_or("parameter requires a location")?;
                effective.insert((location, name), parameter);
            }
        }
        let mut parameters = Parameters::default();
        for ((location, name), parameter) in effective {
            match location {
                "path" => {
                    parameters.path.insert(name, parameter);
                }
                "query" => {
                    parameters.query.insert(name, parameter);
                }
                _ => self
                    .skipped
                    .push(format!("Skipped {context} {location} parameter {name:?}")),
            }
        }
        Ok(parameters)
    }

    fn operation(
        &mut self,
        method: &str,
        path: &str,
        item: &'a Json,
        operation: &'a Json,
        context: &str,
    ) -> Result<Option<String>, String> {
        if matches!(method, "head" | "options" | "trace") {
            return Ok(self.skip(context, "unsupported HTTP method"));
        }
        let parameters = self.parameters(item, operation, context)?;
        let body = if let Some(body) = operation.get("requestBody") {
            if method == "get" {
                return Ok(self.skip(context, "GET request bodies are unsupported"));
            }
            let body = resolve(self.document, body)?;
            let schema = resolve(
                self.document,
                &body["content"]["application/json"]["schema"],
            )?;
            if schema_type(schema) != Some("object") {
                return Ok(self.skip(context, "request body must be an application/json object"));
            }
            for (name, _) in properties(&schema["properties"]) {
                if parameters.path.contains_key(name) || parameters.query.contains_key(name) {
                    return Ok(self.skip(
                        context,
                        &format!(
                            "request body property {name:?} overlaps a path or query parameter"
                        ),
                    ));
                }
            }
            schema
        } else {
            &Json::Null
        };
        let name = operation["operationId"]
            .as_str()
            .map(field_name)
            .unwrap_or_else(|| {
                field_name(&format!(
                    "{method}_{}",
                    path.replace('{', "_by_").replace('}', "_")
                ))
            });
        let name = unique(&mut self.names, name);
        let stem = type_name(&name);
        let mut names = parameters
            .query
            .keys()
            .filter(|name| valid_name(name))
            .map(|name| (*name).to_owned())
            .collect::<BTreeSet<_>>();
        names.extend(
            properties(&body["properties"])
                .map(|(name, _)| name)
                .filter(|name| valid_name(name))
                .map(str::to_owned),
        );
        let mut path_names = BTreeMap::new();
        let mut arguments = Vec::new();
        for (&parameter, definition) in &parameters.path {
            let base = if valid_name(parameter) {
                parameter.to_owned()
            } else {
                field_name(parameter)
            };
            let argument = unique(&mut names, base);
            let ty = self.types.convert(
                parameter_schema(definition),
                &format!("{stem}{}", type_name(&argument)),
                Direction::Input,
                true,
            )?;
            arguments.push(format!("{argument}: {ty}"));
            path_names.insert(parameter, argument);
        }
        let path = request_path(path, &path_names)?;
        let mut query = Vec::new();
        for (&parameter, definition) in &parameters.query {
            let argument = mapped_name(parameter, &mut names);
            let ty = self.types.convert(
                parameter_schema(definition),
                &format!("{stem}{}", type_name(&argument)),
                Direction::Input,
                definition["required"].as_bool() == Some(true),
            )?;
            arguments.push(format!("{argument}: {ty}{}", column(&argument, parameter)));
            query.push(format!("{argument}: {}", quote(&marker(&argument))));
        }
        for (property, schema) in properties(&body["properties"]) {
            let argument = mapped_name(property, &mut names);
            let ty = self.types.convert(
                schema,
                &format!("{stem}{}", type_name(&argument)),
                Direction::Input,
                required(body, property),
            )?;
            arguments.push(format!("{argument}: {ty}{}", column(&argument, property)));
        }
        let (response, not_found) = self.response(operation, &stem, method)?;
        let mut directive = format!("source: {}, path: {}", quote(self.source), quote(&path));
        if method != "get" {
            write!(directive, ", method: {}", method.to_ascii_uppercase()).unwrap();
        }
        if !query.is_empty() {
            write!(directive, ", query: {{{}}}", query.join(", ")).unwrap();
        }
        if not_found {
            directive.push_str(", not_found: null");
        }
        if !self.roles.is_empty() {
            let roles = self
                .roles
                .iter()
                .map(|role| quote(role))
                .collect::<Vec<_>>()
                .join(", ");
            write!(directive, ", roles: [{roles}]").unwrap();
        }
        let arguments = if arguments.is_empty() {
            String::new()
        } else {
            format!("({})", arguments.join(", "))
        };
        Ok(Some(format!(
            "  {name}{arguments}: {response} @rest({directive})\n"
        )))
    }

    fn response(
        &mut self,
        operation: &'a Json,
        stem: &str,
        method: &str,
    ) -> Result<(String, bool), String> {
        let responses = &operation["responses"];
        let mut success = properties(responses)
            .filter(|(code, _)| {
                *code == "2XX"
                    || code.len() == 3
                        && code
                            .parse::<u16>()
                            .is_ok_and(|code| (200..300).contains(&code))
            })
            .collect::<Vec<_>>();
        success.sort_unstable_by_key(|(code, _)| *code);
        if matches!(success.as_slice(), [("204", _)]) {
            return Ok(("Boolean!".into(), false));
        }
        for &(_, response) in &success {
            let response = resolve(self.document, response)?;
            if let Some(json) = response["content"].get("application/json") {
                let schema = resolve(self.document, &json["schema"])?;
                let not_found = object_properties(schema).is_some()
                    && method == "get"
                    && responses.get("404").is_some();
                let ty = self.types.convert(
                    &json["schema"],
                    &format!("{stem}Response"),
                    Direction::Output,
                    !not_found,
                )?;
                return Ok((ty, not_found));
            }
        }
        Ok((
            self.types
                .convert(&Json::Null, "", Direction::Output, false)?,
            false,
        ))
    }
}
